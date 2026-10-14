#![no_std]
#![no_main]

use job_utils::{args, each_chunk, fail};
use sydon_rt::io;

sydon_rt::main!(main);

fn main() -> u64 {
    let args = args();
    let paths = if args.is_empty() { sydon_rt::vec![None] } else { args.iter().map(|a| Some(*a)).collect() };
    let mut code = 0;
    for path in paths {
        if let Err(e) = each_chunk(path, |c| {
            io::write(c);
            true
        }) {
            code = fail(&e);
        }
    }
    code
}
