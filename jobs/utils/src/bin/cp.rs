#![no_std]
#![no_main]

use job_utils::{args, fail};
use alloc::collections::VecDeque;
use sydon_rt::rpc::{Error, Pending};
use sydon_rt::{format, fs, vec};

extern crate alloc;

sydon_rt::main!(main);

fn main() -> u64 {
    let [from, to] = args()[..] else {
        return fail("usage: cp from to");
    };
    let src = match fs::open(from) {
        Ok(f) if f.is_dir() => return fail(&format!("{}: is a directory", from)),
        Ok(f) => f,
        Err(e) => return fail(&format!("{}: {}", from, e)),
    };
    let target = match fs::open(to) {
        Ok(d) if d.is_dir() => format!("{}/{}", to.trim_end_matches('/'), from.rsplit('/').next().unwrap_or(from)),
        _ => to.into(),
    };
    let dst = match fs::create(&target).and_then(|f| f.truncate(0).map(|()| f)) {
        Ok(f) => f,
        Err(e) => return fail(&format!("{}: {}", target, e)),
    };
    let mut buf = vec![0; fs::CHUNK];
    let mut writes: VecDeque<(Pending, usize)> = VecDeque::new();
    let mut at = 0;
    let mut next = (src.size > 0).then(|| src.read_submit(0, fs::CHUNK));
    while let Some(read) = next.take() {
        let n = match read.and_then(|p| p.wait()) {
            Ok(reply) => fs::take(reply, &mut buf),
            Err(e) => return fail(&format!("{}: {}", from, e)),
        };
        if n == 0 {
            break;
        }
        if at + (n as u64) < src.size {
            next = Some(src.read_submit(at + n as u64, fs::CHUNK));
        }
        match dst.write_submit(at, &buf[..n]) {
            Ok(p) => writes.push_back((p, n)),
            Err(e) => return fail(&format!("{}: {}", target, e)),
        }
        at += n as u64;
        while writes.len() > 4 || (next.is_none() && !writes.is_empty()) {
            let (p, n) = writes.pop_front().unwrap();
            match p.wait() {
                Ok(r) if r.get_word(0) as usize == n => {}
                Ok(_) => return fail(&format!("{}: {}", target, Error::Full)),
                Err(e) => return fail(&format!("{}: {}", target, e)),
            }
        }
    }
    0
}
