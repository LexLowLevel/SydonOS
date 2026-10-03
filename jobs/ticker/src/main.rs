#![no_std]
#![no_main]

use sydon_rt::{println, sys};

sydon_rt::main!(main);

fn main() -> u64 {
    let core = sys::info().0;
    for i in 1..=5 {
        sys::sleep_ns(500_000_000);
        println!("ticker: tick {} on cpu {}", i, core);
    }
    0
}
