use crate::rpc::{self, Answer, Body, Packet, Request, Then, DOWN, EXISTS, INVALID, NOT_FOUND, OK};
use crate::sync::SpinLock;
use crate::{fabric, jobs, task};
use alloc::vec::Vec;

pub const OP_REGISTER: u16 = 1;
pub const OP_UNREGISTER: u16 = 2;
pub const OP_LOOKUP: u16 = 3;
pub const OP_SERVICES: u16 = 4;
pub const OP_SPAWN: u16 = 5;
pub const OP_WAIT: u16 = 6;
pub const OP_PS: u16 = 7;
pub const OP_EXITED: u16 = 8;
pub const OP_CORES: u16 = 10;
pub const OP_HANG: u16 = 11;
pub const OP_KILL: u16 = 14;

pub const ANY_CORE: u64 = u64::MAX;

const DOWN_TICKS: u64 = crate::clock::ms(1000);
const CHECK_TICKS: u64 = crate::clock::ms(100);
const KEEP_EXITED: usize = 32;

#[derive(Clone, Copy, PartialEq)]
enum State {
    Starting,
    Running,
    Exited(u64),
    Failed,
    Lost,
}

impl State {
    fn code(self) -> u64 {
        match self {
            State::Starting => 0,
            State::Running => 1,
            State::Exited(_) => 2,
            State::Failed => 3,
            State::Lost => 4,
        }
    }
}

struct Entry {
    name: [u8; 32],
    handle: u64,
}

struct Job {
    pid: u64,
    name: [u8; 16],
    core: usize,
    state: State,
    spawn_token: u64,
}

struct CoreInfo {
    up: bool,
    last_beat: u64,
    last_count: u64,
    jobs: u64,
}

struct Director {
    registry: Vec<Entry>,
    jobs: Vec<Job>,
    cores: Vec<CoreInfo>,
    waiters: Vec<(u64, u64)>,
    next_pid: u64,
    next_check: u64,
}

static DIRECTOR: SpinLock<Option<Director>> = SpinLock::new(None);

fn with<R>(f: impl FnOnce(&mut Director) -> R) -> R {
    f(DIRECTOR.lock().as_mut().expect("director: not initialised"))
}

fn name32(n: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 32];
    let len = n.len().min(32);
    k[..len].copy_from_slice(&n[..len]);
    k
}

fn trimmed(n: &[u8]) -> &[u8] {
    &n[..n.iter().position(|&b| b == 0).unwrap_or(n.len())]
}

pub fn init(cores: usize) {
    let now = task::ticks();
    let d = Director {
        registry: Vec::new(),
        jobs: Vec::new(),
        cores: (0..cores).map(|_| CoreInfo { up: true, last_beat: now, last_count: 0, jobs: 0 }).collect(),
        waiters: Vec::new(),
        next_pid: 1,
        next_check: 0,
    };
    *DIRECTOR.lock() = Some(d);
    rpc::register_kernel(rpc::SVC_DIRECTOR, handler);
    with(|d| d.registry.push(Entry { name: name32(b"director"), handle: rpc::handle(0, rpc::SVC_DIRECTOR) }));
}

impl Director {
    fn place(&self, needs: &str, requested: u64) -> Result<usize, u8> {
        if requested != ANY_CORE {
            let c = requested as usize;
            return match self.cores.get(c) {
                Some(info) if info.up => Ok(c),
                Some(_) => Err(DOWN),
                None => Err(INVALID),
            };
        }
        let key = name32(needs.as_bytes());
        if let Some(e) = self.registry.iter().find(|e| !needs.is_empty() && e.name == key) {
            return Ok(rpc::handle_core(e.handle));
        }
        // core 0 counts one extra job for the director
        (0..self.cores.len())
            .filter(|&c| self.cores[c].up)
            .min_by_key(|&c| (self.cores[c].jobs + (c == 0) as u64, c))
            .ok_or(DOWN)
    }

    fn finish_job(&mut self, pid: u64, state: State) -> Vec<u64> {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.pid == pid) {
            if matches!(job.state, State::Starting | State::Running) {
                self.cores[job.core].jobs -= 1;
            }
            job.state = state;
        }
        let (done, rest): (Vec<_>, Vec<_>) = self.waiters.drain(..).partition(|&(p, _)| p == pid);
        self.waiters = rest;
        let finished = self.jobs.iter().filter(|j| !matches!(j.state, State::Starting | State::Running)).count();
        if finished > KEEP_EXITED {
            if let Some(i) = self.jobs.iter().position(|j| !matches!(j.state, State::Starting | State::Running)) {
                self.jobs.remove(i);
            }
        }
        done.into_iter().map(|(_, token)| token).collect()
    }
}

fn wait_answer(state: State) -> (u8, Body) {
    match state {
        State::Exited(code) => (OK, Body::words(&[code])),
        State::Lost => (DOWN, Body::EMPTY),
        _ => (INVALID, Body::EMPTY),
    }
}

