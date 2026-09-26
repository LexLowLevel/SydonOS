use crate::cpu::{self, inb, outb};
use crate::sync::SpinLock;
use core::fmt::{self, Write};
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

const COM1: u16 = 0x3F8;
const LSR: u16 = COM1 + 5;
const LSR_EMPTY: u8 = 0x20;

// lives in memory shared by every core; each kernel image only holds a pointer to it
static CONSOLE: AtomicPtr<SpinLock<()>> = AtomicPtr::new(ptr::null_mut());

// 115200 baud 8n1, fifos on
pub fn init() {
    outb(COM1 + 1, 0x00);
    outb(COM1 + 3, 0x80);
    outb(COM1, 0x01);
    outb(COM1 + 1, 0x00);
    outb(COM1 + 3, 0x03);
    outb(COM1 + 2, 0xC7);
    outb(COM1 + 4, 0x0B);
}

fn putb(b: u8) {
    while inb(LSR) & LSR_EMPTY == 0 {}
    outb(COM1, b);
}

pub struct Writer;

impl Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                putb(b'\r');
            }
            putb(b);
        }
        Ok(())
    }
}

pub fn share_console(lock: *mut SpinLock<()>) {
    CONSOLE.store(lock, Ordering::Release);
}

pub fn console() -> *mut SpinLock<()> {
    CONSOLE.load(Ordering::Acquire)
}

pub fn _print(args: fmt::Arguments) {
    let flags = cpu::push_cli();
    let lock = unsafe { CONSOLE.load(Ordering::Acquire).as_ref() };
    let guard = lock.map(|l| l.lock());
    let _ = Writer.write_fmt(args);
    drop(guard);
    cpu::pop_flags(flags);
}

#[allow(unused_macros)]
macro_rules! print {
    ($($arg:tt)*) => ($crate::serial::_print(::core::format_args!($($arg)*)));
}

macro_rules! println {
    () => ($crate::serial::_print(::core::format_args!("\n")));
    ($($arg:tt)*) => ($crate::serial::_print(::core::format_args!("{}\n", ::core::format_args!($($arg)*))));
}
