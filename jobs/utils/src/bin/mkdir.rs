#![no_std]
#![no_main]

use job_utils::{args, fail};
use sydon_rt::{format, fs};

sydon_rt::main!(main);

fn main() -> u64 {
    let args = args();
    if args.is_empty() {
        return fail("usage: mkdir dir...");
    }
    let mut code = 0;
    for path in args {
        if let Err(e) = fs::mkdir(path) {
            code = fail(&format!("{}: {}", path, e));
        }
    }
    code
}