// token 0 means nobody gets a reply
pub fn spawn(name: &[u8], requested: u64, token: u64) -> Result<(), u8> {
    let job = jobs::builtin(name).ok_or(NOT_FOUND)?;
    let (pid, core) = with(|d| {
        let core = d.place(job.needs, requested)?;
        let pid = d.next_pid;
        d.next_pid += 1;
        let mut short = [0u8; 16];
        let n = name.len().min(16);
        short[..n].copy_from_slice(&name[..n]);
        d.jobs.push(Job { pid, name: short, core, state: State::Starting, spawn_token: token });
        d.cores[core].jobs += 1;
        Ok::<_, u8>((pid, core))
    })?;
    let mut body = Body::words(&[pid]);
    body.set_name(8, 32, name);
    if let Err(status) = rpc::submit(rpc::handle(core, rpc::SVC_CORE), rpc::CORE_LOAD, body, Then::Call(loaded, pid)) {
        with(|d| d.finish_job(pid, State::Failed));
        return Err(status);
    }
    Ok(())
}

fn loaded(pid: u64, pkt: &Packet) {
    let (token, core, waiters) = with(|d| {
        let job = d.jobs.iter_mut().find(|j| j.pid == pid)?;
        let (token, core) = (job.spawn_token, job.core);
        if pkt.status == OK {
            if job.state == State::Starting {
                job.state = State::Running;
            }
            Some((token, core, Vec::new()))
        } else {
            Some((token, core, d.finish_job(pid, State::Failed)))
        }
    })
    .unwrap_or((0, 0, Vec::new()));
    if pkt.status == OK {
        rpc::reply(token, OK, Body::words(&[pid, core as u64]));
    } else {
        rpc::reply(token, pkt.status, Body::EMPTY);
    }
    for t in waiters {
        rpc::reply(t, pkt.status, Body::EMPTY);
    }
}

// no job is trusted with these yet
fn kernel_only(op: u16) -> bool {
    matches!(op, OP_REGISTER | OP_UNREGISTER | OP_EXITED | OP_HANG | OP_KILL)
}

pub fn kill(pid: u64) -> u8 {
    let core = with(|d| d.jobs.iter().find(|j| j.pid == pid && matches!(j.state, State::Starting | State::Running)).map(|j| j.core));
    match core {
        None => NOT_FOUND,
        Some(core) => {
            rpc::oneway(rpc::handle(core, rpc::SVC_CORE), rpc::CORE_KILL, Body::words(&[pid]));
            OK
        }
    }
}

fn handler(req: &Request) -> Answer {
    let b = &req.body;
    if kernel_only(req.op) && !req.from_kernel() {
        return Answer::Reply(INVALID, Body::EMPTY);
    }
    match req.op {
        OP_REGISTER => {
            let key = name32(b.name_at(8, 32));
            with(|d| {
                if d.registry.iter().any(|e| e.name == key) {
                    return Answer::Reply(EXISTS, Body::EMPTY);
                }
                d.registry.push(Entry { name: key, handle: b.word(0) });
                Answer::Reply(OK, Body::EMPTY)
            })
        }
        OP_UNREGISTER => {
            with(|d| d.registry.retain(|e| e.handle != b.word(0)));
            Answer::Reply(OK, Body::EMPTY)
        }
        OP_LOOKUP => {
            let key = name32(b.name_at(0, 32));
            with(|d| match d.registry.iter().find(|e| e.name == key) {
                Some(e) => Answer::Reply(OK, Body::words(&[e.handle])),
                None => Answer::Reply(NOT_FOUND, Body::EMPTY),
            })
        }
        OP_SERVICES => with(|d| match d.registry.get(b.word(0) as usize) {
            Some(e) => {
                let mut out = Body::words(&[e.handle]);
                out.set_name(8, 32, trimmed(&e.name));
                Answer::Reply(OK, out)
            }
            None => Answer::Reply(NOT_FOUND, Body::EMPTY),
        }),
        OP_SPAWN => match spawn(b.name_at(8, 32), b.word(0), req.token) {
            Ok(()) => Answer::Later,
            Err(status) => Answer::Reply(status, Body::EMPTY),
        },
        OP_WAIT => {
            let pid = b.word(0);
            with(|d| match d.jobs.iter().find(|j| j.pid == pid) {
                None => Answer::Reply(NOT_FOUND, Body::EMPTY),
                Some(j) if matches!(j.state, State::Starting | State::Running) => {
                    d.waiters.push((pid, req.token));
                    Answer::Later
                }
                Some(j) => {
                    let (status, body) = wait_answer(j.state);
                    Answer::Reply(status, body)
                }
            })
        }
        OP_PS => with(|d| match d.jobs.get(b.word(0) as usize) {
            Some(j) => {
                let code = if let State::Exited(c) = j.state { c } else { 0 };
                let mut out = Body::words(&[j.pid, j.core as u64 | j.state.code() << 16 | code << 32]);
                out.set_name(16, 16, trimmed(&j.name));
                Answer::Reply(OK, out)
            }
            None => Answer::Reply(NOT_FOUND, Body::EMPTY),
        }),
        OP_EXITED => {
            let (pid, code) = (b.word(0), b.word(1));
            let waiters = with(|d| d.finish_job(pid, State::Exited(code)));
            for t in waiters {
                rpc::reply(t, OK, Body::words(&[code]));
            }
            Answer::Reply(OK, Body::EMPTY)
        }
        OP_CORES => {
            let now = task::ticks();
            with(|d| match d.cores.get(b.word(0) as usize) {
                Some(c) => {
                    let local = b.word(0) == 0;
                    let age = if local { 0 } else { now.saturating_sub(c.last_beat) * crate::clock::TICK_US / 1000 };
                    let free = if local { crate::frame::total_free() * 4 } else { fabric::beat_of(b.word(0) as usize).1 };
                    Answer::Reply(OK, Body::words(&[c.up as u64, c.jobs, age, free]))
                }
                None => Answer::Reply(NOT_FOUND, Body::EMPTY),
            })
        }
        OP_HANG => {
            let core = b.word(0) as usize;
            if core == 0 || core >= with(|d| d.cores.len()) {
                return Answer::Reply(INVALID, Body::EMPTY);
            }
            rpc::oneway(rpc::handle(core, rpc::SVC_CORE), rpc::CORE_HANG, Body::EMPTY);
            Answer::Reply(OK, Body::EMPTY)
        }
        OP_KILL => Answer::Reply(kill(b.word(0)), Body::EMPTY),
        _ => Answer::Reply(INVALID, Body::EMPTY),
    }
}

