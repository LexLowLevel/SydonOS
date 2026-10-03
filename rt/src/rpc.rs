use crate::sys::{self, PAYLOAD};
use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Error {
    NoService,
    Timeout,
    NotFound,
    Exists,
    Invalid,
    Down,
    NoMemory,
    Busy,
    Other(u64),
}

impl Error {
    pub fn from_status(s: u64) -> Error {
        match s {
            1 => Error::NoService,
            2 => Error::Timeout,
            3 => Error::NotFound,
            4 => Error::Exists,
            5 => Error::Invalid,
            6 => Error::Down,
            7 => Error::NoMemory,
            8 => Error::Busy,
            s => Error::Other(s),
        }
    }

    pub fn status(self) -> u8 {
        match self {
            Error::NoService => 1,
            Error::Timeout => 2,
            Error::NotFound => 3,
            Error::Exists => 4,
            Error::Invalid => 5,
            Error::Down => 6,
            Error::NoMemory => 7,
            Error::Busy => 8,
            Error::Other(s) => s as u8,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::NoService => write!(f, "no such service"),
            Error::Timeout => write!(f, "timed out"),
            Error::NotFound => write!(f, "not found"),
            Error::Exists => write!(f, "already exists"),
            Error::Invalid => write!(f, "invalid request"),
            Error::Down => write!(f, "core is down"),
            Error::NoMemory => write!(f, "out of memory"),
            Error::Busy => write!(f, "no room for another job on that core"),
            Error::Other(s) => write!(f, "status {}", s),
        }
    }
}

fn handle_or_err(v: u64) -> Result<u64, Error> {
    match sys::status_of(v) {
        Some(s) => Err(Error::from_status(s)),
        None => Ok(v),
    }
}

#[derive(Clone, Copy)]
pub struct Payload {
    pub data: [u8; PAYLOAD],
    pub len: usize,
}

impl Payload {
    pub fn new() -> Payload {
        Payload { data: [0; PAYLOAD], len: 0 }
    }

    pub fn word(mut self, i: usize, v: u64) -> Payload {
        self.data[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
        self.len = self.len.max(i * 8 + 8);
        self
    }

    pub fn name(mut self, at: usize, max: usize, s: &str) -> Payload {
        let n = s.len().min(max);
        self.data[at..at + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len = self.len.max(at + max);
        self
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data[..self.len]
    }

    pub fn get_word(&self, i: usize) -> u64 {
        u64::from_le_bytes(self.data[i * 8..i * 8 + 8].try_into().unwrap())
    }

    pub fn get_name(&self, at: usize, max: usize) -> &str {
        let field = &self.data[at..at + max];
        let n = field.iter().position(|&b| b == 0).unwrap_or(max);
        core::str::from_utf8(&field[..n]).unwrap_or("?")
    }
}

impl Default for Payload {
    fn default() -> Self {
        Payload::new()
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct Service(pub u64);

impl Service {
    pub fn lookup(name: &str) -> Result<Service, Error> {
        handle_or_err(sys::lookup(name)).map(Service)
    }

    pub fn core(&self) -> usize {
        (self.0 >> 16) as usize
    }

    pub fn submit(&self, op: u16, data: &[u8]) -> Result<Pending, Error> {
        self.submit_timeout(op, data, 0)
    }

    pub fn submit_timeout(&self, op: u16, data: &[u8], timeout_ns: u64) -> Result<Pending, Error> {
        handle_or_err(sys::submit(self.0, op, data, timeout_ns)).map(Pending)
    }

    pub fn call(&self, op: u16, data: &[u8]) -> Result<Payload, Error> {
        self.submit(op, data)?.wait()
    }

    pub fn call_forever(&self, op: u16, data: &[u8]) -> Result<Payload, Error> {
        self.submit_timeout(op, data, sys::FOREVER)?.wait()
    }
}

pub struct Pending(u64);

fn finish(v: u64, out: [u8; PAYLOAD]) -> Result<Payload, Error> {
    if v == sys::BAD_ID {
        return Err(Error::Invalid);
    }
    let (status, len) = (v >> 8, (v & 0xFF) as usize);
    if status != 0 {
        return Err(Error::from_status(status));
    }
    Ok(Payload { data: out, len: len.min(PAYLOAD) })
}

impl Pending {
    pub fn poll(&self) -> Option<Result<Payload, Error>> {
        let mut out = [0u8; PAYLOAD];
        match sys::poll(self.0, &mut out) {
            sys::PENDING => None,
            v => Some(finish(v, out)),
        }
    }

    pub fn wait(self) -> Result<Payload, Error> {
        let mut out = [0u8; PAYLOAD];
        finish(sys::wait(self.0, &mut out), out)
    }
}

pub struct Server(u64);

pub struct Request {
    token: u64,
    pub op: u16,
    pub payload: Payload,
}

impl Server {
    pub fn register(name: &str) -> Result<Server, Error> {
        handle_or_err(sys::register(name)).map(Server)
    }

    pub fn recv(&self) -> Result<Request, Error> {
        let mut out = [0u8; PAYLOAD];
        let (token, meta) = sys::recv(self.0, &mut out);
        let token = handle_or_err(token)?;
        let len = (meta as u32 as usize).min(PAYLOAD);
        Ok(Request { token, op: (meta >> 32) as u16, payload: Payload { data: out, len } })
    }
}

impl Request {
    pub fn reply(self, data: &[u8]) {
        sys::reply(self.token, 0, data);
    }

    pub fn fail(self, e: Error) {
        sys::reply(self.token, e.status(), &[]);
    }
}
