#![no_std]
#![no_main]

use job_utils::args;
use sydon_rt::io;

sydon_rt::main!(main);

fn main() -> u64 {
    let mut args = args();
    let newline = !job_utils::flag("-n", &mut args);
    let mut text = args.join(" ");
    if newline {
        text.push('\n');
    }
    io::write(text.as_bytes());
    0
}
