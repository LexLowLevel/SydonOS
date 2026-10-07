#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use sydon_rt::rpc::{Error, Payload, Server};
use sydon_rt::{format, println, sys};

sydon_rt::main!(main);

const GET: u16 = 1;
const PUT: u16 = 2;
const DEL: u16 = 3;
const MAX_SHARDS: usize = 16;

// shard i registers as "kv<i>", and keys go to shard key % shards
fn main() -> u64 {
    let Some((server, shard)) = (0..MAX_SHARDS).find_map(|i| Server::register(&format!("kv{}", i)).ok().map(|s| (s, i))) else {
        println!("kv: all {} shard names are taken", MAX_SHARDS);
        return 1;
    };
    println!("kv: shard {} on cpu {}", shard, sys::info().0);
    let mut map = BTreeMap::new();
    loop {
        let req = match server.recv() {
            Ok(req) => req,
            Err(e) => {
                println!("kv: {}", e);
                return 1;
            }
        };
        let key = req.payload.get_word(0);
        match req.op {
            GET => match map.get(&key) {
                Some(&v) => req.reply(Payload::new().word(0, v).bytes()),
                None => req.fail(Error::NotFound),
            },
            PUT => {
                map.insert(key, req.payload.get_word(1));
                req.reply(&[]);
            }
            DEL => {
                map.remove(&key);
                req.reply(&[]);
            }
            _ => req.fail(Error::Invalid),
        }
    }
}
