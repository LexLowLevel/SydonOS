use crate::cpu;
use crate::gdt;
use alloc::alloc::{alloc_zeroed, dealloc};
use core::alloc::Layout;
use core::arch::global_asm;
use core::ptr;
use core::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};

pub type Entry = fn(u64);

const STACK_SIZE: usize = 32 * 1024;
pub const MAX_TASKS: usize = 32;
// the boot stack; it runs whenever no task is ready
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
    cr3: u64,
}

fn dummy(_: u64) {}

const TASK_EMPTY: Task = Task {
    rsp: 0,
    stack: ptr::null_mut(),
    arg: 0,
    entry: dummy,
    state: AtomicU8::new(State::Empty as u8),
    cr3: 0,
};

static mut TABLE: [Task; MAX_TASKS] = [TASK_EMPTY; MAX_TASKS];
static IDLE_RSP: AtomicU64 = AtomicU64::new(0);
static CURRENT: AtomicUsize = AtomicUsize::new(IDLE);
static TICKS: AtomicU64 = AtomicU64::new(0);
static KERNEL_CR3: AtomicU64 = AtomicU64::new(0);

extern "C" {
    static __stack_top: u8;
}

// saves callee-saved registers on the old stack and stores rsp in *old, then
// loads the new rsp and pops. ret goes wherever that task last switched out.
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

// a finished task still owns its kernel stack and maybe an address space.
// both are freed here, when the slot is reused, because a task cannot free
// the stack it is running on.
unsafe fn reap(t: *mut Task) {
    let layout = Layout::from_size_align(STACK_SIZE, 16).unwrap();
    dealloc((*t).stack, layout);
    let kernel = KERNEL_CR3.load(Ordering::Relaxed);
    if (*t).cr3 != kernel {
        // kernel tasks borrow whatever space was loaded, which may be this one
        if crate::paging::current() == (*t).cr3 {
            cpu::write_cr3(kernel);
        }
        crate::paging::free_user_space((*t).cr3);
    }
    set_state(t, State::Empty);
}

pub fn spawn(entry: Entry, arg: u64) -> Option<usize> {
    spawn_in(entry, arg, KERNEL_CR3.load(Ordering::Relaxed))
}

pub fn spawn_in(entry: Entry, arg: u64, cr3: u64) -> Option<usize> {
    let flags = cpu::push_cli();
    let Some(i) = take_slot() else {
        cpu::pop_flags(flags);
        return None;
    };
    let stack = unsafe { alloc_zeroed(Layout::from_size_align(STACK_SIZE, 16).unwrap()) };
    if stack.is_null() {
        cpu::pop_flags(flags);
        return None;
    }
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
        (*t).cr3 = cr3;
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

pub fn current() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

pub fn wake(i: usize) {
    unsafe {
        let t = task_at(i);
        if get_state(t) == State::Blocked {
            set_state(t, State::Ready);
        }
    }
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

fn kstack_top(i: usize) -> u64 {
    if i == IDLE {
        ptr::addr_of!(__stack_top) as u64
    } else {
        unsafe { (*task_at(i)).stack as u64 + STACK_SIZE as u64 }
    }
}

// round robin, starting after the current task
fn pick(cur: usize) -> Option<usize> {
    let start = if cur == IDLE { 0 } else { cur + 1 };
    (0..MAX_TASKS)
        .map(|k| (start + k) % MAX_TASKS)
        .find(|&i| unsafe { get_state(task_at(i)) == State::Ready })
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
    let next = pick(cur);

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
    // the stack the cpu lands on when this task traps out of ring 3
    gdt::set_rsp0(kstack_top(target));
    // the kernel half is the same in every space, so kernel tasks and idle
    // just keep the current one and skip the tlb flush
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
    TICKS.fetch_add(1, Ordering::Relaxed);
    schedule();
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn all_done() -> bool {
    let mut any = false;
    unsafe {
        for i in 0..MAX_TASKS {
            match get_state(task_at(i)) {
                State::Empty => {}
                State::Done => any = true,
                _ => return false,
            }
        }
    }
    any
}
