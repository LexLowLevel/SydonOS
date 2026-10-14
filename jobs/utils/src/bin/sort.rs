#![no_std]
#![no_main]

use job_utils::{args, each_line, fail, flag, Out};
use sydon_rt::Vec;

sydon_rt::main!(main);

fn numeric(l: &[u8]) -> i64 {
    let s = core::str::from_utf8(l).unwrap_or("").trim_start();
    let end = s.char_indices().find(|&(i, c)| !(c.is_ascii_digit() || (i == 0 && c == '-'))).map_or(s.len(), |(i, _)| i);
    s[..end].parse().unwrap_or(0)
}

fn main() -> u64 {
    let mut args = args();
    let (reverse, numbers) = (flag("-r", &mut args), flag("-n", &mut args));
    let mut lines: Vec<Vec<u8>> = Vec::new();
    if let Err(e) = each_line(args.first().copied(), |l| {
        lines.push(l.to_vec());
        true
    }) {
        return fail(&e);
    }
    if numbers {
        lines.sort_by_cached_key(|l| numeric(l));
    } else {
        lines.sort_unstable();
    }
    if reverse {
        lines.reverse();
    }
    let mut out = Out::new();
    for l in &lines {
        out.line(l);
    }
    0
}
