#![no_std]
#![no_main]

use core::fmt::Write;
use job_utils::{args, fail, Out};
use sydon_rt::String;

sydon_rt::main!(main);

fn main() -> u64 {
    let nums: Result<sydon_rt::Vec<i64>, _> = args().iter().map(|a| a.parse::<i64>()).collect();
    let (first, last) = match nums.as_deref() {
        Ok([last]) => (1, *last),
        Ok([first, last]) => (*first, *last),
        _ => return fail("usage: seq [first] last"),
    };
    let mut out = Out::new();
    let mut s = String::new();
    for i in first..=last {
        s.clear();
        let _ = write!(s, "{}", i);
        out.line(s.as_bytes());
    }
    0
}
