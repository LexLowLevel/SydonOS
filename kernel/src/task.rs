use crate::cpu;
use crate::gdt;
use alloc::alloc::alloc_zeroed;
use core::alloc::Layout;
use core::arch::global_asm;
use core::ptr;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub type Entry = fn(u64);

const STACK_SIZE: usize = 32 * 1024;
const MAX_TASKS: usize = 8;
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

#[derive(Clone, Copy)]
struct Task {
    rsp: u64,
    stack: *mut u8,
    arg: u64,
    entry: Entry,
    state: State,
}

fn dummy(_: u64) {}

const TASK_EMPTY: Task = Task {
    rsp: 0,
    stack: ptr::null_mut(),
    arg: 0,
    entry: dummy,
    state: State::Empty,
};

static mut TABLE: [Task; MAX_TASKS] = [TASK_EMPTY; MAX_TASKS];
static mut IDLE_RSP: u64 = 0;
static CURRENT: AtomicUsize = AtomicUsize::new(IDLE);
static TICKS: AtomicU64 = AtomicU64::new(0);

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
    ptr::read_volatile(ptr::addr_of_mut!((*t).state))
}

unsafe fn set_state(t: *mut Task, s: State) {
    ptr::write_volatile(ptr::addr_of_mut!((*t).state), s);
}

extern "C" fn task_root() {
    cpu::sti();
    let cur = CURRENT.load(Ordering::Relaxed);
    let (entry, arg) = unsafe { ((*task_at(cur)).entry, (*task_at(cur)).arg) };
    entry(arg);
    exit_current();
}

pub fn spawn(entry: Entry, arg: u64) {
    let layout = Layout::from_size_align(STACK_SIZE, 16).unwrap();
    let stack = unsafe { alloc_zeroed(layout) };
    assert!(!stack.is_null(), "task: stack alloc failed");

    let rsp = stack as u64 + STACK_SIZE as u64 - 64;
    unsafe {
        let p = rsp as *mut u64;
        for i in 0..6 {
            p.add(i).write(0);
        }
        let root: extern "C" fn() = task_root;
        p.add(6).write(root as usize as u64);

        for i in 0..MAX_TASKS {
            let t = task_at(i);
            if get_state(t) == State::Empty {
                (*t).rsp = rsp;
                (*t).stack = stack;
                (*t).arg = arg;
                (*t).entry = entry;
                set_state(t, State::Ready);
                return;
            }
        }
    }
    panic!("task: table full");
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
    let cur = CURRENT.load(Ordering::Relaxed);
    if cur != IDLE {
        unsafe {
            set_state(task_at(cur), State::Done);
        }
    }
    println!("kernel: task {} exited", cur);
    loop {
        schedule();
    }
}

fn kstack_top(i: usize) -> u64 {
    if i == IDLE {
        unsafe { ptr::addr_of!(__stack_top) as u64 }
    } else {
        unsafe { (*task_at(i)).stack as u64 + STACK_SIZE as u64 }
    }
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

    let mut next = IDLE;
    let mut found = false;
    unsafe {
        let start = if cur == IDLE { 0 } else { cur + 1 };
        for k in 0..MAX_TASKS {
            let i = (start + k) % MAX_TASKS;
            if get_state(task_at(i)) == State::Ready {
                next = i;
                found = true;
                break;
            }
        }
    }

    if !found && !cur_inactive {
        cpu::pop_flags(flags);
        return;
    }

    let old: *mut u64 = if cur == IDLE {
        unsafe { ptr::addr_of_mut!(IDLE_RSP) }
    } else {
        unsafe { ptr::addr_of_mut!((*task_at(cur)).rsp) }
    };
    let target = if found { next } else { IDLE };

    unsafe {
        if cur != IDLE && !cur_inactive && get_state(task_at(cur)) == State::Running {
            set_state(task_at(cur), State::Ready);
        }
        if found {
            set_state(task_at(next), State::Running);
        }
    }
    gdt::set_rsp0(kstack_top(target));
    CURRENT.store(target, Ordering::Relaxed);

    let new_rsp: u64 = if found {
        unsafe { (*task_at(next)).rsp }
    } else {
        unsafe { ptr::read_volatile(ptr::addr_of!(IDLE_RSP)) }
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
