use crate::elf;
use crate::frame;
use crate::idt::Frame;
use crate::paging::{self, phys_to_virt, USER, WRITABLE};
use crate::smp;
use crate::sync::SpinLock;
use crate::task;
use alloc::boxed::Box;
use core::sync::atomic::{AtomicU64, Ordering};

pub const SYS_PRINT: u64 = 0;
pub const SYS_EXIT: u64 = 1;
pub const SYS_YIELD: u64 = 2;
pub const SYS_TIME: u64 = 3;
pub const SYS_SUBMIT: u64 = 4;
pub const SYS_POLL: u64 = 5;
pub const SYS_WAIT: u64 = 6;

const STACK_PAGES: u64 = 32;
const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000;
const PAGE: u64 = 0x1000;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const MAX_REQS: usize = 16;
const MSG_MAX: usize = 64;
const LINE_MAX: usize = 256;

struct Launch {
    entry: u64,
    user_sp: u64,
}

#[derive(Clone, Copy)]
struct Req {
    id: u64,
    owner: usize,
    waiter: bool,
    ready_at: u64,
    len: usize,
    data: [u8; MSG_MAX],
}

// whole lines only, so jobs on different cores never interleave mid-line
struct Line {
    buf: [u8; LINE_MAX],
    len: usize,
}

impl Line {
    fn flush(&mut self) {
        let text = &self.buf[..self.len];
        let valid = match core::str::from_utf8(text) {
            Ok(s) => s,
            Err(e) => unsafe { core::str::from_utf8_unchecked(&text[..e.valid_up_to()]) },
        };
        println!("[cpu{}] {}", smp::core(), valid);
        self.len = 0;
    }
}

static LINE: SpinLock<Line> = SpinLock::new(Line {
    buf: [0; LINE_MAX],
    len: 0,
});
// fake requests: each one completes a tick after submit.
// only touched from syscalls and the timer, both with interrupts off.
static mut REQS: [Option<Req>; MAX_REQS] = [None; MAX_REQS];
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn reqs() -> &'static mut [Option<Req>; MAX_REQS] {
    unsafe { &mut *core::ptr::addr_of_mut!(REQS) }
}

pub fn spawn(image: &[u8]) -> bool {
    let Some((pml4, launch)) = load(image) else {
        return false;
    };
    let arg = Box::into_raw(Box::new(launch));
    if task::spawn_in(run, arg as u64, pml4).is_none() {
        drop(unsafe { Box::from_raw(arg) });
        paging::free_user_space(pml4);
        return false;
    }
    true
}

