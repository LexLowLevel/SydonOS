#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use sydon_rt::rpc::{Error, Payload, Server};
use sydon_rt::{format, println, sys};

sydon_rt::main!(main);
sydon_rt::manifest!(restart);

const GET: u16 = 1;
const PUT: u16 = 2;
const DEL: u16 = 3;
const MAX_SHARDS: usize = 16;

fn main() -> u64 {
    let Some((server, shard)) = (0..MAX_SHARDS).find_map(|i| Server::register(&format!("kv{}", i)).ok().map(|s| (s, i))) else {
        println!("kv: all {} shard names are taken", MAX_SHARDS);
        return 1;
    };
    println!("kv: shard {} on cpu {}", shard, sys::info().0);
    let mut map = BTreeMap::new();
    let mut next = server.recv();
    loop {
        let req = match next {
            Ok(req) => req,
            Err(e) => {
                println!("kv: {}", e);
                return 1;
            }
        };
        let key = req.payload.get_word(0);
        next = match req.op {
            GET => match map.get(&key) {
                Some(&v) => server.reply_recv(req, Payload::new().word(0, v).bytes()),
                None => server.fail_recv(req, Error::NotFound),
            },
            PUT => {
                map.insert(key, req.payload.get_word(1));
                server.reply_recv(req, &[])
            }
            DEL => {
                map.remove(&key);
                server.reply_recv(req, &[])
            }
            _ => server.fail_recv(req, Error::Invalid),
        };
    }
}
