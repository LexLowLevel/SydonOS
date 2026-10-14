#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use sydon_rt::fs::{self, CHUNK};
use sydon_rt::rpc::Pending;
use sydon_rt::{println, sys, vec};

sydon_rt::main!(main);
sydon_rt::manifest!(class: Io, needs: "fs");

const PATH: &str = "/data/big";
const WINDOW: usize = 8;
const RANDOM_READS: u64 = 20_000;
const RANDOM_WINDOW: usize = 16;

fn main() -> u64 {
    let file = match fs::open(PATH) {
        Ok(f) => f,
        Err(e) => {
            println!("fsbench: {}: {}", PATH, e);
            return 1;
        }
    };
    let core = sys::info().0;
    let mut chunk = vec![0u8; CHUNK];

    let t0 = sys::time_ns();
    let mut inflight: VecDeque<Pending> = VecDeque::new();
    let (mut next, mut total) = (0u64, 0u64);
    while next < file.size || !inflight.is_empty() {
        while inflight.len() < WINDOW && next < file.size {
            match file.read_submit(next, CHUNK) {
                Ok(p) => inflight.push_back(p),
                Err(e) => {
                    println!("fsbench: read at {}: {}", next, e);
                    return 1;
                }
            }
            next += CHUNK as u64;
        }
        match inflight.pop_front().unwrap().wait() {
            Ok(reply) => total += fs::take(reply, &mut chunk) as u64,
            Err(e) => {
                println!("fsbench: {}", e);
                return 1;
            }
        }
    }
    let ns = (sys::time_ns() - t0).max(1);
    println!("fsbench: cpu {} read {} MiB sequentially in {} ms: {} MiB/s", core, total >> 20, ns / 1_000_000, (total >> 20) * 1_000_000_000 / ns);

    let mut small = [0u8; 4096];
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let blocks = file.size / 4096;
    let t0 = sys::time_ns();
    let mut inflight: VecDeque<Pending> = VecDeque::new();
    let mut ok = 0u64;
    for _ in 0..RANDOM_READS {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        if inflight.len() == RANDOM_WINDOW {
            ok += inflight.pop_front().unwrap().wait().map(|r| fs::take(r, &mut small) == 4096).unwrap_or(false) as u64;
        }
        if let Ok(p) = file.read_submit(rng % blocks * 4096, 4096) {
            inflight.push_back(p);
        }
    }
    while let Some(p) = inflight.pop_front() {
        ok += p.wait().map(|r| fs::take(r, &mut small) == 4096).unwrap_or(false) as u64;
    }
    let ns = (sys::time_ns() - t0).max(1);
    println!("fsbench: cpu {} did {} random 4K reads: {} k reads/s, {} failed", core, RANDOM_READS, ok * 1_000_000 / ns, RANDOM_READS - ok);
    0
}
