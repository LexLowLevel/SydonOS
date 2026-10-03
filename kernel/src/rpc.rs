use crate::fabric::{self, Msg};
use crate::sync::IrqCell;
use crate::{clock, cpu, director, smp, task, user};
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub const PAYLOAD: usize = 40;

pub const SVC_CORE: u16 = 0;
pub const SVC_DIRECTOR: u16 = 1;
const FIRST_USER_SVC: u16 = 16;

// reply status, the same numbers user space sees
pub const OK: u8 = 0;
pub const NO_SERVICE: u8 = 1;
pub const TIMEOUT: u8 = 2;
pub const NOT_FOUND: u8 = 3;
pub const EXISTS: u8 = 4;
pub const INVALID: u8 = 5;
pub const DOWN: u8 = 6;
pub const NO_MEMORY: u8 = 7;
pub const BUSY: u8 = 8;

pub const ONEWAY: u8 = 1;
// only kernels set this, SYS_SUBMIT never does
const FROM_KERNEL: u8 = 4;

const REQUEST: u8 = 1;
const REPLY: u8 = 2;

pub const CORE_LOAD: u16 = 1;
pub const CORE_HANG: u16 = 2;
pub const CORE_PEER_DOWN: u16 = 3;
pub const CORE_KILL: u16 = 5;

pub const DEFAULT_TIMEOUT: u64 = clock::ms(3000);
pub const NEVER: u64 = u64::MAX;
const HEARTBEAT_TICKS: u64 = clock::ms(200);
const BATCH: usize = 64;
const RING_BATCH: usize = 32;
const SPIN_NS: u64 = 20_000;
// a job past these gets BUSY instead of eating the kernel heap
const OPEN_MAX: u16 = 256;
const QUEUE_MAX: usize = 1024;
const SERVICES_MAX: usize = 8;
// the low bits of a request id pick its slot, the rest keeps a late reply
// from matching whoever has the slot now
const SLOT_BITS: u32 = 16;
const NO_SLOT: u64 = (1 << SLOT_BITS) - 1;

#[derive(Clone, Copy)]
pub struct Body {
    pub data: [u8; PAYLOAD],
    pub len: u8,
}

impl Body {
    pub const EMPTY: Body = Body { data: [0; PAYLOAD], len: 0 };

    pub fn bytes(b: &[u8]) -> Body {
        let mut body = Body::EMPTY;
        let n = b.len().min(PAYLOAD);
        body.data[..n].copy_from_slice(&b[..n]);
        body.len = n as u8;
        body
    }

    pub fn words(w: &[u64]) -> Body {
        let mut body = Body::EMPTY;
        for (i, v) in w.iter().enumerate() {
            body.set_word(i, *v);
        }
        body
    }

    pub fn word(&self, i: usize) -> u64 {
        u64::from_le_bytes(self.data[i * 8..i * 8 + 8].try_into().unwrap())
    }

    pub fn set_word(&mut self, i: usize, v: u64) {
        self.data[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
        self.len = self.len.max((i * 8 + 8) as u8);
    }

    pub fn name_at(&self, at: usize, max: usize) -> &[u8] {
        let field = &self.data[at..at + max];
        let n = field.iter().position(|&b| b == 0).unwrap_or(max);
        &field[..n]
    }

    pub fn set_name(&mut self, at: usize, max: usize, name: &[u8]) {
        let n = name.len().min(max);
        self.data[at..at + n].copy_from_slice(&name[..n]);
        self.data[at + n..at + max].fill(0);
        self.len = self.len.max((at + max) as u8);
    }
}

#[derive(Clone, Copy)]
pub struct Packet {
    kind: u8,
    flags: u8,
    pub status: u8,
    service: u16,
    op: u16,
    id: u64,
    pub body: Body,
}

impl Packet {
    pub fn status_only(status: u8) -> Packet {
        Packet { kind: REPLY, flags: 0, status, service: 0, op: 0, id: 0, body: Body::EMPTY }
    }

