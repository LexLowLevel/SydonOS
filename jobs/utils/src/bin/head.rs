#![no_std]
#![no_main]

use job_utils::{args, each_chunk, each_line, fail, number, Out};
use sydon_rt::io;

sydon_rt::main!(main);

fn main() -> u64 {
    let mut args = args();
    let (lines, bytes) = match (number("-n", &mut args), number("-c", &mut args)) {
        (Ok(n), Ok(c)) => (n, c),
        (Err(e), _) | (_, Err(e)) => return fail(&e),
    };
    let path = args.first().copied();
    let done = match bytes {
        Some(mut left) => each_chunk(path, |c| {
            let n = (left as usize).min(c.len());
            io::write(&c[..n]);
            left -= n as u64;
            left > 0
        }),
        None => {
            let mut left = lines.unwrap_or(10);
            let mut out = Out::new();
            each_line(path, |l| {
                if left == 0 {
                    return false;
                }
                out.line(l);
                left -= 1;
                left > 0
            })
        }
    };
    done.map_or_else(|e| fail(&e), |()| 0)
}
