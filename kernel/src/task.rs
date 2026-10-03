use crate::cpu;
use crate::gdt;
use crate::{frame, paging};
use core::arch::global_asm;
use core::ptr;
use core::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};

pub type Entry = fn(u64);

const STACK_SIZE: usize = 32 * 1024;
// each slot leaves the space below its stack unmapped, so an overflow faults
// instead of writing over whatever lies next to it
const STACKS: u64 = 0xFFFF_FFFF_0000_0000;
const STACK_STRIDE: u64 = 2 * STACK_SIZE as u64;
const PAGE: u64 = 0x1000;
pub const MAX_TASKS: usize = 32;
pub const IDLE: usize = usize::MAX;

#[derive(Clone, Copy, PartialEq)]
#[repr(u8)]
enum State {
    Empty,
    Ready,
    Running,
    Blocked,
    Done,
}

impl State {
    fn from_u8(v: u8) -> State {
        match v {
            1 => State::Ready,
            2 => State::Running,
            3 => State::Blocked,
            4 => State::Done,
            _ => State::Empty,
        }
    }
}

struct Task {
    rsp: u64,
    stack: *mut u8,
    arg: u64,
    entry: Entry,
    state: AtomicU8,
    system: bool,
    cr3: u64,
    wake_at: u64,
    kill: bool,
    boost: bool,
}

fn dummy(_: u64) {}

const TASK_EMPTY: Task = Task {
    rsp: 0,
    stack: ptr::null_mut(),
    arg: 0,
    entry: dummy,
    state: AtomicU8::new(State::Empty as u8),
    system: false,
    cr3: 0,
    wake_at: 0,
    kill: false,
    boost: false,
};

static mut TABLE: [Task; MAX_TASKS] = [TASK_EMPTY; MAX_TASKS];
static IDLE_RSP: AtomicU64 = AtomicU64::new(0);
static CURRENT: AtomicUsize = AtomicUsize::new(IDLE);
static TICKS: AtomicU64 = AtomicU64::new(0);
static KERNEL_CR3: AtomicU64 = AtomicU64::new(0);

extern "C" {
    static __stack_top: u8;
}

global_asm!(
    ".global task_switch",
    "task_switch:",
    "push rbp",
    "push rbx",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rdi], rsp",
    "mov rsp, rsi",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "ret",
);

extern "C" {
    fn task_switch(old: *mut u64, new: u64);
}

unsafe fn task_at(i: usize) -> *mut Task {
    (ptr::addr_of_mut!(TABLE) as *mut Task).add(i)
}

unsafe fn get_state(t: *mut Task) -> State {
    State::from_u8((*t).state.load(Ordering::Acquire))
}

unsafe fn set_state(t: *mut Task, s: State) {
    (*t).state.store(s as u8, Ordering::Release);
}

pub fn init(kernel_cr3: u64) {
    KERNEL_CR3.store(kernel_cr3, Ordering::Relaxed);
}

extern "C" fn task_root() {
    cpu::sti();
    let cur = CURRENT.load(Ordering::Relaxed);
    let (entry, arg) = unsafe { ((*task_at(cur)).entry, (*task_at(cur)).arg) };
    entry(arg);
    exit_current();
}

// a task cannot free the stack it runs on, so this happens later
unsafe fn reap(t: *mut Task) {
    free_stack((*t).stack as u64);
    let kernel = KERNEL_CR3.load(Ordering::Relaxed);
    if (*t).cr3 != kernel {
        // a kernel task may still run on this space
        if crate::paging::current() == (*t).cr3 {
            cpu::write_cr3(kernel);
        }
        crate::paging::free_user_space((*t).cr3);
    }
    set_state(t, State::Empty);
}

pub fn spawn(entry: Entry, arg: u64, system: bool) -> Option<usize> {
    spawn_in(entry, arg, system, KERNEL_CR3.load(Ordering::Relaxed))
}

