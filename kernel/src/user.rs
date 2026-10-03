use crate::director;
use crate::elf;
use crate::frame;
use crate::idt::Frame;
use crate::paging::{self, phys_to_virt, USER, WRITABLE};
use crate::rpc::{self, Answer, Body, Taken, BUSY, INVALID, NOT_FOUND, NO_MEMORY, OK, PAYLOAD};
use crate::sync::IrqCell;
use crate::task::{self, MAX_TASKS};
use crate::{clock, jobs, smp};
use alloc::boxed::Box;

const SYS_PRINT: u64 = 0;
const SYS_EXIT: u64 = 1;
const SYS_YIELD: u64 = 2;
const SYS_TIME: u64 = 3;
const SYS_SUBMIT: u64 = 4;
const SYS_POLL: u64 = 5;
const SYS_WAIT: u64 = 6;
const SYS_LOOKUP: u64 = 7;
const SYS_REGISTER: u64 = 8;
const SYS_RECV: u64 = 9;
const SYS_REPLY: u64 = 10;
const SYS_INFO: u64 = 11;
const SYS_GROW: u64 = 12;
const SYS_SLEEP: u64 = 13;

const PENDING: u64 = u64::MAX;
const BAD_ID: u64 = u64::MAX - 1;

const PRINT_MAX: usize = 256;
const STACK_PAGES: u64 = 32;
const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000;
const HEAP_BASE: u64 = 0x0000_0000_1000_0000;
const HEAP_MAX: u64 = 64 << 20;
const PAGE: u64 = 0x1000;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const CRASH_CODE: u64 = 255;
pub const KILLED_CODE: u64 = 130;

struct Launch {
    entry: u64,
    user_sp: u64,
}

#[derive(Clone, Copy)]
struct Proc {
    pid: u64,
    heap_top: u64,
}

static PROCS: IrqCell<[Option<Proc>; MAX_TASKS]> = IrqCell::new([None; MAX_TASKS]);

fn err(status: u8) -> u64 {
    u64::MAX - status as u64
}

pub fn load_job(pid: u64, name: &[u8]) -> Answer {
    let Some(b) = jobs::builtin(name) else {
        return Answer::Reply(NOT_FOUND, Body::EMPTY);
    };
    match spawn_image(pid, b.image) {
        Ok(()) => Answer::Reply(OK, Body::EMPTY),
        Err(status) => Answer::Reply(status, Body::EMPTY),
    }
}

fn spawn_image(pid: u64, image: &[u8]) -> Result<(), u8> {
    let (pml4, launch) = load(image)?;
    let arg = Box::into_raw(Box::new(launch));
    // the job must not run before its Proc exists
    let flags = crate::cpu::push_cli();
    let tid = task::spawn_in(run, arg as u64, false, pml4);
    if let Some(tid) = tid {
        PROCS.with(|p| p[tid] = Some(Proc { pid, heap_top: HEAP_BASE }));
    }
    crate::cpu::pop_flags(flags);
    if tid.is_none() {
        drop(unsafe { Box::from_raw(arg) });
        paging::free_user_space(pml4);
        return Err(BUSY);
    }
    Ok(())
}

fn run(arg: u64) {
    let l = unsafe { Box::from_raw(arg as *mut Launch) };
    unsafe { enter(l.entry, l.user_sp) }
}

// the top half points at the same lower tables, so kernel mappings are shared
fn inherit_kernel_half(pml4: u64) {
    let cur = crate::cpu::read_cr3() & ADDR_MASK;
    unsafe {
        for i in 256..512 {
            let k = (phys_to_virt(cur) as *mut u64).add(i).read_volatile();
            (phys_to_virt(pml4) as *mut u64).add(i).write_volatile(k);
        }
    }
}

// segments must stay below the heap, which sits below the stack
fn load(image: &[u8]) -> Result<(u64, Launch), u8> {
    let fits = |s: &elf::Segment| s.vaddr.checked_add(s.memsz).is_some_and(|end| end <= HEAP_BASE);
    if !elf::is_elf(image) || !elf::segments(image).all(|s| fits(&s)) {
        return Err(INVALID);
    }
    let pml4 = frame::alloc_zeroed().ok_or(NO_MEMORY)?;
    inherit_kernel_half(pml4);
    match fill(pml4, image) {
        Ok(()) => Ok((pml4, Launch { entry: elf::entry(image), user_sp: USER_STACK_TOP })),
        Err(status) => {
            paging::free_user_space(pml4);
            Err(status)
        }
    }
}

