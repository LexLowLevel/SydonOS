<<<<<<< Updated upstream
use crate::rpc::Service;
use crate::{sys, String};
use core::fmt;

const CONSOLE_READ: u16 = 2;
=======
use crate::fs::File;
use crate::rpc::{Error, Payload, Pending, Service};
use crate::sys;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::fmt;

const CONSOLE_READ: u16 = 2;
const CONSOLE_FOREGROUND: u16 = 4;
const CONSOLE_FOREGROUND_ADD: u16 = 5;
const PIPE_WRITE: u16 = 1;
const PIPE_READ: u16 = 2;
const FS_STAT: u16 = 3;
const FS_WRITE: u16 = 6;

pub const CHUNK: usize = 64 * 1024;
pub const BROKEN_PIPE: u64 = 141;

// must match the start block in the kernel's user.rs
const START_MAGIC: &[u8; 8] = b"SYDSTART";
const START_STREAMS: usize = 16;
const START_ARGS: usize = 88;
pub const START_MAX: usize = 4096;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Stream {
    Console,
    PipeRead(u64, u64),
    PipeWrite(u64, u64),
    File(u64, u64),
    NewPipe,
    Null,
    FileAppend(u64, u64),
}

impl Stream {
    fn encode(self) -> (u8, u64, u64) {
        match self {
            Stream::Console => (0, 0, 0),
            Stream::PipeRead(h, id) => (1, h, id),
            Stream::PipeWrite(h, id) => (2, h, id),
            Stream::File(h, ino) => (3, h, ino),
            Stream::NewPipe => (4, 0, 0),
            Stream::Null => (5, 0, 0),
            Stream::FileAppend(h, ino) => (6, h, ino),
        }
    }

    fn decode(kind: u8, h: u64, id: u64) -> Stream {
        match kind {
            0 => Stream::Console,
            1 => Stream::PipeRead(h, id),
            2 => Stream::PipeWrite(h, id),
            3 => Stream::File(h, id),
            6 => Stream::FileAppend(h, id),
            _ => Stream::Null,
        }
    }
}

pub fn start_block(streams: [Stream; 3], args: &[&str]) -> Vec<u8> {
    let mut b = vec![0u8; START_ARGS];
    b[..8].copy_from_slice(START_MAGIC);
    for (i, s) in streams.iter().enumerate() {
        let (kind, h, id) = s.encode();
        let at = START_STREAMS + 24 * i;
        b[at] = kind;
        b[at + 8..at + 16].copy_from_slice(&h.to_le_bytes());
        b[at + 16..at + 24].copy_from_slice(&id.to_le_bytes());
    }
    for a in args {
        if b.len() + a.len() + 1 > START_MAX {
            break;
        }
        b.extend_from_slice(a.as_bytes());
        b.push(0);
    }
    let len = b.len() as u32;
    b[8..12].copy_from_slice(&len.to_le_bytes());
    b
}

struct Input {
    stream: Stream,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
    offset: u64,
    next: Option<Pending>,
    eof: bool,
}

struct Output {
    stream: Stream,
    buf: Vec<u8>,
    inflight: VecDeque<(Pending, u64)>,
    offset: Option<u64>,
    direct: bool,
}

impl Output {
    const fn new(direct: bool) -> Output {
        Output { stream: Stream::Console, buf: Vec::new(), inflight: VecDeque::new(), offset: None, direct }
    }
}

struct Global<T>(UnsafeCell<T>);

unsafe impl<T> Sync for Global<T> {}

// jobs have one thread. stdin, stdout and stderr are kept apart so reading
// with read_chunks can print from inside the callback.
static ARGS: Global<Vec<String>> = Global(UnsafeCell::new(Vec::new()));
static INPUT: Global<Input> = Global(UnsafeCell::new(Input {
    stream: Stream::Console,
    buf: Vec::new(),
    pos: 0,
    len: 0,
    offset: 0,
    next: None,
    eof: false,
}));
static OUT: Global<Output> = Global(UnsafeCell::new(Output::new(false)));
static ERR: Global<Output> = Global(UnsafeCell::new(Output::new(true)));