pub fn spawn_in(entry: Entry, arg: u64, system: bool, cr3: u64) -> Option<usize> {
    let flags = cpu::push_cli();
    let Some(i) = take_slot() else {
        cpu::pop_flags(flags);
        return None;
    };
    let Some(stack) = map_stack(i) else {
        cpu::pop_flags(flags);
        return None;
    };
    let stack = stack as *mut u8;
    unsafe {
        let t = task_at(i);
        // fake a task_switch frame: six zeroed registers, then task_root to ret into
        let rsp = stack as u64 + STACK_SIZE as u64 - 64;
        let p = rsp as *mut u64;
        for k in 0..6 {
            p.add(k).write(0);
        }
        let root: extern "C" fn() = task_root;
        p.add(6).write(root as usize as u64);

        (*t).rsp = rsp;
        (*t).stack = stack;
        (*t).arg = arg;
        (*t).entry = entry;
        (*t).system = system;
        (*t).cr3 = cr3;
        (*t).wake_at = 0;
        (*t).kill = false;
        (*t).boost = false;
        set_state(t, State::Ready);
    }
    cpu::pop_flags(flags);
    Some(i)
}

fn take_slot() -> Option<usize> {
    let cur = CURRENT.load(Ordering::Relaxed);
    let i = (0..MAX_TASKS).find(|&i| unsafe {
        match get_state(task_at(i)) {
            State::Empty => true,
            State::Done => i != cur,
            _ => false,
        }
    })?;
    unsafe {
        let t = task_at(i);
        if get_state(t) == State::Done {
            reap(t);
        }
    }
    Some(i)
}

pub fn reap_finished() {
    let flags = cpu::push_cli();
    let cur = CURRENT.load(Ordering::Relaxed);
    for i in (0..MAX_TASKS).filter(|&i| i != cur) {
        unsafe {
            let t = task_at(i);
            if get_state(t) == State::Done {
                reap(t);
            }
        }
    }
    cpu::pop_flags(flags);
}

pub fn others_ready() -> bool {
    let cur = CURRENT.load(Ordering::Relaxed);
    (0..MAX_TASKS).any(|i| i != cur && unsafe { get_state(task_at(i)) } == State::Ready)
}

pub fn current() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

pub fn wake(i: usize) {
    unsafe {
        let t = task_at(i);
        if get_state(t) == State::Blocked {
            (*t).wake_at = 0;
            (*t).boost = true;
            set_state(t, State::Ready);
        }
    }
}

// takes effect on the next return to ring 3
pub fn kill(i: usize) {
    let flags = cpu::push_cli();
    unsafe {
        let t = task_at(i);
        if matches!(get_state(t), State::Ready | State::Running | State::Blocked) {
            (*t).kill = true;
            wake(i);
        }
    }
    cpu::pop_flags(flags);
}

pub fn killed() -> bool {
    let cur = CURRENT.load(Ordering::Relaxed);
    cur != IDLE && unsafe { (*task_at(cur)).kill }
}

pub fn block_current() {
    let cur = CURRENT.load(Ordering::Relaxed);
    if cur != IDLE {
        unsafe {
            set_state(task_at(cur), State::Blocked);
        }
    }
    schedule();
}

pub fn sleep_until(tick: u64) {
    let flags = cpu::push_cli();
    let cur = CURRENT.load(Ordering::Relaxed);
    if cur != IDLE && tick > ticks() {
        unsafe {
            (*task_at(cur)).wake_at = tick;
        }
        block_current();
    }
    cpu::pop_flags(flags);
}

pub fn exit_current() -> ! {
    cpu::cli();
    let cur = CURRENT.load(Ordering::Relaxed);
    if cur != IDLE {
        unsafe {
            set_state(task_at(cur), State::Done);
        }
    }
    loop {
        schedule();
    }
}

fn map_stack(i: usize) -> Option<u64> {
    let base = STACKS + i as u64 * STACK_STRIDE + STACK_STRIDE - STACK_SIZE as u64;
    let kernel = KERNEL_CR3.load(Ordering::Relaxed);
    for page in (0..STACK_SIZE as u64).step_by(PAGE as usize) {
        let Some(f) = frame::alloc_zeroed() else {
            free_stack(base);
            return None;
        };
        unsafe { paging::map_4k(kernel, base + page, f, paging::WRITABLE) };
    }
    Some(base)
}