    fn encode(&self) -> Msg {
        let mut m = [0u64; 7];
        m[0] = self.kind as u64
            | (self.flags as u64) << 8
            | (self.body.len as u64) << 16
            | (self.status as u64) << 24
            | (self.service as u64) << 32
            | (self.op as u64) << 48;
        m[1] = self.id;
        for i in 0..5 {
            m[2 + i] = self.body.word(i);
        }
        Msg(m)
    }

    fn decode(m: &Msg) -> Packet {
        let h = m.0[0];
        let mut body = Body::EMPTY;
        for i in 0..5 {
            body.data[i * 8..i * 8 + 8].copy_from_slice(&m.0[2 + i].to_le_bytes());
        }
        body.len = ((h >> 16) as u8).min(PAYLOAD as u8);
        Packet {
            kind: h as u8,
            flags: (h >> 8) as u8,
            status: (h >> 24) as u8,
            service: (h >> 32) as u16,
            op: (h >> 48) as u16,
            id: m.0[1],
            body,
        }
    }
}

pub fn handle(core: usize, svc: u16) -> u64 {
    (core as u64) << 16 | svc as u64
}

pub fn handle_core(h: u64) -> usize {
    (h >> 16) as usize
}

fn handle_svc(h: u64) -> u16 {
    h as u16
}

pub fn token_of(src: usize, id: u64) -> u64 {
    (src as u64) << 48 | id
}

pub fn token_core(t: u64) -> usize {
    (t >> 48) as usize
}

pub struct Request {
    pub token: u64,
    pub op: u16,
    pub flags: u8,
    pub body: Body,
}

impl Request {
    pub fn from_kernel(&self) -> bool {
        self.flags & FROM_KERNEL != 0
    }
}

pub enum Answer {
    Reply(u8, Body),
    Later,
}

pub type KernelHandler = fn(&Request) -> Answer;

pub enum Then {
    Task(usize),
    Call(fn(u64, &Packet), u64),
}

pub enum Taken {
    Done(Packet),
    Pending,
    Unknown,
}

struct Pending {
    id: u64,
    handle: u64,
    then: Then,
    deadline: u64,
    reply: Option<Packet>,
    waiting: bool,
}

enum Handler {
    Kernel(KernelHandler),
    User { task: usize, queue: VecDeque<(u64, u16, Body)>, open: Vec<u64>, waiting: bool },
}

struct Service {
    id: u16,
    handler: Handler,
}

struct Rpc {
    me: usize,
    cores: usize,
    next_id: u64,
    next_svc: u16,
    pending: Vec<Option<Pending>>,
    free_slots: Vec<usize>,
    open: [u16; task::MAX_TASKS],
    soonest: u64,
    services: Vec<Service>,
    local: VecDeque<Msg>,
    outbox: VecDeque<(usize, Msg)>,
    cache: Vec<([u8; 32], u64)>,
    down: u64,
    next_beat: u64,
    dirty: u64,
    batching: u32,
}

// per core like every static here, so no other core ever takes it
static RPC: IrqCell<Option<Rpc>> = IrqCell::new(None);
static EXEC: AtomicUsize = AtomicUsize::new(usize::MAX);
static WAKE_AT: AtomicU64 = AtomicU64::new(u64::MAX);
static ACTIVE_UNTIL: AtomicU64 = AtomicU64::new(0);

fn with<R>(f: impl FnOnce(&mut Rpc) -> R) -> R {
    RPC.with(|r| f(r.as_mut().expect("rpc: not initialised")))
}

pub fn init() {
    let rpc = Some(Rpc {
        me: smp::core(),
        cores: fabric::cores(),
        // a restarted kernel must not reuse ids a late reply could still carry
        next_id: cpu::rdtsc() & 0xFFFF_FFFF,
        next_svc: FIRST_USER_SVC,
        pending: Vec::new(),
        free_slots: Vec::new(),
        open: [0; task::MAX_TASKS],
        soonest: u64::MAX,
        services: Vec::new(),
        local: VecDeque::new(),
        outbox: VecDeque::new(),
        cache: Vec::new(),
        down: 0,
        next_beat: 0,
        dirty: 0,
        batching: 0,
    });
    RPC.with(|r| *r = rpc);
    register_kernel(SVC_CORE, core_service);
}

pub fn register_kernel(id: u16, f: KernelHandler) {
    with(|r| r.services.push(Service { id, handler: Handler::Kernel(f) }));
}

pub fn wake_executor() {
    let e = EXEC.load(Ordering::Relaxed);
    if e != usize::MAX {
        task::wake(e);
    }
}

impl Rpc {
    fn is_down(&self, core: usize) -> bool {
        core < 64 && self.down & (1 << core) != 0
    }