fn get<T>(g: &'static Global<T>) -> &'static mut T {
    unsafe { &mut *g.0.get() }
}

pub fn init(start: *const u8, len: usize) {
    let mut streams = [Stream::Console; 3];
    let mut args = Vec::new();
    if !start.is_null() && (START_ARGS..=START_MAX).contains(&len) {
        let b = unsafe { core::slice::from_raw_parts(start, len) };
        if &b[..8] == START_MAGIC {
            for (i, s) in streams.iter_mut().enumerate() {
                let at = START_STREAMS + 24 * i;
                let word = |o: usize| u64::from_le_bytes(b[at + o..at + o + 8].try_into().unwrap());
                *s = Stream::decode(b[at], word(8), word(16));
            }
            args = b[START_ARGS..]
                .split(|&c| c == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into())
                .collect();
        }
    }
    *get(&ARGS) = args;
    get(&INPUT).stream = streams[0];
    get(&OUT).stream = streams[1];
    get(&ERR).stream = streams[2];
}

pub fn args() -> &'static [String] {
    get(&ARGS)
}

pub fn stdin() -> Stream {
    get(&INPUT).stream
}

pub fn stdout() -> Stream {
    get(&OUT).stream
}

fn console() -> Result<Service, Error> {
    Service::lookup("console")
}

fn read_pipe(h: u64, id: u64) -> Result<Pending, Error> {
    Service(h).submit_timeout(PIPE_READ, Payload::new().word(0, id).bytes(), sys::FOREVER)
}

// the next read from a pipe goes out before this one is copied, so the
// writer's core can keep going
fn refill(inp: &mut Input) -> bool {
    inp.pos = 0;
    inp.len = 0;
    if inp.eof {
        return false;
    }
    if inp.buf.is_empty() {
        inp.buf = vec![0; CHUNK];
    }
    let got = match inp.stream {
        Stream::Console => console().and_then(|c| c.call_forever(CONSOLE_READ, &[])).map(|p| {
            inp.buf[..p.len].copy_from_slice(p.bytes());
            p.len
        }),
        Stream::PipeRead(h, id) => {
            let pending = match inp.next.take() {
                Some(p) => Ok(p),
                None => read_pipe(h, id),
            };
            pending.and_then(|p| p.wait()).map(|reply| {
                let len = (reply.get_word(0) as usize).min(CHUNK);
                if len > 0 {
                    inp.next = read_pipe(h, id).ok();
                    sys::buf_copy(reply.get_word(4), &mut inp.buf[..len], 0);
                }
                len
            })
        }
        Stream::File(h, ino) => File::from_raw(h, ino).read_at(inp.offset, &mut inp.buf).inspect(|&n| inp.offset += n as u64),
        _ => Ok(0),
    };
    inp.len = got.unwrap_or(0);
    // ctrl-d ends one read from the console, not all of them
    inp.eof = inp.len == 0 && inp.stream != Stream::Console;
    !inp.eof
}

pub fn read_chunks(mut f: impl FnMut(&[u8]) -> bool) {
    let inp = get(&INPUT);
    loop {
        if inp.pos == inp.len && !refill(inp) {
            return;
        }
        let (pos, len) = (inp.pos, inp.len);
        inp.pos = len;
        if !f(&inp.buf[pos..len]) {
            return;
        }
    }
}

pub fn read_line() -> Option<String> {
    let inp = get(&INPUT);
    let mut line = Vec::new();
    loop {
        if inp.pos == inp.len && !refill(inp) {
            return (!line.is_empty()).then(|| String::from_utf8_lossy(&line).into());
        }
        let avail = &inp.buf[inp.pos..inp.len];
        match avail.iter().position(|&b| b == b'\n') {
            Some(i) => {
                line.extend_from_slice(&avail[..i]);
                inp.pos += i + 1;
                return Some(String::from_utf8_lossy(&line).into());
            }
            None => {
                line.extend_from_slice(avail);
                inp.pos = inp.len;
            }
        }
    }
}

pub fn read_to_end() -> Vec<u8> {
    let mut all = Vec::new();
    read_chunks(|c| {
        all.extend_from_slice(c);
        true
    });
    all
}

fn broken(o: &mut Output) -> ! {
    o.stream = Stream::Null;
    sys::exit(BROKEN_PIPE)
}

fn failed(o: &mut Output, e: Error) -> ! {
    if let Stream::PipeWrite(..) = o.stream {
        broken(o);
    }
    o.stream = Stream::Null;
    let msg = alloc::format!("{}: writing the output: {}\n", args().first().map_or("?", |a| a.as_str()), e);
    sys::print(msg.as_bytes());
    sys::exit(1)
}

