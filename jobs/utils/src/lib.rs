#![no_std]

use sydon_rt::{eprintln, format, fs, io, vec, String, Vec};

pub fn fail(msg: &str) -> u64 {
    let name = io::args().first().map_or("?", |a| a.as_str());
    eprintln!("{}: {}", name, msg);
    1
}

pub fn args() -> Vec<&'static str> {
    io::args().iter().skip(1).map(|a| a.as_str()).collect()
}

pub fn number(flag: &str, args: &mut Vec<&str>) -> Result<Option<u64>, String> {
    let Some(i) = args.iter().position(|a| a.starts_with(flag)) else {
        return Ok(None);
    };
    let text = if args[i].len() > flag.len() {
        args.remove(i)[flag.len()..].into()
    } else {
        args.remove(i);
        if i >= args.len() {
            return Err(format!("{} needs a number", flag));
        }
        String::from(args.remove(i))
    };
    text.parse().map(Some).map_err(|_| format!("{}: not a number", text))
}

pub fn flag(name: &str, args: &mut Vec<&str>) -> bool {
    let before = args.len();
    args.retain(|a| *a != name);
    args.len() != before
}

pub fn each_chunk(path: Option<&str>, mut f: impl FnMut(&[u8]) -> bool) -> Result<(), String> {
    let Some(path) = path else {
        io::read_chunks(f);
        return Ok(());
    };
    let file = fs::open(path).map_err(|e| format!("{}: {}", path, e))?;
    if file.is_dir() {
        return Err(format!("{}: is a directory", path));
    }
    let mut buf = vec![0; fs::CHUNK];
    let mut at = 0;
    while at < file.size {
        let n = file.read_at(at, &mut buf).map_err(|e| format!("{}: {}", path, e))?;
        if n == 0 || !f(&buf[..n]) {
            break;
        }
        at += n as u64;
    }
    Ok(())
}

pub fn each_line(path: Option<&str>, mut f: impl FnMut(&[u8]) -> bool) -> Result<(), String> {
    let mut carry = Vec::new();
    let mut going = true;
    each_chunk(path, |chunk| {
        let mut rest = chunk;
        while let Some(i) = rest.iter().position(|&b| b == b'\n') {
            going = if carry.is_empty() {
                f(&rest[..i])
            } else {
                carry.extend_from_slice(&rest[..i]);
                let keep = f(&carry);
                carry.clear();
                keep
            };
            if !going {
                return false;
            }
            rest = &rest[i + 1..];
        }
        carry.extend_from_slice(rest);
        true
    })?;
    if going && !carry.is_empty() {
        f(&carry);
    }
    Ok(())
}

pub struct Out(Vec<u8>);

impl Out {
    pub fn new() -> Out {
        Out(Vec::with_capacity(io::CHUNK))
    }

    pub fn line(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
        self.0.push(b'\n');
        if self.0.len() >= io::CHUNK {
            self.flush();
        }
    }

    pub fn flush(&mut self) {
        io::write(&self.0);
        self.0.clear();
    }
}

impl Default for Out {
    fn default() -> Self {
        Out::new()
    }
}

impl Drop for Out {
    fn drop(&mut self) {
        self.flush();
    }
}
