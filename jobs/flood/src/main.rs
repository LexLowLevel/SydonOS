#![no_std]
#![no_main]

use sydon_rt::rpc::{Error, Server, Service};
use sydon_rt::{format, println, sys};

sydon_rt::main!(main);

// echod never answers op 2
const SILENT: u16 = 2;

fn main() -> u64 {
    let echo = match Service::lookup("echo") {
        Ok(s) => s,
        Err(e) => {
            println!("flood: no echo service ({})", e);
            return 1;
        }
    };
    let (mut sent, mut busy) = (0, 0);
    for _ in 0..100_000 {
        match echo.submit_timeout(SILENT, b"x", sys::FOREVER) {
            Ok(_) => sent += 1,
            Err(Error::Busy) => busy += 1,
            Err(e) => {
                println!("flood: {}", e);
                return 1;
            }
        }
    }
    println!("flood: {} requests out, {} refused as busy", sent, busy);
    let registered = (0..20).filter(|i| Server::register(&format!("flood{}", i)).is_ok()).count();
    println!("flood: registered {} of 20 services", registered);
    0
}
