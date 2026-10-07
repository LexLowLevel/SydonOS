#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use sydon_rt::rpc::{Payload, Pending, Service};
use sydon_rt::{format, println, sys, Vec};

sydon_rt::main!(main);

const GET: u16 = 1;
const PUT: u16 = 2;
const MAX_SHARDS: usize = 16;
const KEYS: u64 = 10_000;
const OPS: u64 = 200_000;
// requests in flight at once, so the round trip is not all that is measured
const WINDOW: usize = 16;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn main() -> u64 {
    let shards: Vec<Service> = (0..MAX_SHARDS).map_while(|i| Service::lookup(&format!("kv{}", i)).ok()).collect();
    if shards.is_empty() {
        println!("kvbench: no kv shards, try: spawn kv");
        return 1;
    }
    let (core, pid) = sys::info();
    let shard = |key: u64| &shards[(key % shards.len() as u64) as usize];
    let mut window: VecDeque<Pending> = VecDeque::with_capacity(WINDOW);
    let mut failed = 0;
    let mut issue = |svc: &Service, op: u16, key: u64, value: u64, window: &mut VecDeque<Pending>| {
        if window.len() == WINDOW {
            failed += window.pop_front().unwrap().wait().is_err() as u64;
        }
        match svc.submit(op, Payload::new().word(0, key).word(1, value).bytes()) {
            Ok(p) => window.push_back(p),
            Err(_) => failed += 1,
        }
    };

    for key in 0..KEYS {
        issue(shard(key), PUT, key, key * 3, &mut window);
    }
    let mut rng = Rng(pid * 0x9E37_79B9 + 1);
    let t0 = sys::time_ns();
    for _ in 0..OPS {
        let r = rng.next();
        let key = r % KEYS;
        let op = if r % 10 == 0 { PUT } else { GET };
        issue(shard(key), op, key, r, &mut window);
    }
    while let Some(p) = window.pop_front() {
        failed += p.wait().is_err() as u64;
    }
    let ns = (sys::time_ns() - t0).max(1);
    println!(
        "kvbench: cpu {} did {} ops on {} shards in {} ms: {} k ops/s, {} failed",
        core,
        OPS,
        shards.len(),
        ns / 1_000_000,
        OPS * 1_000_000 / ns,
        failed
    );
    (failed > 0) as u64
}
