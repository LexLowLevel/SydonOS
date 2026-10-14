use crate::rpc::{Error, Payload, Pending, Service};
use crate::sys;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

const OPEN: u16 = 1;
const READ: u16 = 2;
const STAT: u16 = 3;
const READDIR: u16 = 4;
const CREATE: u16 = 5;
const WRITE: u16 = 6;
const TRUNCATE: u16 = 7;
const REMOVE: u16 = 8;
const MKDIR: u16 = 9;
const SYNC: u16 = 10;
pub const CHUNK: usize = 64 * 1024;
const DIR: u64 = 0x4000;

pub struct File {
    svc: Service,
    ino: u64,
    pub size: u64,
    mode: u64,
}

pub struct Entry {
    pub name: String,
    pub size: u64,
    pub dir: bool,
}

fn service() -> Result<Service, Error> {
    Service::lookup("fs")
}

pub fn open(path: &str) -> Result<File, Error> {
    let svc = service()?;
    let p = svc.call(OPEN, Payload::new().name(0, 40, path).bytes())?;
    Ok(File { svc, ino: p.get_word(0), size: p.get_word(1), mode: p.get_word(2) })
}

fn path_op(op: u16, path: &str) -> Result<Payload, Error> {
    service()?.call(op, Payload::new().name(0, 40, path).bytes())
}

pub fn create(path: &str) -> Result<File, Error> {
    let svc = service()?;
    let p = svc.call(CREATE, Payload::new().name(0, 40, path).bytes())?;
    Ok(File { svc, ino: p.get_word(0), size: p.get_word(1), mode: p.get_word(2) })
}

pub fn mkdir(path: &str) -> Result<(), Error> {
    path_op(MKDIR, path).map(|_| ())
}

pub fn remove(path: &str) -> Result<(), Error> {
    path_op(REMOVE, path).map(|_| ())
}

pub fn sync() -> Result<(), Error> {
    service()?.call(SYNC, &[]).map(|_| ())
}

pub fn read_dir(path: &str) -> Result<Vec<Entry>, Error> {
    let dir = open(path)?;
    if !dir.is_dir() {
        return Err(Error::Invalid);
    }
    let mut out = Vec::new();
    loop {
        match dir.svc.call(READDIR, Payload::new().word(0, dir.ino).word(1, out.len() as u64).bytes()) {
            Ok(p) => out.push(Entry {
                name: p.get_name(16, 24).to_string(),
                size: p.get_word(1) >> 8,
                dir: p.get_word(1) & 0xFF == 2,
            }),
            Err(Error::NotFound) => return Ok(out),
            Err(e) => return Err(e),
        }
    }
}

impl File {
    pub fn from_raw(svc: u64, ino: u64) -> File {
        File { svc: Service(svc), ino, size: 0, mode: 0x8000 }
    }

    pub fn raw(&self) -> (u64, u64) {
        (self.svc.0, self.ino)
    }

    pub fn is_dir(&self) -> bool {
        self.mode & 0xF000 == DIR
    }

    pub fn read_submit(&self, offset: u64, len: usize) -> Result<Pending, Error> {
        let len = len.min(CHUNK) as u64;
        self.svc.submit(READ, Payload::new().word(0, self.ino).word(1, offset).word(2, len).bytes())
    }

    pub fn read_at(&self, offset: u64, dst: &mut [u8]) -> Result<usize, Error> {
        let mut done = 0;
        while done < dst.len() {
            let n = take(self.read_submit(offset + done as u64, dst.len() - done)?.wait()?, &mut dst[done..]);
            if n == 0 {
                break;
            }
            done += n;
        }
        Ok(done)
    }

    pub fn write_submit(&self, offset: u64, data: &[u8]) -> Result<Pending, Error> {
        let body = Payload::new().word(0, self.ino).word(1, offset).word(2, data.len().min(CHUNK) as u64);
        loop {
            match self.svc.submit_buf(WRITE, body.bytes(), &data[..data.len().min(CHUNK)]) {
                Err(Error::Busy) => sys::sleep_ns(50_000),
                out => return out,
            }
        }
    }

    pub fn write_at(&self, offset: u64, data: &[u8]) -> Result<usize, Error> {
        let mut done = 0;
        for chunk in data.chunks(CHUNK) {
            let n = self.write_submit(offset + done as u64, chunk)?.wait()?.get_word(0) as usize;
            done += n;
            if n < chunk.len() {
                break;
            }
        }
        Ok(done)
    }

    pub fn truncate(&self, size: u64) -> Result<(), Error> {
        self.svc.call(TRUNCATE, Payload::new().word(0, self.ino).word(1, size).bytes()).map(|_| ())
    }

    pub fn len(&self) -> Result<u64, Error> {
        self.svc.call(STAT, Payload::new().word(0, self.ino).bytes()).map(|p| p.get_word(0))
    }

    pub fn read_to_end(&self) -> Result<Vec<u8>, Error> {
        let mut data = vec![0; self.size as usize];
        let n = self.read_at(0, &mut data)?;
        data.truncate(n);
        Ok(data)
    }
}

pub fn manifest(path: &str) -> Option<(&'static str, String)> {
    let file = open(path).ok()?;
    let mut head = vec![0u8; 8192.min(file.size as usize)];
    let n = file.read_at(0, &mut head).ok()?;
    let at = head[..n].windows(8).position(|w| w == b"SYDMAN01")?;
    let m = head.get(at..at + 64)?;
    let class = match m[8] {
        1 => "io",
        2 => "io-latency",
        3 => "compute",
        _ => "other",
    };
    let len = m[16..48].iter().position(|&b| b == 0).unwrap_or(32);
    Some((class, String::from_utf8_lossy(&m[16..16 + len]).to_string()))
}

pub fn take(reply: Payload, dst: &mut [u8]) -> usize {
    let (len, skew, buf) = (reply.get_word(0) as usize, reply.get_word(1) as usize, reply.get_word(4));
    let n = len.min(dst.len());
    if len == 0 {
        return 0;
    }
    sys::buf_copy(buf, &mut dst[..n], skew);
    n
}
