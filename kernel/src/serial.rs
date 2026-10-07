use crate::cpu::{self, inb, outb};
use crate::sync::SpinLock;
use core::fmt::{self, Write};
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

const COM1: u16 = 0x3F8;
const DATA: u16 = COM1;
const IER: u16 = COM1 + 1;
const IIR: u16 = COM1 + 2;
const LSR: u16 = COM1 + 5;
const MSR: u16 = COM1 + 6;

const LSR_DATA: u8 = 0x01;
const LSR_EMPTY: u8 = 0x20;
const IER_RX: u8 = 0x01;
const IER_TX: u8 = 0x02;
const FIFO: usize = 16;

pub const TX_PAGES: u64 = 4;
const TX_SIZE: usize = TX_PAGES as usize * 4096 - 64;

// output from every core goes through this queue, which lives in memory all
// cores share. with interrupts on, core 0 empties it as the uart takes bytes.
pub struct Tx {
    buf: [u8; TX_SIZE],
    head: usize,
    len: usize,
    irq: bool,
}

const _: () = assert!(core::mem::size_of::<SpinLock<Tx>>() <= TX_PAGES as usize * 4096);

static TX: AtomicPtr<SpinLock<Tx>> = AtomicPtr::new(ptr::null_mut());

fn queue() -> Option<&'static SpinLock<Tx>> {
    unsafe { TX.load(Ordering::Acquire).as_ref() }
}

// 115200 baud 8n1, fifos on. OUT2 gates the uart's interrupt line.
pub fn init() {
    outb(IER, 0x00);
    outb(COM1 + 3, 0x80);
    outb(DATA, 0x01);
    outb(IER, 0x00);
    outb(COM1 + 3, 0x03);
    outb(IIR, 0xC7);
    outb(COM1 + 4, 0x0B);
}

fn putb(b: u8) {
    while inb(LSR) & LSR_EMPTY == 0 {}
    outb(DATA, b);
}

impl Tx {
    fn push(&mut self, b: u8) {
        while self.len == TX_SIZE {
            self.wait_empty();
            self.fill();
        }
        self.buf[(self.head + self.len) % TX_SIZE] = b;
        self.len += 1;
    }

    // an empty transmit fifo takes 16 bytes at once
    fn fill(&mut self) {
        if inb(LSR) & LSR_EMPTY == 0 {
            return;
        }
        for _ in 0..FIFO.min(self.len) {
            outb(DATA, self.buf[self.head]);
            self.head = (self.head + 1) % TX_SIZE;
            self.len -= 1;
        }
    }

    fn wait_empty(&self) {
        while inb(LSR) & LSR_EMPTY == 0 {
            core::hint::spin_loop();
        }
    }

    fn drain(&mut self) {
        while self.len > 0 {
            self.wait_empty();
            self.fill();
        }
    }
}

struct Out<'a>(Option<&'a mut Tx>);

impl Out<'_> {
    fn byte(&mut self, b: u8) {
        match &mut self.0 {
            Some(tx) => tx.push(b),
            None => putb(b),
        }
    }
}

impl Write for Out<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                self.byte(b'\r');
            }
            self.byte(b);
        }
        Ok(())
    }
}

// before the queue is shared only the first core prints, straight to the uart
fn output(f: impl FnOnce(&mut Out)) {
    match queue() {
        Some(lock) => {
            let mut tx = lock.lock();
            f(&mut Out(Some(&mut tx)));
            if tx.irq {
                tx.fill();
            } else {
                tx.drain();
            }
        }
        None => {
            let flags = cpu::push_cli();
            f(&mut Out(None));
            cpu::pop_flags(flags);
        }
    }
}

pub fn write_bytes(bytes: &[u8]) {
    output(|out| {
        for &b in bytes {
            if b == b'\n' {
                out.byte(b'\r');
            }
            out.byte(b);
        }
    });
}

pub fn _print(args: fmt::Arguments) {
    output(|out| {
        let _ = out.write_fmt(args);
    });
}

// before a panic or power off, so the last words get out
pub fn flush() {
    if let Some(lock) = queue() {
        lock.lock().drain();
    }
}

// the pages must be zeroed, which is an unlocked, empty queue
pub fn share_console(tx: *mut SpinLock<Tx>) {
    TX.store(tx, Ordering::Release);
}

pub fn console() -> *mut SpinLock<Tx> {
    TX.load(Ordering::Acquire)
}

pub fn enable_irqs() {
    if let Some(lock) = queue() {
        let mut tx = lock.lock();
        tx.irq = true;
        outb(IER, IER_RX | IER_TX);
        tx.fill();
    }
}

// one irq for rx and tx. IIR says which, reading that register clears it.
pub fn on_irq(mut rx: impl FnMut(u8)) {
    for _ in 0..16 {
        let iir = inb(IIR);
        if iir & 1 != 0 {
            return;
        }
        match (iir >> 1) & 7 {
            1 => {
                if let Some(lock) = queue() {
                    lock.lock().fill();
                }
            }
            2 | 6 => {
                while inb(LSR) & LSR_DATA != 0 {
                    rx(inb(DATA));
                }
            }
            3 => {
                inb(LSR);
            }
            _ => {
                inb(MSR);
            }
        }
    }
}

#[allow(unused_macros)]
macro_rules! print {
    ($($arg:tt)*) => ($crate::serial::_print(::core::format_args!($($arg)*)));
}

macro_rules! println {
    () => ($crate::serial::_print(::core::format_args!("\n")));
    ($($arg:tt)*) => ($crate::serial::_print(::core::format_args!("{}\n", ::core::format_args!($($arg)*))));
}