    // a full ring sends later messages to that core through the outbox too, so
    // they keep their order
    fn send(&mut self, dst: usize, pkt: &Packet) {
        let msg = pkt.encode();
        if dst >= self.cores {
            return;
        }
        if dst == self.me {
            self.local.push_back(msg);
            wake_executor();
            return;
        }
        if self.is_down(dst) {
            return;
        }
        let queued = self.outbox.iter().any(|&(d, _)| d == dst);
        if !queued && fabric::try_push(dst, &msg).is_ok() {
            self.dirty |= 1 << dst;
            return;
        }
        self.outbox.push_back((dst, msg));
        wake_executor();
    }

    fn ring(&mut self) {
        if self.batching == 0 && self.dirty != 0 {
            fabric::doorbells(core::mem::take(&mut self.dirty));
        }
    }

    fn submit(&mut self, h: u64, op: u16, body: Body, flags: u8, then: Option<(Then, u64)>) -> Result<u64, u8> {
        let dst = handle_core(h);
        if dst >= self.cores {
            return Err(INVALID);
        }
        if self.is_down(dst) {
            return Err(DOWN);
        }
        let slot = match then {
            Some(_) => self.free_slots.pop().or_else(|| (self.pending.len() < NO_SLOT as usize).then(|| {
                self.pending.push(None);
                self.pending.len() - 1
            })).ok_or(BUSY)?,
            None => NO_SLOT as usize,
        };
        let id = self.next_id << SLOT_BITS | slot as u64;
        // tokens keep 48 bits of the id
        self.next_id = (self.next_id + 1) & 0xFFFF_FFFF;
        if let Some((then, timeout)) = then {
            let deadline = task::ticks().saturating_add(timeout);
            if let Then::Task(t) = then {
                self.open[t] += 1;
            }
            self.soonest = self.soonest.min(deadline);
            self.pending[slot] = Some(Pending { id, handle: h, then, deadline, reply: None, waiting: false });
        }
        let pkt = Packet { kind: REQUEST, flags, status: OK, service: handle_svc(h), op, id, body };
        self.send(dst, &pkt);
        Ok(id)
    }

    fn slot_of(&self, id: u64) -> Option<usize> {
        let i = (id & NO_SLOT) as usize;
        self.pending.get(i)?.as_ref().filter(|p| p.id == id).map(|_| i)
    }

    fn remove(&mut self, i: usize) -> Pending {
        let p = self.pending[i].take().unwrap();
        self.free_slots.push(i);
        if let Then::Task(t) = p.then {
            self.open[t] -= 1;
        }
        p
    }

    fn at(&mut self, i: usize) -> &mut Pending {
        self.pending[i].as_mut().unwrap()
    }

