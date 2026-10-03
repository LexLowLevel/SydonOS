#![no_std]
#![no_main]

use sydon_rt::{println, sys};

sydon_rt::main!(main);

// the first page is never mapped
const UNMAPPED: usize = 0x1000;

fn main() -> u64 {
    println!("fault: writing to {:#x} on cpu {}", UNMAPPED, sys::info().0);
    unsafe { core::ptr::write_volatile(UNMAPPED as *mut u64, 1) };
    println!("fault: still alive, that is wrong");
    1
}
