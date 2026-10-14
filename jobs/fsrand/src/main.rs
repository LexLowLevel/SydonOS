#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use sydon_rt::fs;
use sydon_rt::rpc::Pending;
use sydon_rt::{println, sys};

sydon_rt::main!(main);
sydon_rt::manifest!(class: IoLatency, needs: "fs");

const PATH: &str = "/data/big";
const READS: u64 = 20_000;
const WINDOW: usize = 16;

fn main() -> u64 {
    let file = match fs::open(PATH) {
        Ok(f) => f,
        Err(e) => {
            println!("fsrand: {}: {}", PATH, e);
            return 1;
        }
    };
    let mut block = [0u8; 4096];
    let mut rng = 0x2545_F491_4F6C_DD1Du64;
    let blocks = file.size / 4096;
    let t0 = sys::time_ns();
    let mut inflight: VecDeque<Pending> = VecDeque::new();
    let mut ok = 0u64;
    for _ in 0..READS {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        if inflight.len() == WINDOW {
            ok += inflight.pop_front().unwrap().wait().map(|r| fs::take(r, &mut block) == 4096).unwrap_or(false) as u64;
        }
        if let Ok(p) = file.read_submit(rng % blocks * 4096, 4096) {
            inflight.push_back(p);
        }
    }
    while let Some(p) = inflight.pop_front() {
        ok += p.wait().map(|r| fs::take(r, &mut block) == 4096).unwrap_or(false) as u64;
    }
    let ns = (sys::time_ns() - t0).max(1);
    println!("fsrand: cpu {} did {} random 4K reads: {} k reads/s, {} failed", sys::info().0, READS, ok * 1_000_000 / ns, READS - ok);
    0
}