pub fn tick(now: u64) {
    let silent: Vec<usize> = with(|d| {
        if now < d.next_check {
            return Vec::new();
        }
        d.next_check = now + CHECK_TICKS;
        let mut silent = Vec::new();
        for (core, c) in d.cores.iter_mut().enumerate().skip(1).filter(|(_, c)| c.up) {
            let (count, _) = fabric::beat_of(core);
            if count != c.last_count {
                c.last_count = count;
                c.last_beat = now;
            } else if now.saturating_sub(c.last_beat) > DOWN_TICKS {
                silent.push(core);
            }
        }
        silent
    });
    for c in silent {
        mark_down(c);
    }
}

fn mark_down(core: usize) {
    let (waiters, others) = with(|d| {
        d.cores[core].up = false;
        d.registry.retain(|e| rpc::handle_core(e.handle) != core);
        let lost: Vec<u64> = d
            .jobs
            .iter()
            .filter(|j| j.core == core && matches!(j.state, State::Starting | State::Running))
            .map(|j| j.pid)
            .collect();
        let mut waiters = Vec::new();
        for pid in lost {
            waiters.extend(d.finish_job(pid, State::Lost));
        }
        let others: Vec<usize> = (1..d.cores.len()).filter(|&c| c != core && d.cores[c].up).collect();
        (waiters, others)
    });
    println!("director: cpu {} stopped answering, marked down", core);
    for t in waiters {
        rpc::reply(t, DOWN, Body::EMPTY);
    }
    rpc::peer_down(core);
    for c in others {
        rpc::oneway(rpc::handle(c, rpc::SVC_CORE), rpc::CORE_PEER_DOWN, Body::words(&[core as u64]));
    }
}

// nothing else can start jobs yet, so the director runs a fixed plan
pub fn start_plan() {
    if task::spawn(plan, 0, false).is_none() {
        println!("director: no task for the boot plan");
    }
}

fn ask(op: u16, body: Body) -> Packet {
    let director = rpc::handle(0, rpc::SVC_DIRECTOR);
    match rpc::submit_timeout(director, op, body, Then::Task(task::current()), rpc::NEVER) {
        Ok(id) => rpc::wait(id).unwrap_or_else(|| Packet::status_only(INVALID)),
        Err(status) => Packet::status_only(status),
    }
}

fn start(name: &str, core: u64) -> Option<u64> {
    let mut body = Body::words(&[core]);
    body.set_name(8, 32, name.as_bytes());
    let p = ask(OP_SPAWN, body);
    if p.status != OK {
        println!("director: could not start {} (status {})", name, p.status);
        return None;
    }
    Some(p.body.word(0))
}

fn run(name: &str, core: u64) {
    if let Some(pid) = start(name, core) {
        ask(OP_WAIT, Body::words(&[pid]));
    }
}

fn plan(_: u64) {
    let cores = fabric::cores() as u64;
    run("hello", ANY_CORE);
    let echo_core = if cores > 1 { 1 } else { 0 };
    if start("echod", echo_core).is_some() {
        let mut key = Body::EMPTY;
        key.set_name(0, 32, b"echo");
        while ask(OP_LOOKUP, key).status != OK {
            task::sleep_until(task::ticks() + crate::clock::ms(1));
        }
        run("ping", echo_core);
        for c in (0..cores).filter(|&c| c != echo_core) {
            run("ping", c);
        }
        run("flood", ANY_CORE);
    }
    run("fault", ANY_CORE);
    run("ticker", ANY_CORE);
    println!("director: boot plan done");
}
