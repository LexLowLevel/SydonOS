#![no_std]
#![no_main]

use job_utils::{args, each_chunk, fail, flag};
use sydon_rt::{format, io, Vec};

sydon_rt::main!(main);

fn main() -> u64 {
    let mut args = args();
    let (l, w, c) = (flag("-l", &mut args), flag("-w", &mut args), flag("-c", &mut args));
    let (mut lines, mut words, mut bytes) = (0u64, 0u64, 0u64);
    let all = !(l || w || c);
    let mut in_word = false;
    let read = each_chunk(args.first().copied(), |chunk| {
        bytes += chunk.len() as u64;
        if !(w || all) {
            lines += chunk.iter().filter(|&&b| b == b'\n').count() as u64;
            return true;
        }
        for &b in chunk {
            let space = b.is_ascii_whitespace();
            lines += (b == b'\n') as u64;
            words += (!space && !in_word) as u64;
            in_word = !space;
        }
        true
    });
    if let Err(e) = read {
        return fail(&e);
    }
    let mut cols = Vec::new();
    for (on, n) in [(l || all, lines), (w || all, words), (c || all, bytes)] {
        if on {
            cols.push(format!("{:>8}", n));
        }
    }
    io::write(format!("{}\n", cols.join(" ")).as_bytes());
    0
}