fn fill(pml4: u64, image: &[u8]) -> Result<(), u8> {
    for seg in elf::segments(image) {
        let mut v = seg.vaddr & !(PAGE - 1);
        let vend = (seg.vaddr + seg.memsz + PAGE - 1) & !(PAGE - 1);
        while v < vend {
            // a page shared with a previous segment keeps its frame
            let f = match paging::translate(pml4, v) {
                Some(pa) => pa & !(PAGE - 1),
                None => {
                    let f = frame::alloc_zeroed().ok_or(NO_MEMORY)?;
                    unsafe {
                        paging::map_4k(pml4, v, f, USER | WRITABLE);
                    }
                    f
                }
            };
            let lo = seg.vaddr.max(v);
            let hi = (seg.vaddr + seg.filesz).min(v + PAGE);
            if hi > lo {
                unsafe {
                    let dst = (phys_to_virt(f) as *mut u8).add((lo - v) as usize);
                    let src = image.as_ptr().add((seg.offset + (lo - seg.vaddr)) as usize);
                    dst.copy_from_nonoverlapping(src, (hi - lo) as usize);
                }
            }
            v += PAGE;
        }
    }

    let mut sp = USER_STACK_TOP;
    while sp > USER_STACK_TOP - STACK_PAGES * PAGE {
        sp -= PAGE;
        let f = frame::alloc_zeroed().ok_or(NO_MEMORY)?;
        unsafe {
            paging::map_4k(pml4, sp, f, USER | WRITABLE);
        }
    }
    Ok(())
}

// iretq pops rip, cs, rflags, rsp, ss. rflags 0x202 turns interrupts on in ring 3.
unsafe fn enter(entry: u64, user_sp: u64) -> ! {
    core::arch::asm!(
        "push {udata}",
        "push {sp}",
        "push 0x202",
        "push {ucode}",
        "push {entry}",
        "iretq",
        udata = in(reg) crate::gdt::USER_DS as u64,
        ucode = in(reg) crate::gdt::USER_CS as u64,
        sp = in(reg) user_sp,
        entry = in(reg) entry,
        options(noreturn),
    )
}

fn current_pid() -> u64 {
    PROCS.with(|p| p[task::current()].map_or(0, |p| p.pid))
}

pub fn kill(pid: u64) -> bool {
    let tid = PROCS.with(|p| p.iter().position(|p| p.is_some_and(|p| p.pid == pid)));
    if let Some(tid) = tid {
        task::kill(tid);
    }
    tid.is_some()
}

pub fn exit_job(code: u64) -> ! {
    let tid = task::current();
    let pid = PROCS.with(|p| p[tid].take()).map_or(0, |p| p.pid);
    let director = rpc::handle(0, rpc::SVC_DIRECTOR);
    for h in rpc::drop_task(tid) {
        rpc::oneway(director, director::OP_UNREGISTER, Body::words(&[h]));
    }
    rpc::oneway(director, director::OP_EXITED, Body::words(&[pid, code]));
    task::exit_current()
}

pub fn fault(f: &Frame, name: &str) -> ! {
    println!(
        "cpu {}: pid {} killed by {} at {:#x} (cr2 {:#x})",
        smp::core(),
        current_pid(),
        name,
        f.rip,
        crate::cpu::read_cr2()
    );
    exit_job(CRASH_CODE)
}

fn user_ok(ptr: u64, len: u64) -> bool {
    paging::user_range_ok(paging::current(), ptr, len)
}

fn user_bytes<'a>(ptr: u64, len: u64, max: usize) -> Option<&'a [u8]> {
    if len as usize > max || !user_ok(ptr, len) {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) })
}

fn copy_out(out: u64, body: &Body) -> bool {
    if !user_ok(out, PAYLOAD as u64) {
        return false;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(body.data.as_ptr(), out as *mut u8, PAYLOAD);
    }
    true
}