    fn evict(&mut self, h: u64) {
        self.cache.retain(|&(_, c)| c != h);
    }
}

pub fn submit(h: u64, op: u16, body: Body, then: Then) -> Result<u64, u8> {
    submit_timeout(h, op, body, then, DEFAULT_TIMEOUT)
}

pub fn submit_timeout(h: u64, op: u16, body: Body, then: Then, ticks: u64) -> Result<u64, u8> {
    with(|r| {
        let id = r.submit(h, op, body, FROM_KERNEL, Some((then, ticks)));
        r.ring();
        id
    })
}

pub fn submit_user(h: u64, op: u16, body: Body, then: Then, ticks: u64) -> Result<u64, u8> {
    with(|r| {
        if matches!(then, Then::Task(t) if r.open[t] >= OPEN_MAX) {
            return Err(BUSY);
        }
        let id = r.submit(h, op, body, 0, Some((then, ticks)));
        r.ring();
        id
    })
}

pub fn oneway(h: u64, op: u16, body: Body) {
    with(|r| {
        let _ = r.submit(h, op, body, ONEWAY | FROM_KERNEL, None);
        r.ring();
    });
}

pub fn batch<R>(f: impl FnOnce() -> R) -> R {
    with(|r| r.batching += 1);
    let out = f();
    with(|r| {
        r.batching -= 1;
        r.ring();
    });
    out
}

pub fn reply(token: u64, status: u8, body: Body) {
    if token == 0 {
        return;
    }
    let pkt = Packet { kind: REPLY, flags: 0, status, service: 0, op: 0, id: token & 0xFFFF_FFFF_FFFF, body };
    with(|r| {
        r.send(token_core(token), &pkt);
        r.ring();
    });
}

pub fn take(id: u64, task: usize) -> Taken {
    with(|r| {
        let Some(i) = r.slot_of(id).filter(|&i| matches!(r.at(i).then, Then::Task(t) if t == task)) else {
            return Taken::Unknown;
        };
        match r.at(i).reply {
            Some(pkt) => {
                let h = r.remove(i).handle;
                if matches!(pkt.status, NO_SERVICE | TIMEOUT | DOWN) {
                    r.evict(h);
                }
                Taken::Done(pkt)
            }
            None => {
                r.at(i).waiting = true;
                Taken::Pending
            }
        }
    })
}

// call with interrupts off
fn wait_until<T>(mut check: impl FnMut() -> Option<T>) -> T {
    let spin_until = clock::now_ns() + SPIN_NS;
    let mut polling = fabric::start_polling();
    let out = loop {
        if let Some(v) = check() {
            break v;
        }
        if polling && clock::now_ns() < spin_until && !task::others_ready() {
            if pump() == 0 {
                core::hint::spin_loop();
            }
            continue;
        }
        // this task stops talking rpc for now. whatever runs next may never
        // make a syscall, so the doorbell has to be on.
        ACTIVE_UNTIL.store(0, Ordering::Relaxed);
        fabric::stop_polling();
        polling = false;
        task::block_current();
    };
    if polling && !active() {
        fabric::stop_polling();
    }
    out
}

pub fn wait(id: u64) -> Option<Packet> {
    let flags = cpu::push_cli();
    let me = task::current();
    let out = wait_until(|| match take(id, me) {
        Taken::Done(p) => Some(Some(p)),
        Taken::Unknown => Some(None),
        Taken::Pending if task::killed() => Some(None),
        Taken::Pending => None,
    });
    cpu::pop_flags(flags);
    out
}

pub fn call(h: u64, op: u16, body: Body) -> Packet {
    match submit(h, op, body, Then::Task(task::current())) {
        Ok(id) => wait(id).unwrap_or(Packet::status_only(INVALID)),
        Err(status) => Packet::status_only(status),
    }
}

// callbacks are returned, not called, so they run without the lock
fn deliver(r: &mut Rpc, i: usize, pkt: Packet) -> Option<(fn(u64, &Packet), u64)> {
    match r.at(i).then {
        Then::Task(t) => {
            r.at(i).reply = Some(pkt);
            if r.at(i).waiting {
                task::wake(t);
            }
            None
        }
        Then::Call(f, ctx) => {
            r.remove(i);
            Some((f, ctx))
        }
    }
}

// a reply must come from the core the request went to
fn complete(src: usize, id: u64, pkt: Packet) {
    let call = with(|r| {
        let i = r.slot_of(id).filter(|&i| r.at(i).reply.is_none() && handle_core(r.at(i).handle) == src)?;
        deliver(r, i, pkt)
    });
    if let Some((f, ctx)) = call {
        f(ctx, &pkt);
    }
}

fn fail_where(status: u8, pred: impl Fn(&Pending) -> bool) {
    let pkt = Packet::status_only(status);
    let calls: Vec<_> = with(|r| {
        let mut calls = Vec::new();
        for i in 0..r.pending.len() {
            if r.pending[i].as_ref().is_some_and(|p| p.reply.is_none() && pred(p)) {
                calls.extend(deliver(r, i, pkt));
            }
        }
        calls
    });
    for (f, ctx) in calls {
        f(ctx, &pkt);
    }
}

pub fn peer_down(core: usize) {
    with(|r| {
        if core < 64 {
            r.down |= 1 << core;
        }
        r.outbox.retain(|&(d, _)| d != core);
        r.cache.retain(|&(_, h)| handle_core(h) != core);
        for s in r.services.iter_mut() {
            if let Handler::User { queue, open, .. } = &mut s.handler {
                queue.retain(|q| q.0 == 0 || token_core(q.0) != core);
                open.retain(|&t| token_core(t) != core);
            }
        }
    });
    fail_where(DOWN, |p| handle_core(p.handle) == core);
}

pub fn lookup(name: &[u8]) -> Result<u64, u8> {
    if name.is_empty() || name.len() > 32 {
        return Err(INVALID);
    }
    let mut key = [0u8; 32];
    key[..name.len()].copy_from_slice(name);
    if let Some(h) = with(|r| r.cache.iter().find(|(k, _)| *k == key).map(|&(_, h)| h)) {
        return Ok(h);
    }
    let mut body = Body::EMPTY;
    body.set_name(0, 32, name);
    let p = call(handle(0, SVC_DIRECTOR), director::OP_LOOKUP, body);
    if p.status != OK {
        return Err(p.status);
    }
    let h = p.body.word(0);
    with(|r| r.cache.push((key, h)));
    Ok(h)
}

pub fn register_user(task: usize, name: &[u8]) -> Result<u64, u8> {
    if name.is_empty() || name.len() > 32 {
        return Err(INVALID);
    }
    let (me, id) = with(|r| {
        let mine = r.services.iter().filter(|s| matches!(s.handler, Handler::User { task: t, .. } if t == task)).count();
        if mine >= SERVICES_MAX {
            return Err(BUSY);
        }
        let id = r.next_svc;
        r.next_svc += 1;
        let handler = Handler::User { task, queue: VecDeque::new(), open: Vec::new(), waiting: false };
        r.services.push(Service { id, handler });
        Ok((r.me, id))
    })?;
    let h = handle(me, id);
    let mut body = Body::words(&[h]);
    body.set_name(8, 32, name);
    let p = call(handle(0, SVC_DIRECTOR), director::OP_REGISTER, body);
    if p.status != OK {
        with(|r| r.services.retain(|s| s.id != id));
        return Err(p.status);
    }
    Ok(h)
}

pub fn svc_recv(svc: u16, task: usize) -> Result<(u64, u16, Body), u8> {
    let flags = cpu::push_cli();
    let mut dropped = None;
    let out = wait_until(|| {
        let got = with(|r| {
            let s = r.services.iter_mut().find(|s| s.id == svc).ok_or(INVALID)?;
            match &mut s.handler {
                Handler::User { task: t, queue, open, waiting } if *t == task => match queue.pop_front() {
                    Some(req) => {
                        // a job that never answers would grow this forever
                        if open.len() >= QUEUE_MAX {
                            dropped = Some(open.remove(0));
                        }
                        if req.0 != 0 {
                            open.push(req.0);
                        }
                        Ok(Some(req))
                    }
                    None => {
                        *waiting = true;
                        Ok(None)
                    }
                },
                _ => Err(INVALID),
            }
        });
        match got {
            Ok(Some(req)) => Some(Ok(req)),
            Ok(None) if task::killed() => Some(Err(INVALID)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    });
    cpu::pop_flags(flags);
    if let Some(token) = dropped {
        reply(token, TIMEOUT, Body::EMPTY);
    }
    out
}

pub fn svc_reply(task: usize, token: u64, status: u8, body: Body) -> Result<(), u8> {
    let owned = with(|r| {
        r.services.iter_mut().any(|s| match &mut s.handler {
            Handler::User { task: t, open, .. } if *t == task => {
                let before = open.len();
                open.retain(|&o| o != token);
                open.len() != before
            }
            _ => false,
        })
    });
    if !owned {
        return Err(INVALID);
    }
    reply(token, status, body);
    Ok(())
}

// owed requests get NO_SERVICE. returns the task's service handles.
pub fn drop_task(task: usize) -> Vec<u64> {
    let (gone, owed) = with(|r| {
        for i in 0..r.pending.len() {
            if r.pending[i].as_ref().is_some_and(|p| matches!(p.then, Then::Task(t) if t == task)) {
                r.remove(i);
            }
        }
        let me = r.me;
        let mut gone = Vec::new();
        let mut owed = Vec::new();
        r.services.retain(|s| match &s.handler {
            Handler::User { task: t, queue, open, .. } if *t == task => {
                gone.push(handle(me, s.id));
                owed.extend(open.iter().copied());
                owed.extend(queue.iter().map(|q| q.0));
                false
            }
            _ => true,
        });
        (gone, owed)
    });
    batch(|| {
        for token in owed {
            reply(token, NO_SERVICE, Body::EMPTY);
        }
    });
    gone
}

fn dispatch(src: usize, msg: &Msg) {
    let pkt = Packet::decode(msg);
    match pkt.kind {
        REPLY => complete(src, pkt.id, pkt),
        REQUEST => serve(src, pkt),
        _ => {}
    }
}

fn serve(src: usize, pkt: Packet) {
    let oneway = pkt.flags & ONEWAY != 0;
    let req = Request {
        token: if oneway { 0 } else { token_of(src, pkt.id) },
        op: pkt.op,
        flags: pkt.flags,
        body: pkt.body,
    };
    let kernel = with(|r| {
        let Some(s) = r.services.iter_mut().find(|s| s.id == pkt.service) else {
            return Err(NO_SERVICE);
        };
        match &mut s.handler {
            Handler::Kernel(f) => Ok(Some(*f)),
            Handler::User { queue, .. } if queue.len() >= QUEUE_MAX => Err(BUSY),
            Handler::User { task, queue, waiting, .. } => {
                queue.push_back((req.token, req.op, req.body));
                if *waiting {
                    *waiting = false;
                    task::wake(*task);
                }
                Ok(None)
            }
        }
    });
    match kernel {
        Ok(Some(f)) => {
            if let Answer::Reply(status, body) = f(&req) {
                reply(req.token, status, body);
            }
        }
        Ok(None) => {}
        Err(status) => reply(req.token, status, Body::EMPTY),
    }
}

fn core_service(req: &Request) -> Answer {
    if !req.from_kernel() {
        return Answer::Reply(INVALID, Body::EMPTY);
    }
    match req.op {
        CORE_LOAD => user::load_job(req.body.word(0), req.body.name_at(8, 32)),
        CORE_HANG => {
            println!("cpu {}: told to hang, stopping", smp::core());
            cpu::cli();
            cpu::hlt_loop();
        }
        CORE_PEER_DOWN => {
            peer_down(req.body.word(0) as usize);
            Answer::Reply(OK, Body::EMPTY)
        }
        CORE_KILL => match user::kill(req.body.word(0)) {
            true => Answer::Reply(OK, Body::EMPTY),
            false => Answer::Reply(NOT_FOUND, Body::EMPTY),
        },
        _ => Answer::Reply(INVALID, Body::EMPTY),
    }
}

// keeps per-destination order: nothing passes a queued message to the same core
fn flush_outbox() {
    with(|r| {
        let mut blocked = 0u64;
        let mut i = 0;
        while i < r.outbox.len() {
            let (dst, msg) = r.outbox[i];
            let bit = 1u64 << (dst % 64);
            if blocked & bit == 0 && fabric::try_push(dst, &msg).is_ok() {
                r.outbox.remove(i);
                r.dirty |= bit;
            } else {
                blocked |= bit;
                i += 1;
            }
        }
    });
}


// bumped by the executor, so a stuck executor stops the count
fn heartbeat(now: u64) {
    let due = with(|r| {
        if r.me == 0 || now < r.next_beat {
            return false;
        }
        r.next_beat = now + HEARTBEAT_TICKS;
        true
    });
    if due {
        fabric::beat(crate::frame::total_free() * 4);
    }
}

fn expire(now: u64) {
    if now < with(|r| r.soonest) {
        return;
    }
    fail_where(TIMEOUT, |p| p.deadline <= now);
    with(|r| {
        r.soonest = r.pending.iter().flatten().filter(|p| p.reply.is_none()).map(|p| p.deadline).min().unwrap_or(u64::MAX);
    });
}

fn next_wake(now: u64) -> u64 {
    with(|r| {
        let mut at = if r.me == 0 { now + HEARTBEAT_TICKS } else { r.next_beat.max(now + 1) };
        if !r.outbox.is_empty() {
            at = now + 1;
        }
        at.min(r.soonest)
    })
}

// interrupts stay off from the queue check until the task blocks
fn sleep(until: u64) {
    let flags = cpu::push_cli();
    let busy = with(|r| !r.local.is_empty());
    if !busy {
        WAKE_AT.store(until, Ordering::Relaxed);
        fabric::wait(!active());
        WAKE_AT.store(u64::MAX, Ordering::Relaxed);
    }
    cpu::pop_flags(flags);
}

pub fn on_tick() {
    if task::ticks() + 1 >= WAKE_AT.load(Ordering::Relaxed) {
        wake_executor();
    }
    if !active() {
        fabric::stop_polling();
    }
}

fn active() -> bool {
    task::ticks() < ACTIVE_UNTIL.load(Ordering::Relaxed)
}

// a job that talks rpc will pick up its messages in its next syscall, so
// the doorbell stays off until it has been quiet for a tick
pub fn syscall_active() {
    ACTIVE_UNTIL.store(task::ticks() + 2, Ordering::Relaxed);
    fabric::start_polling();
    if fabric::pending() {
        pump();
    }
}

fn pump() -> usize {
    batch(|| {
        flush_outbox();
        let mut handled = 0;
        let mut incoming = [(0, Msg([0; 7])); RING_BATCH];
        while handled < BATCH {
            if let Some((me, msg)) = with(|r| r.local.pop_front().map(|m| (r.me, m))) {
                dispatch(me, &msg);
                handled += 1;
                continue;
            }
            let room = (BATCH - handled).min(RING_BATCH);
            let n = fabric::poll_many(&mut incoming[..room]);
            if n == 0 {
                break;
            }
            for (src, msg) in &incoming[..n] {
                dispatch(*src, msg);
            }
            handled += n;
        }
        handled
    })
}

fn work_waiting() -> bool {
    with(|r| !r.local.is_empty()) || fabric::pending()
}

// not while a job is ready: jobs never preempt the executor
fn spin_for_work() -> bool {
    let until = clock::now_ns() + SPIN_NS;
    while clock::now_ns() < until && !task::others_ready() {
        if work_waiting() {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

pub fn executor() -> ! {
    EXEC.store(task::current(), Ordering::Relaxed);
    let core0 = smp::core() == 0;
    loop {
        let now = task::ticks();
        let handled = pump();
        task::reap_finished();
        expire(now);
        heartbeat(now);
        if core0 {
            director::tick(now);
        }
        if handled == BATCH || (handled > 0 && spin_for_work()) {
            continue;
        }
        sleep(next_wake(now));
    }
}
