#![no_std]
#![no_main]

use core::fmt::Write;
use job_utils::{args, each_line, fail, flag, Out};

sydon_rt::main!(main);

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || hay.windows(needle.len()).any(|w| w == needle)
}

fn main() -> u64 {
    let mut args = args();
    let (invert, count) = (flag("-v", &mut args), flag("-c", &mut args));
    let Some((pattern, rest)) = args.split_first() else {
        return fail("usage: grep [-v] [-c] pattern [file]");
    };
    let mut out = Out::new();
    let mut found = 0u64;
    let read = each_line(rest.first().copied(), |l| {
        if contains(l, pattern.as_bytes()) != invert {
            found += 1;
            if !count {
                out.line(l);
            }
        }
        true
    });
    if let Err(e) = read {
        return fail(&e);
    }
    if count {
        let mut s = sydon_rt::String::new();
        let _ = write!(s, "{}", found);
        out.line(s.as_bytes());
    }
    (found == 0) as u64
}
