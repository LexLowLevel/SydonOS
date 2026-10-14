#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use sydon_rt::fs::{self, CHUNK};
use sydon_rt::rpc::{Error, Pending};
use sydon_rt::{format, println, sys, vec};

sydon_rt::main!(main);
sydon_rt::manifest!(class: Io, needs: "fs");

const FILE: &str = "/data/fswrite";
const DIR: &str = "/data/fswrite.d";
const MIB: u64 = 48;
const WINDOW: usize = 8;
const SMALL: usize = 200;

fn main() -> u64 {
    match run() {
        Ok(()) => 0,
        Err(e) => {
            println!("fswrite: {}", e);
            1
        }
    }
}

fn run() -> Result<(), Error> {
    let core = sys::info().0;
    let file = fs::create(FILE)?;
    file.truncate(0)?;
    let data = vec![0x5Au8; CHUNK];
    let t0 = sys::time_ns();
    let mut inflight: VecDeque<Pending> = VecDeque::new();
    let mut at = 0;
    while at < MIB << 20 || !inflight.is_empty() {
        while inflight.len() < WINDOW && at < MIB << 20 {
            inflight.push_back(file.write_submit(at, &data)?);
            at += CHUNK as u64;
        }
        if let Some(p) = inflight.pop_front() {
            p.wait()?;
        }
    }
    let ms = (sys::time_ns() - t0) / 1_000_000;
    println!("fswrite: cpu {} wrote {} MiB in {} ms: {} MiB/s", core, MIB, ms, MIB * 1000 / ms.max(1));
    fs::remove(FILE)?;

    let _ = fs::mkdir(DIR);
    let t0 = sys::time_ns();
    for i in 0..SMALL {
        fs::create(&format!("{}/f{}", DIR, i))?.write_at(0, b"a small file\n")?;
    }
    let made = (sys::time_ns() - t0) / 1000 / SMALL as u64;
    let t0 = sys::time_ns();
    for i in 0..SMALL {
        fs::remove(&format!("{}/f{}", DIR, i))?;
    }
    let gone = (sys::time_ns() - t0) / 1000 / SMALL as u64;
    fs::remove(DIR)?;
    println!("fswrite: {} small files: create and write {} us each, remove {} us each", SMALL, made, gone);
    Ok(())
}