fn free_stack(base: u64) {
    let kernel = KERNEL_CR3.load(Ordering::Relaxed);
    for page in (0..STACK_SIZE as u64).step_by(PAGE as usize) {
        if let Some(f) = paging::translate(kernel, base + page) {
            unsafe { paging::unmap_4k(kernel, base + page) };
            frame::free(f, 1);
        }
    }
}

pub fn overflowed(addr: u64) -> Option<usize> {
    let i = addr.checked_sub(STACKS)? / STACK_STRIDE;
    let guard = addr.checked_sub(STACKS)? % STACK_STRIDE < STACK_STRIDE - STACK_SIZE as u64;
    (i < MAX_TASKS as u64 && guard).then_some(i as usize)
}

fn kstack_top(i: usize) -> u64 {
    if i == IDLE {
        ptr::addr_of!(__stack_top) as u64
    } else {
        unsafe { (*task_at(i)).stack as u64 + STACK_SIZE as u64 }
    }
}

fn pick(cur: usize) -> Option<usize> {
    let start = if cur == IDLE { 0 } else { cur + 1 };
    let ready = |i: usize| unsafe { get_state(task_at(i)) == State::Ready };
    let order = (0..MAX_TASKS).map(|k| (start + k) % MAX_TASKS);
    order
        .clone()
        .find(|&i| ready(i) && unsafe { (*task_at(i)).system })
        .or_else(|| order.clone().find(|&i| ready(i) && unsafe { (*task_at(i)).boost }))
        .or_else(|| order.clone().find(|&i| ready(i)))
}

pub fn schedule() {
    let flags = cpu::push_cli();
    let cur = CURRENT.load(Ordering::Relaxed);
    let cur_state = if cur == IDLE {
        State::Running
    } else {
        unsafe { get_state(task_at(cur)) }
    };
    let cur_inactive = cur_state == State::Done || cur_state == State::Blocked;
    let cur_system = cur != IDLE && unsafe { (*task_at(cur)).system };

    let next = pick(cur);
    // a running system task is not preempted by a job
    let next = match next {
        Some(n) if cur_system && !cur_inactive && !unsafe { (*task_at(n)).system } => None,
        n => n,
    };

    if next.is_none() && !cur_inactive {
        cpu::pop_flags(flags);
        return;
    }

    let old: *mut u64 = if cur == IDLE {
        IDLE_RSP.as_ptr()
    } else {
        unsafe { ptr::addr_of_mut!((*task_at(cur)).rsp) }
    };
    let target = next.unwrap_or(IDLE);

    unsafe {
        if cur != IDLE && !cur_inactive {
            set_state(task_at(cur), State::Ready);
        }
        if let Some(n) = next {
            set_state(task_at(n), State::Running);
        }
    }
    gdt::set_rsp0(kstack_top(target));
    // kernel tasks and idle keep the current space: same kernel half, no tlb flush
    let kernel = KERNEL_CR3.load(Ordering::Relaxed);
    let cr3 = if target == IDLE { kernel } else { unsafe { (*task_at(target)).cr3 } };
    if cr3 != kernel && cr3 != crate::paging::current() {
        cpu::write_cr3(cr3);
    }
    CURRENT.store(target, Ordering::Relaxed);

    let new_rsp: u64 = match next {
        Some(n) => unsafe { (*task_at(n)).rsp },
        None => IDLE_RSP.load(Ordering::Relaxed),
    };
    unsafe {
        task_switch(old, new_rsp);
    }
    cpu::pop_flags(flags);
}

pub fn on_timer() {
    let now = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
    let cur = CURRENT.load(Ordering::Relaxed);
    if cur != IDLE {
        unsafe { (*task_at(cur)).boost = false };
    }
    for i in 0..MAX_TASKS {
        unsafe {
            let t = task_at(i);
            if (*t).wake_at != 0 && (*t).wake_at <= now && get_state(t) == State::Blocked {
                (*t).wake_at = 0;
                set_state(t, State::Ready);
            }
        }
    }
    schedule();
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}
