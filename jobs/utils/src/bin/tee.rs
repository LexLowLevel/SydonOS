#![no_std]
#![no_main]

use job_utils::{args, fail, flag};
use sydon_rt::{format, fs, io};

sydon_rt::main!(main);

fn main() -> u64 {
    let mut args = args();
    let append = flag("-a", &mut args);
    let [path] = args[..] else {
        return fail("usage: tee [-a] file");
    };
    let file = match fs::create(path) {
        Ok(f) if f.is_dir() => return fail(&format!("{}: is a directory", path)),
        Ok(f) => f,
        Err(e) => return fail(&format!("{}: {}", path, e)),
    };
    let start = if append { file.len() } else { file.truncate(0).map(|()| 0) };
    let mut at = match start {
        Ok(at) => at,
        Err(e) => return fail(&format!("{}: {}", path, e)),
    };
    let mut err = None;
    io::read_chunks(|c| {
        io::write(c);
        match file.write_at(at, c) {
            Ok(n) if n == c.len() => at += n as u64,
            Ok(_) => err = Some(sydon_rt::rpc::Error::Full),
            Err(e) => err = Some(e),
        }
        err.is_none()
    });
    match err {
        Some(e) => fail(&format!("{}: {}", path, e)),
        None => 0,
    }
}
