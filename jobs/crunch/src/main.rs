#![no_std]
#![no_main]

use sydon_rt::{println, sys};

sydon_rt::main!(main);
sydon_rt::manifest!(class: Compute);

const LIMIT: u64 = 3_000_000;

fn main() -> u64 {
    let t0 = sys::time_ns();
    let mut primes = 0;
    for n in 2..LIMIT {
        if (2..).take_while(|d| d * d <= n).all(|d| n % d != 0) {
            primes += 1;
        }
    }
    println!("crunch: cpu {} found {} primes in {} ms", sys::info().0, primes, (sys::time_ns() - t0) / 1_000_000);
    0
}
