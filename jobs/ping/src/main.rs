#![no_std]
#![no_main]

use sydon_rt::rpc::{Error, Pending, Service};
use sydon_rt::{println, sys, Vec};

sydon_rt::main!(main);
sydon_rt::manifest!(needs: "echo");

const ROUNDS: u64 = 1000;
const IN_FLIGHT: usize = 8;
const IDLE_ROUNDS: u64 = 100;

fn main() -> u64 {
    let echo = match Service::lookup("echo") {
        Ok(s) => s,
        Err(e) => {
            println!("ping: no echo service ({}), try: spawn echod", e);
            return 1;
        }
    };
    let me = sys::info().0;
    let msg = b"are you there";

    let (mut min, mut max, mut total) = (u64::MAX, 0, 0);
    for _ in 0..ROUNDS {
        let t0 = sys::time_ns();
        match echo.call(1, msg) {
            Ok(p) if p.bytes() == msg => {}
            Ok(_) => {
                println!("ping: echo sent back something else");
                return 1;
            }
            Err(e) => {
                println!("ping: {}", e);
                return 1;
            }
        }
        let dt = sys::time_ns() - t0;
        min = min.min(dt);
        max = max.max(dt);
        total += dt;
    }
    println!(
        "ping: {} calls cpu {} -> cpu {}: min {} us, avg {} us, max {} us",
        ROUNDS,
        me,
        echo.core(),
        micros(min),
        micros(total / ROUNDS),
        micros(max)
    );

    // long enough apart that the server's core has gone to sleep each time
    let (mut min, mut total) = (u64::MAX, 0);
    for _ in 0..IDLE_ROUNDS {
        sys::sleep_ns(2_000_000);
        let t0 = sys::time_ns();
        if let Err(e) = echo.call(1, msg) {
            println!("ping: {}", e);
            return 1;
        }
        let dt = sys::time_ns() - t0;
        min = min.min(dt);
        total += dt;
    }
    println!(
        "ping: {} calls after 2 ms idle cpu {} -> cpu {}: min {} us, avg {} us",
        IDLE_ROUNDS,
        me,
        echo.core(),
        micros(min),
        micros(total / IDLE_ROUNDS)
    );

    let t0 = sys::time_ns();
    let mut pending: Vec<Pending> = (0..IN_FLIGHT).filter_map(|_| echo.submit(1, msg).ok()).collect();
    let mut answered = 0;
    while !pending.is_empty() {
        pending.retain(|p| match p.poll() {
            Some(r) => {
                answered += r.is_ok() as usize;
                false
            }
            None => true,
        });
        if !pending.is_empty() {
            sys::yield_now();
        }
    }
    println!("ping: {} requests in flight at once, {} answered in {} us", IN_FLIGHT, answered, (sys::time_ns() - t0) / 1000);

    let t0 = sys::time_ns();
    match echo.submit_timeout(2, msg, 200_000_000).and_then(|p| p.wait()) {
        Err(Error::Timeout) => println!("ping: unanswered request timed out after {} ms", (sys::time_ns() - t0) / 1_000_000),
        other => {
            println!("ping: expected a timeout, got {:?}", other.map(|p| p.len));
            return 1;
        }
    }
    0
}

fn micros(ns: u64) -> sydon_rt::String {
    sydon_rt::format!("{}.{}", ns / 1000, ns % 1000 / 100)
}
