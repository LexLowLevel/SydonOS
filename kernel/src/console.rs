use crate::rpc::{self, Answer, Body, Request, INVALID, OK, PAYLOAD};
use crate::{director, serial};
use crate::sync::SpinLock;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

pub const OP_PRINT: u16 = 1;
pub const OP_READ: u16 = 2;
pub const OP_CANCEL: u16 = 3;
pub const OP_FOREGROUND: u16 = 4;

pub const IRQ_VECTOR: u8 = 0x24;

const LINE_MAX: usize = 1024;
const HISTORY: usize = 16;
const RAW_SIZE: usize = 4096;
const EOF: u8 = 0x04;

struct Raw {
    buf: [u8; RAW_SIZE],
    head: usize,
    len: usize,
}

static RAW: SpinLock<Raw> = SpinLock::new(Raw { buf: [0; RAW_SIZE], head: 0, len: 0 });
static PENDING: AtomicBool = AtomicBool::new(false);

#[derive(PartialEq)]
enum Esc {
    None,
    Seen,
    Bracket,
}

struct Console {
    line: Vec<u8>,
    history: Vec<Vec<u8>>,
    browse: usize,
    esc: Esc,
    cooked: VecDeque<u8>,
    readers: VecDeque<u64>,
    partial: Vec<(usize, Vec<u8>)>,
    foreground: Vec<u64>,
}

static CONSOLE: SpinLock<Option<Console>> = SpinLock::new(None);

pub fn init() {
    *CONSOLE.lock() = Some(Console {
        line: Vec::new(),
        history: Vec::new(),
        browse: 0,
        esc: Esc::None,
        cooked: VecDeque::new(),
        readers: VecDeque::new(),
        partial: Vec::new(),
        foreground: Vec::new(),
    });
    rpc::register_kernel(rpc::SVC_CONSOLE, handler);
}

pub fn on_irq() {
    let mut raw = RAW.lock();
    let mut push = |b: u8| {
        if raw.len < RAW_SIZE {
            let at = (raw.head + raw.len) % RAW_SIZE;
            raw.buf[at] = b;
            raw.len += 1;
        }
    };
    serial::on_irq(&mut push);
    drop(raw);
    PENDING.store(true, Ordering::Release);
    rpc::wake_executor();
}

pub fn input_pending() -> bool {
    PENDING.load(Ordering::Acquire)
}

pub fn write(bytes: &[u8]) {
    serial::write_bytes(bytes);
}

// prints arrive in 40-byte pieces, MORE set on all but the last
fn handler(req: &Request) -> Answer {
    match req.op {
        OP_PRINT => {
            with(|c| {
                let i = match c.partial.iter().position(|(src, _)| *src == req.src) {
                    Some(i) => i,
                    None => {
                        c.partial.push((req.src, Vec::new()));
                        c.partial.len() - 1
                    }
                };
                c.partial[i].1.extend_from_slice(req.body.as_bytes());
                if req.flags & rpc::MORE == 0 {
                    let (_, text) = c.partial.swap_remove(i);
                    write(&text);
                }
            });
            Answer::Reply(OK, Body::EMPTY)
        }
        OP_CANCEL if req.from_kernel() => {
            let token = rpc::token_of(req.src, req.body.word(0));
            with(|c| c.readers.retain(|&t| t != token));
            Answer::Reply(OK, Body::EMPTY)
        }
        // the jobs ctrl-c kills, set by the shell while it waits for them
        OP_FOREGROUND if req.privileged() => {
            with(|c| {
                c.foreground.clear();
                c.foreground.extend((0..5).map(|i| req.body.word(i)).filter(|&p| p != 0));
            });
            Answer::Reply(OK, Body::EMPTY)
        }
        OP_READ => {
            let now = with(|c| match take_chunk(c) {
                Some(body) => Some(body),
                None => {
                    c.readers.push_back(req.token);
                    None
                }
            });
            match now {
                Some(body) => Answer::Reply(OK, body),
                None => Answer::Later,
            }
        }
        _ => Answer::Reply(INVALID, Body::EMPTY),
    }
}

fn with<R>(f: impl FnOnce(&mut Console) -> R) -> R {
    f(CONSOLE.lock().as_mut().expect("console: not initialised"))
}

