#![no_std]
#![no_main]

use sydon_rt::{println, sys, Vec};

sydon_rt::main!(main);

fn main() -> u64 {
    let (core, pid) = sys::info();
    println!("hello from cpu {}, pid {}", core, pid);
    let squares: Vec<u64> = (1..=8).map(|x| x * x).collect();
    println!("squares on the heap: {:?}", squares);
    0
}