fn done(out: u64, status: u8, body: &Body) -> u64 {
    if !copy_out(out, body) {
        return err(INVALID);
    }
    (status as u64) << 8 | body.len as u64
}

fn timeout(ns: u64) -> u64 {
    match ns {
        0 => rpc::DEFAULT_TIMEOUT,
        u64::MAX => rpc::NEVER,
        ns => ns.div_ceil(clock::TICK_US * 1000).max(1),
    }
}

pub fn syscall(f: &mut Frame) {
    if matches!(f.rax, SYS_SUBMIT..=SYS_WAIT | SYS_RECV | SYS_REPLY) {
        rpc::syscall_active();
    }
    let (a, b, c, d) = (f.rdi, f.rsi, f.rdx, f.r10);
    let me = task::current();
    f.rax = match f.rax {
        SYS_PRINT => match user_bytes(a, b, PRINT_MAX) {
            Some(bytes) => {
                print!("{}", alloc::string::String::from_utf8_lossy(bytes));
                0
            }
            None => err(INVALID),
        },
        SYS_EXIT => exit_job(a),
        SYS_YIELD => {
            task::schedule();
            0
        }
        SYS_TIME => clock::now_ns(),
        SYS_SUBMIT => match user_bytes(c, d, PAYLOAD) {
            Some(bytes) => match rpc::submit_user(a, b as u16, Body::bytes(bytes), rpc::Then::Task(me), timeout(f.r8)) {
                Ok(id) => id,
                Err(status) => err(status),
            },
            None => err(INVALID),
        },
        SYS_POLL => match rpc::take(a, me) {
            Taken::Done(p) => done(b, p.status, &p.body),
            Taken::Pending => PENDING,
            Taken::Unknown => BAD_ID,
        },
        SYS_WAIT => match rpc::wait(a) {
            Some(p) => done(b, p.status, &p.body),
            None => BAD_ID,
        },
        SYS_LOOKUP => match user_bytes(a, b, 32) {
            Some(name) => rpc::lookup(name).unwrap_or_else(err),
            None => err(INVALID),
        },
        SYS_REGISTER => match user_bytes(a, b, 32) {
            Some(name) => rpc::register_user(me, name).unwrap_or_else(err),
            None => err(INVALID),
        },
        SYS_RECV => {
            if rpc::handle_core(a) != smp::core() || !user_ok(b, PAYLOAD as u64) {
                err(INVALID)
            } else {
                match rpc::svc_recv(a as u16, me) {
                    Ok((token, op, body)) => {
                        copy_out(b, &body);
                        f.rdx = (op as u64) << 32 | body.len as u64;
                        token
                    }
                    Err(status) => err(status),
                }
            }
        }
        SYS_REPLY => match user_bytes(c, d, PAYLOAD) {
            Some(bytes) => match rpc::svc_reply(me, a, b as u8, Body::bytes(bytes)) {
                Ok(()) => 0,
                Err(status) => err(status),
            },
            None => err(INVALID),
        },
        SYS_INFO => {
            f.rdx = current_pid();
            smp::core() as u64
        }
        SYS_GROW => grow(a),
        SYS_SLEEP => {
            let ticks = a.div_ceil(clock::TICK_US * 1000);
            task::sleep_until(task::ticks() + ticks);
            0
        }
        _ => err(INVALID),
    };
}

fn grow(pages: u64) -> u64 {
    let tid = task::current();
    let Some(mut p) = PROCS.with(|p| p[tid]) else {
        return err(INVALID);
    };
    let start = p.heap_top;
    let end = pages.checked_mul(PAGE).and_then(|n| n.checked_add(start));
    if pages == 0 || end.is_none_or(|e| e > HEAP_BASE + HEAP_MAX) {
        return err(INVALID);
    }
    let pml4 = paging::current();
    for i in 0..pages {
        let Some(f) = frame::alloc_zeroed() else {
            return err(NO_MEMORY);
        };
        unsafe { paging::map_4k(pml4, start + i * PAGE, f, USER | WRITABLE) };
        p.heap_top += PAGE;
        PROCS.with(|procs| procs[tid] = Some(p));
    }
    start
}
