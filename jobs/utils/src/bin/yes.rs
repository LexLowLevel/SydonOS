#![no_std]
#![no_main]

use sydon_rt::{io, vec, Vec};

sydon_rt::main!(main);

fn main() -> u64 {
    let args = job_utils::args();
    let mut line: Vec<u8> = if args.is_empty() { b"y".to_vec() } else { args.join(" ").into_bytes() };
    line.push(b'\n');
    let mut block = vec![];
    while block.len() + line.len() <= io::CHUNK {
        block.extend_from_slice(&line);
    }
    loop {
        io::write(&block);
    }
}
