#![no_std]
#![no_main]

use sydon_rt::rpc::{Server, Service};
use sydon_rt::{io, println, sys};

sydon_rt::main!(main);

const YIELDS: u64 = 1_000_000;

// alone on a core every yield comes straight back. "yielder pair" twice on
// one core: the two start together and every yield switches to the other.
fn main() -> u64 {
    let pair = io::args().get(1).is_some_and(|a| a == "pair");
    if pair {
        // whoever registers first waits for the second, so both begin at once
        match Server::register("yielder") {
            Ok(server) => match server.recv() {
                Ok(req) => req.reply(&[]),
                Err(e) => println!("yielder: {}", e),
            },
            Err(_) => {
                if let Err(e) = Service::lookup("yielder").and_then(|s| s.call(0, &[])) {
                    println!("yielder: {}", e);
                }
            }
        }
    }
    let core = sys::info().0;
    let t0 = sys::time_ns();
    for _ in 0..YIELDS {
        sys::yield_now();
    }
    let ns = sys::time_ns() - t0;
    if pair {
        println!("yielder: cpu {} switched {} times with its partner in {} us: {} ns per switch", core, 2 * YIELDS, ns / 1000, ns / (2 * YIELDS));
    } else {
        println!("yielder: cpu {} did {} yields alone in {} us: {} ns each", core, YIELDS, ns / 1000, ns / YIELDS);
    }
    0
}