fn take_chunk(c: &mut Console) -> Option<Body> {
    if c.cooked.front() == Some(&EOF) {
        c.cooked.pop_front();
        return Some(Body::EMPTY);
    }
    if !c.cooked.contains(&b'\n') {
        return None;
    }
    let mut out = [0u8; PAYLOAD];
    let mut n = 0;
    while n < PAYLOAD {
        let Some(b) = c.cooked.pop_front() else { break };
        out[n] = b;
        n += 1;
        if b == b'\n' {
            break;
        }
    }
    Some(Body::bytes(&out[..n]))
}

fn replace_line(c: &mut Console, new: Vec<u8>) {
    let mut out = Vec::new();
    for _ in 0..c.line.len() {
        out.extend_from_slice(b"\x08 \x08");
    }
    out.extend_from_slice(&new);
    write(&out);
    c.line = new;
}

fn edit(c: &mut Console, b: u8) {
    match c.esc {
        Esc::Seen => {
            c.esc = if b == b'[' { Esc::Bracket } else { Esc::None };
            return;
        }
        // CSI parameter bytes
        Esc::Bracket if (0x30..=0x3F).contains(&b) => return,
        Esc::Bracket => {
            c.esc = Esc::None;
            match b {
                b'A' if c.browse > 0 => {
                    c.browse -= 1;
                    let item = c.history[c.browse].clone();
                    replace_line(c, item);
                }
                b'B' if c.browse < c.history.len() => {
                    c.browse += 1;
                    let item = c.history.get(c.browse).cloned().unwrap_or_default();
                    replace_line(c, item);
                }
                _ => {}
            }
            return;
        }
        Esc::None => {}
    }
    match b {
        0x1B => c.esc = Esc::Seen,
        b'\r' | b'\n' => {
            write(b"\n");
            let line = core::mem::take(&mut c.line);
            if !line.is_empty() && c.history.last() != Some(&line) {
                if c.history.len() == HISTORY {
                    c.history.remove(0);
                }
                c.history.push(line.clone());
            }
            c.browse = c.history.len();
            c.cooked.extend(line);
            c.cooked.push_back(b'\n');
        }
        0x7F | 0x08 => {
            if c.line.pop().is_some() {
                write(b"\x08 \x08");
            }
        }
        0x03 => {
            write(b"^C\n");
            c.line.clear();
            if c.foreground.is_empty() {
                c.cooked.push_back(b'\n');
            }
            for pid in core::mem::take(&mut c.foreground) {
                director::kill(pid);
            }
        }
        EOF if c.line.is_empty() => c.cooked.push_back(EOF),
        0x15 => replace_line(c, Vec::new()),
        0x20..=0x7E if c.line.len() < LINE_MAX => {
            c.line.push(b);
            write(&[b]);
        }
        _ => {}
    }
}

pub fn process_input() {
    if !PENDING.swap(false, Ordering::AcqRel) {
        return;
    }
    let mut bytes = [0u8; RAW_SIZE];
    let n = {
        let mut raw = RAW.lock();
        let n = raw.len;
        for (i, slot) in bytes.iter_mut().enumerate().take(n) {
            *slot = raw.buf[(raw.head + i) % RAW_SIZE];
        }
        raw.head = (raw.head + n) % RAW_SIZE;
        raw.len = 0;
        n
    };
    let ready = with(|c| {
        for &b in &bytes[..n] {
            edit(c, b);
        }
        let mut ready = Vec::new();
        while !c.readers.is_empty() {
            let Some(body) = take_chunk(c) else { break };
            ready.push((c.readers.pop_front().unwrap(), body));
        }
        ready
    });
    for (token, body) in ready {
        rpc::reply(token, OK, body);
    }
}

pub fn print_remote(bytes: &[u8]) {
    let h = rpc::handle(0, rpc::SVC_CONSOLE);
    rpc::batch(|| {
        let mut chunks = bytes.chunks(PAYLOAD).peekable();
        while let Some(chunk) = chunks.next() {
            rpc::oneway_more(h, OP_PRINT, Body::bytes(chunk), chunks.peek().is_some());
        }
    });
}
