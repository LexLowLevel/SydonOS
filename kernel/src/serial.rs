use core::arch::asm;
use core::fmt::{self, Write};

const COM1: u16 = 0x3F8;

unsafe fn outb(port: u16, val: u8) {
    asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}

unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    val
}

pub fn init() {
    unsafe {
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x80);
        outb(COM1 + 0, 0x01);
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03);
        outb(COM1 + 2, 0xC7);
        outb(COM1 + 4, 0x0B);
    }
}

fn putb(b: u8) {
    unsafe {
        while inb(COM1 + 5) & 0x20 == 0 {}
        outb(COM1, b);
    }
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

pub fn _print(args: fmt::Arguments) {
    let flags = crate::cpu::push_cli();
    let _ = Writer.write_fmt(args);
    crate::cpu::pop_flags(flags);
}

macro_rules! print {
    ($($arg:tt)*) => ($crate::serial::_print(::core::format_args!($($arg)*)));
}

macro_rules! println {
    () => ($crate::serial::_print(::core::format_args!("\n")));
    ($($arg:tt)*) => ($crate::serial::_print(::core::format_args!("{}\n", ::core::format_args!($($arg)*))));
}
