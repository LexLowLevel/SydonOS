use crate::rpc::{self, Answer, Body, Request, CLOSED, INVALID, NOT_FOUND, OK};
use crate::sync::IrqCell;
use crate::{bufs, cpu};
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

pub const OP_WRITE: u16 = 1;
pub const OP_READ: u16 = 2;
pub const OP_CLOSE_WRITE: u16 = 3;
pub const OP_CLOSE_READ: u16 = 4;

const QUEUE_MAX: usize = 4;

struct Pipe {
    id: u64,
    writers: u32,
    reader: bool,
    chunks: VecDeque<(u64, u64)>,
    reader_waiting: Option<u64>,
    writers_waiting: VecDeque<(u64, u64, u64)>,
}

enum Out {
    Reply(u64, u8),
    Chunk(u64, u64, u64),
    Eof(u64),
    Free(u64),
}

static PIPES: IrqCell<Vec<Pipe>> = IrqCell::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);

// ids mix a counter with the clock so they are hard to guess
pub fn create() -> u64 {
    let id = NEXT.fetch_add(1, Ordering::Relaxed) << 32 | (cpu::rdtsc() & 0xFFFF_FFFF);
    PIPES.with(|p| {
        p.push(Pipe {
            id,
            writers: 1,
            reader: true,
            chunks: VecDeque::new(),
            reader_waiting: None,
            writers_waiting: VecDeque::new(),
        })
    });
    id
}

pub fn discard(id: u64) {
    PIPES.with(|p| p.retain(|p| p.id != id));
}

pub fn handler(req: &Request) -> Answer {
    let id = req.body.word(0);
    let out = match req.op {
        OP_WRITE => match req.buf() {
            Some(buf) => write(id, req.token, buf, req.body.word(1).min(bufs::SIZE as u64)),
            None => vec_of(Out::Reply(req.token, INVALID)),
        },
        OP_READ => read(id, req.token),
        OP_CLOSE_WRITE if req.from_kernel() => close_write(id),
        OP_CLOSE_READ if req.from_kernel() => close_read(id),
        _ => {
            if let Some(b) = req.buf() {
                bufs::free(b);
            }
            vec_of(Out::Reply(req.token, INVALID))
        }
    };
    for o in out {
        match o {
            Out::Reply(token, status) => rpc::reply(token, status, Body::EMPTY),
            Out::Chunk(token, buf, len) => rpc::reply_buf(token, OK, Body::words(&[len, 0]), buf),
            Out::Eof(token) => rpc::reply(token, OK, Body::words(&[0, 0])),
            Out::Free(buf) => bufs::free(buf),
        }
    }
    Answer::Later
}

fn vec_of(o: Out) -> Vec<Out> {
    let mut v = Vec::new();
    v.push(o);
    v
}

fn with_pipe(id: u64, f: impl FnOnce(&mut Pipe, &mut Vec<Out>), missing: Out) -> Vec<Out> {
    PIPES.with(|pipes| {
        let mut out = Vec::new();
        match pipes.iter_mut().find(|p| p.id == id) {
            Some(p) => f(p, &mut out),
            None => out.push(missing),
        }
        pipes.retain(|p| p.reader || p.writers > 0);
        out
    })
}

fn write(id: u64, token: u64, buf: u64, len: u64) -> Vec<Out> {
    let mut out = with_pipe(
        id,
        |p, out| {
            if !p.reader {
                out.push(Out::Free(buf));
                out.push(Out::Reply(token, CLOSED));
            } else if p.chunks.len() >= QUEUE_MAX {
                p.writers_waiting.push_back((token, buf, len));
            } else {
                p.chunks.push_back((buf, len));
                out.push(Out::Reply(token, OK));
                if let Some(reader) = p.reader_waiting.take() {
                    let (b, l) = p.chunks.pop_front().unwrap();
                    out.push(Out::Chunk(reader, b, l));
                }
            }
        },
        Out::Reply(token, NOT_FOUND),
    );
    if matches!(out.last(), Some(Out::Reply(_, NOT_FOUND))) {
        out.insert(0, Out::Free(buf));
    }
    out
}

fn read(id: u64, token: u64) -> Vec<Out> {
    with_pipe(
        id,
        |p, out| {
            if let Some((b, l)) = p.chunks.pop_front() {
                out.push(Out::Chunk(token, b, l));
                if let Some((wt, wb, wl)) = p.writers_waiting.pop_front() {
                    p.chunks.push_back((wb, wl));
                    out.push(Out::Reply(wt, OK));
                }
            } else if p.writers == 0 {
                out.push(Out::Eof(token));
            } else {
                p.reader_waiting = Some(token);
            }
        },
        Out::Reply(token, NOT_FOUND),
    )
}

fn close_write(id: u64) -> Vec<Out> {
    with_pipe(
        id,
        |p, out| {
            p.writers = p.writers.saturating_sub(1);
            if p.writers == 0 && p.chunks.is_empty() {
                if let Some(reader) = p.reader_waiting.take() {
                    out.push(Out::Eof(reader));
                }
            }
        },
        Out::Reply(0, OK),
    )
}

fn close_read(id: u64) -> Vec<Out> {
    with_pipe(
        id,
        |p, out| {
            p.reader = false;
            p.reader_waiting = None;
            for (b, _) in p.chunks.drain(..) {
                out.push(Out::Free(b));
            }
            for (t, b, _) in p.writers_waiting.drain(..) {
                out.push(Out::Free(b));
                out.push(Out::Reply(t, CLOSED));
            }
        },
        Out::Reply(0, OK),
    )
}
