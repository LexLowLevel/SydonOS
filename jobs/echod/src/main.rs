#![no_std]
#![no_main]

use sydon_rt::rpc::Server;
use sydon_rt::{println, sys};

sydon_rt::main!(main);
sydon_rt::manifest!(restart);

fn main() -> u64 {
    let server = match Server::register("echo") {
        Ok(s) => s,
        Err(e) => {
            println!("echod: cannot register: {}", e);
            return 1;
        }
    };
    println!("echod: serving on cpu {}", sys::info().0);
    let mut next = server.recv();
    loop {
        next = match next {
            // op 2 is never answered, so callers can test their timeouts
            Ok(req) if req.op == 2 => server.recv(),
            Ok(req) => {
                let data = req.payload;
                server.reply_recv(req, data.bytes())
            }
            Err(e) => {
                println!("echod: {}", e);
                return 1;
            }
        };
    }
}