// a file takes several writes in flight, the fs commits them together. a
// pipe gets one, so a slow reader holds back at most one extra chunk.
fn settle(o: &mut Output, keep: usize) {
    while o.inflight.len() > keep {
        let (p, len) = o.inflight.pop_front().unwrap();
        match p.wait() {
            Ok(r) if matches!(o.stream, Stream::FileAppend(..)) && r.get_word(0) < len => failed(o, Error::Full),
            Ok(_) => {}
            Err(e) => failed(o, e),
        }
    }
}

fn send(o: &mut Output) {
    let depth = if let Stream::FileAppend(..) = o.stream { 4 } else { 1 };
    settle(o, depth - 1);
    let len = o.buf.len() as u64;
    let (h, op, body) = match o.stream {
        _ if len == 0 => return,
        Stream::PipeWrite(h, id) => (h, PIPE_WRITE, Payload::new().word(0, id).word(1, len)),
        Stream::FileAppend(h, ino) => {
            let at = match o.offset {
                Some(at) => at,
                None => match Service(h).call(FS_STAT, Payload::new().word(0, ino).bytes()) {
                    Ok(p) => p.get_word(0),
                    Err(e) => failed(o, e),
                },
            };
            o.offset = Some(at + len);
            (h, FS_WRITE, Payload::new().word(0, ino).word(1, at).word(2, len))
        }
        _ => {
            o.buf.clear();
            return;
        }
    };
    loop {
        match Service(h).submit_buf(op, body.bytes(), &o.buf) {
            Ok(p) => {
                o.inflight.push_back((p, len));
                break;
            }
            Err(Error::Busy) => sys::sleep_ns(50_000),
            Err(e) => failed(o, e),
        }
    }
    o.buf.clear();
}

fn write_to(o: &mut Output, mut bytes: &[u8]) {
    match o.stream {
        Stream::Console => sys::print(bytes),
        Stream::PipeWrite(..) | Stream::FileAppend(..) => {
            while !bytes.is_empty() {
                let n = (CHUNK - o.buf.len()).min(bytes.len());
                o.buf.extend_from_slice(&bytes[..n]);
                bytes = &bytes[n..];
                if o.buf.len() == CHUNK {
                    send(o);
                }
            }
            if o.direct {
                send(o);
                settle(o, 0);
            }
        }
        _ => {}
    }
}

pub fn write(bytes: &[u8]) {
    write_to(get(&OUT), bytes);
}

pub fn write_err(bytes: &[u8]) {
    write_to(get(&ERR), bytes);
}

pub fn flush() {
    for o in [get(&OUT), get(&ERR)] {
        send(o);
        settle(o, 0);
    }
}
>>>>>>> Stashed changes

// formats the whole thing first so one print is one piece of output
pub fn _print(args: fmt::Arguments) {
    match args.as_str() {
        Some(s) => write(s.as_bytes()),
        None => write(alloc::fmt::format(args).as_bytes()),
    }
}

pub fn _eprint(args: fmt::Arguments) {
    write_err(alloc::fmt::format(args).as_bytes());
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::io::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::io::_print(format_args!("\n")));
    ($($arg:tt)*) => ($crate::io::_print(format_args!("{}\n", format_args!($($arg)*))));
}

<<<<<<< Updated upstream
// a line typed at the console, without its newline. None at end of input
// (ctrl-d on an empty line) or when there is no console.
pub fn read_line() -> Option<String> {
    let console = Service::lookup("console").ok()?;
    let mut line = alloc::vec::Vec::new();
    loop {
        let p = console.call_forever(CONSOLE_READ, &[]).ok()?;
        if p.len == 0 {
            return None;
        }
        line.extend_from_slice(p.bytes());
        if line.last() == Some(&b'\n') {
            line.pop();
            return Some(String::from_utf8_lossy(&line).into());
        }
=======
#[macro_export]
macro_rules! eprint {
    ($($arg:tt)*) => ($crate::io::_eprint(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! eprintln {
    () => ($crate::io::_eprint(format_args!("\n")));
    ($($arg:tt)*) => ($crate::io::_eprint(format_args!("{}\n", format_args!($($arg)*))));
}

pub fn set_foreground(pids: &[u64]) -> Result<(), Error> {
    let console = console()?;
    let mut op = CONSOLE_FOREGROUND;
    let mut rest = pids;
    loop {
        let n = rest.len().min(5);
        let mut p = Payload::new();
        for (i, &pid) in rest[..n].iter().enumerate() {
            p = p.word(i, pid);
        }
        console.call(op, p.bytes())?;
        rest = &rest[n..];
        if rest.is_empty() {
            return Ok(());
        }
        op = CONSOLE_FOREGROUND_ADD;
>>>>>>> Stashed changes
    }
}