fn run(arg: u64) {
    let launch = unsafe { Box::from_raw(arg as *mut Launch) };
    unsafe { enter(launch.entry, launch.user_sp) }
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

// segments must stay below the stack
fn load(image: &[u8]) -> Option<(u64, Launch)> {
    let stack_bottom = USER_STACK_TOP - STACK_PAGES * PAGE;
    let fits = |s: &elf::Segment| s.vaddr.checked_add(s.memsz).is_some_and(|end| end <= stack_bottom);
    if !elf::is_elf(image) || !elf::segments(image).all(|s| fits(&s)) {
        return None;
    }
    let pml4 = frame::alloc_zeroed()?;
    inherit_kernel_half(pml4);
    if fill(pml4, image).is_none() {
        paging::free_user_space(pml4);
        return None;
    }
    Some((pml4, Launch { entry: elf::entry(image), user_sp: USER_STACK_TOP }))
}

fn fill(pml4: u64, image: &[u8]) -> Option<()> {
    for seg in elf::segments(image) {
        let mut v = seg.vaddr & !(PAGE - 1);
        let vend = (seg.vaddr + seg.memsz + PAGE - 1) & !(PAGE - 1);
        while v < vend {
            // a page shared with a previous segment keeps its frame
            let f = match paging::translate(pml4, v) {
                Some(pa) => pa & !(PAGE - 1),
                None => {
                    let f = frame::alloc_zeroed()?;
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
        let f = frame::alloc_zeroed()?;
        unsafe {
            paging::map_4k(pml4, sp, f, USER | WRITABLE);
        }
    }
    Some(())
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

pub fn fault(f: &Frame, name: &str) -> ! {
    println!(
        "cpu {}: task {} killed by {} at {:#x} (cr2 {:#x})",
        smp::core(),
        task::current(),
        name,
        f.rip,
        crate::cpu::read_cr2()
    );
    task::exit_current()
}

fn user_bytes<'a>(ptr: u64, len: u64, max: usize) -> Option<&'a [u8]> {
    if len as usize > max || !paging::user_range_ok(paging::current(), ptr, len) {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) })
}

pub fn syscall(f: &mut Frame) {
    match f.rax {
        SYS_PRINT => sys_print(f),
        SYS_EXIT => task::exit_current(),
        SYS_YIELD => task::schedule(),
        SYS_TIME => f.rax = task::ticks(),
        SYS_SUBMIT => f.rax = submit(f.rdi, f.rsi),
        SYS_POLL => f.rax = poll_now(f.rdi, f.rsi),
        SYS_WAIT => f.rax = wait(f.rdi, f.rsi),
        _ => f.rax = u64::MAX,
    }
}

fn sys_print(f: &mut Frame) {
    let Some(bytes) = user_bytes(f.rdi, f.rsi, LINE_MAX) else {
        f.rax = u64::MAX;
        return;
    };
    if core::str::from_utf8(bytes).is_err() {
        f.rax = u64::MAX;
        return;
    }
    let mut line = LINE.lock();
    for &b in bytes {
        if b == b'\n' || line.len == LINE_MAX {
            line.flush();
        }
        if b != b'\n' {
            let n = line.len;
            line.buf[n] = b;
            line.len += 1;
        }
    }
    f.rax = 0;
}

fn submit(ptr: u64, len: u64) -> u64 {
    let Some(src) = user_bytes(ptr, len, MSG_MAX).filter(|s| !s.is_empty()) else {
        return u64::MAX;
    };
    let mut data = [0u8; MSG_MAX];
    data[..src.len()].copy_from_slice(src);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    for slot in reqs().iter_mut() {
        if slot.is_none() {
            *slot = Some(Req {
                id,
                owner: task::current(),
                waiter: false,
                ready_at: task::ticks() + 1,
                len: src.len(),
                data,
            });
            return id;
        }
    }
    u64::MAX
}

// only the task that submitted a request may collect it
fn complete(id: u64, out: u64) -> Option<usize> {
    let me = task::current();
    for slot in reqs().iter_mut() {
        if let Some(req) = slot {
            if req.id == id && req.owner == me && req.ready_at <= task::ticks() {
                if !paging::user_range_ok(paging::current(), out, req.len as u64) {
                    return None;
                }
                let len = req.len;
                let data = req.data;
                *slot = None;
                unsafe {
                    core::ptr::copy_nonoverlapping(data.as_ptr(), out as *mut u8, len);
                }
                return Some(len);
            }
        }
    }
    None
}

fn poll_now(id: u64, out: u64) -> u64 {
    u64::from(complete(id, out).is_some())
}

fn wait(id: u64, out: u64) -> u64 {
    let me = task::current();
    loop {
        if complete(id, out).is_some() {
            return 1;
        }
        let now = task::ticks();
        let mut seen = false;
        let mut armed = false;
        for slot in reqs().iter_mut() {
            if let Some(req) = slot {
                if req.id == id && req.owner == me {
                    seen = true;
                    if req.ready_at > now {
                        req.waiter = true;
                        armed = true;
                    }
                    break;
                }
            }
        }
        if !seen || !armed {
            return u64::MAX;
        }
        task::block_current();
    }
}

pub fn on_tick() {
    let now = task::ticks();
    for slot in reqs().iter_mut() {
        if let Some(req) = slot {
            if req.waiter && req.ready_at <= now {
                req.waiter = false;
                task::wake(req.owner);
            }
        }
    }
}
