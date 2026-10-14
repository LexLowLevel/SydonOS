#![no_std]
#![no_main]

use alloc::collections::VecDeque;
use job_utils::{args, each_line, fail, number, Out};

extern crate alloc;

sydon_rt::main!(main);

fn main() -> u64 {
    let mut args = args();
    let keep = match number("-n", &mut args) {
        Ok(n) => n.unwrap_or(10) as usize,
        Err(e) => return fail(&e),
    };
    let mut last: VecDeque<sydon_rt::Vec<u8>> = VecDeque::with_capacity(keep + 1);
    let read = each_line(args.first().copied(), |l| {
        if keep > 0 {
            if last.len() == keep {
                last.pop_front();
            }
            last.push_back(l.to_vec());
        }
        true
    });
    if let Err(e) = read {
        return fail(&e);
    }
    let mut out = Out::new();
    for l in &last {
        out.line(l);
    }
    0
}
