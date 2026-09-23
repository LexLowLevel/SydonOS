use crate::frame;
use crate::idt::Frame;
use crate::paging::{self, phys_to_virt, USER, WRITABLE};
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

const USER_LIMIT: u64 = 0x0000_8000_0000_0000;
const STACK_PAGES: u64 = 32;
const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000;
const PAGE: u64 = 0x1000;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const MAX_REQS: usize = 16;
const MSG_MAX: usize = 64;

struct Launch {
    pml4: u64,
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

static mut REQS: [Option<Req>; MAX_REQS] = [None; MAX_REQS];
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn rd64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn reqs() -> &'static mut [Option<Req>; MAX_REQS] {
    unsafe { &mut *core::ptr::addr_of_mut!(REQS) }
}

pub fn spawn(image: &[u8]) {
    let launch = Box::new(load(image));
    task::spawn(run, Box::into_raw(launch) as u64);
}

fn run(arg: u64) {
    let launch = unsafe { Box::from_raw(arg as *mut Launch) };
    unsafe {
        paging::activate(launch.pml4);
        enter(launch.entry, launch.user_sp);
    }
}

fn inherit_kernel_half(pml4: u64) {
    let cur = crate::cpu::read_cr3() & ADDR_MASK;
    unsafe {
        let k = (phys_to_virt(cur) as *mut u64).add(511).read_volatile();
        (phys_to_virt(pml4) as *mut u64).add(511).write_volatile(k);
    }
}

fn load(image: &[u8]) -> Launch {
    assert!(image.len() >= 0x40 && &image[0..4] == b"\x7fELF", "user: not ELF");
    let entry = rd64(image, 0x18);
    let phoff = rd64(image, 0x20) as usize;
    let phentsize = rd16(image, 0x36) as usize;
    let phnum = rd16(image, 0x38) as usize;

    let pml4 = frame::alloc_zeroed().expect("user: no pml4");
    inherit_kernel_half(pml4);

    for i in 0..phnum {
        let p = phoff + i * phentsize;
        if rd32(image, p) != 1 {
            continue;
        }
        let p_offset = rd64(image, p + 0x08);
        let p_vaddr = rd64(image, p + 0x10);
        let p_filesz = rd64(image, p + 0x20);
        let p_memsz = rd64(image, p + 0x28);
        assert!(p_vaddr < USER_LIMIT, "user: segment above user limit");
        let mut v = p_vaddr & !(PAGE - 1);
        let vend = (p_vaddr + p_memsz + PAGE - 1) & !(PAGE - 1);
        while v < vend {
            // a page shared with a previous segment keeps its frame
            let f = match paging::translate(pml4, v) {
                Some(pa) => pa & !(PAGE - 1),
                None => {
                    let f = frame::alloc_zeroed().expect("user: out of frames");
                    unsafe {
                        paging::map_4k(pml4, v, f, USER | WRITABLE);
                    }
                    f
                }
            };
            let lo = p_vaddr.max(v);
            let hi = (p_vaddr + p_filesz).min(v + PAGE);
            if hi > lo {
                unsafe {
                    let dst = (phys_to_virt(f) as *mut u8).add((lo - v) as usize);
                    let src = image.as_ptr().add((p_offset + (lo - p_vaddr)) as usize);
                    dst.copy_from_nonoverlapping(src, (hi - lo) as usize);
                }
            }
            v += PAGE;
        }
    }

    let mut sp = USER_STACK_TOP;
    while sp > USER_STACK_TOP - STACK_PAGES * PAGE {
        sp -= PAGE;
        let f = frame::alloc_zeroed().expect("user: out of frames");
        unsafe {
            paging::map_4k(pml4, sp, f, USER | WRITABLE);
        }
    }

    Launch {
        pml4,
        entry,
        user_sp: USER_STACK_TOP,
    }
}

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
    let len = f.rsi as usize;
    if f.rdi >= USER_LIMIT || len > 256 {
        f.rax = u64::MAX;
        return;
    }
    let bytes = unsafe { core::slice::from_raw_parts(f.rdi as *const u8, len) };
    match core::str::from_utf8(bytes) {
        Ok(s) => {
            print!("{}", s);
            f.rax = 0;
        }
        Err(_) => f.rax = u64::MAX,
    }
}

fn submit(ptr: u64, len: u64) -> u64 {
    if ptr >= USER_LIMIT || len == 0 || len as usize > MSG_MAX {
        return u64::MAX;
    }
    let src = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let mut data = [0u8; MSG_MAX];
    data[..len as usize].copy_from_slice(src);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    for slot in reqs().iter_mut() {
        if slot.is_none() {
            *slot = Some(Req {
                id,
                owner: task::current(),
                waiter: false,
                ready_at: task::ticks() + 1,
                len: len as usize,
                data,
            });
            return id;
        }
    }
    u64::MAX
}

fn complete(id: u64, out: u64) -> Option<usize> {
    if out >= USER_LIMIT {
        return None;
    }
    for slot in reqs().iter_mut() {
        if let Some(req) = slot {
            if req.id == id && req.ready_at <= task::ticks() {
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
    loop {
        if complete(id, out).is_some() {
            return 1;
        }
        let now = task::ticks();
        let mut seen = false;
        let mut armed = false;
        for slot in reqs().iter_mut() {
            if let Some(req) = slot {
                if req.id == id {
                    seen = true;
                    if req.ready_at > now {
                        req.waiter = true;
                        req.owner = task::current();
                        armed = true;
                    }
                    break;
                }
            }
        }
        if !seen {
            return u64::MAX;
        }
        if armed {
            task::block_current();
        }
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
