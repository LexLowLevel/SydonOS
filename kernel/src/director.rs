use crate::jobs::{self, Class, Traits};
use alloc::boxed::Box;
use crate::fabric;

const DRIVERS: [&[u8]; 1] = [b"fs"];

#[derive(Clone, Copy)]
enum Side {
    Any,
    Avoid(usize),
    Prefer(usize),
}
use crate::rpc::{self, Answer, Body, Packet, Request, Then, BUSY, DOWN, EXISTS, INVALID, NOT_FOUND, OK};
use crate::sync::SpinLock;
use crate::{bufs, task};
use alloc::string::String;
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
pub const OP_POWEROFF: u16 = 12;
pub const OP_IMAGES: u16 = 13;
pub const OP_KILL: u16 = 14;
pub const OP_STATS: u16 = 15;
pub const OP_CRASH: u16 = 16;
pub const OP_POLL: u16 = 17;
pub const OP_PARK: u16 = 18;

pub const ANY_CORE: u64 = u64::MAX;
const NAME_MAX: usize = 24;

const DOWN_TICKS: u64 = crate::clock::ms(1000);
const CHECK_TICKS: u64 = crate::clock::ms(100);
const KEEP_EXITED: usize = 32;
// a core that fails this often within the window stays down
const RESTARTS_MAX: usize = 3;
const RESTART_WINDOW: u64 = crate::clock::ms(60_000);

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
    class: Class,
    state: State,
    spawn_token: u64,
    restart: Option<Restart>,
}

#[derive(Clone)]
struct Restart {
    name: Vec<u8>,
    start: Option<Vec<u8>>,
}

struct CoreInfo {
    up: bool,
    cluster: u64,
    // the physical core; siblings share it
    phys: u64,
    parked: bool,
    last_beat: u64,
    last_count: u64,
    jobs: u64,
    compute: u64,
    restarts: Vec<u64>,
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
    let l3_shift = crate::cpu::l3_sharing().next_power_of_two().trailing_zeros();
    let smt_shift = crate::cpu::smt_shift();
    let mut d = Director {
        registry: Vec::new(),
        jobs: Vec::new(),
        cores: (0..cores)
            .map(|c| CoreInfo {
                up: true,
                cluster: (fabric::apic_id(c) as u32 >> l3_shift) as u64,
                phys: (fabric::apic_id(c) as u32 >> smt_shift) as u64,
                parked: false,
                last_beat: now,
                last_count: 0,
                jobs: 0,
                compute: 0,
                restarts: Vec::new(),
            })
            .collect(),
        waiters: Vec::new(),
        next_pid: 1,
        next_check: 0,
    };
    d.registry.push(Entry { name: name32(b"director"), handle: rpc::handle(0, rpc::SVC_DIRECTOR) });
    d.registry.push(Entry { name: name32(b"console"), handle: rpc::handle(0, rpc::SVC_CONSOLE) });
    *DIRECTOR.lock() = Some(d);
    rpc::register_kernel(rpc::SVC_DIRECTOR, handler);
}

impl Director {
    // on ties an io job keeps the service's core free, an io latency job takes it
    fn place(&self, traits: &Traits, requested: u64) -> Result<usize, u8> {
        if requested != ANY_CORE {
            let c = requested as usize;
            return match self.cores.get(c) {
                Some(info) if info.up => Ok(c),
                Some(_) => Err(DOWN),
                None => Err(INVALID),
            };
        }
        if let Some(needs) = traits.needs() {
            let key = name32(needs);
            if let Some(e) = self.registry.iter().find(|e| e.name == key) {
                let home = rpc::handle_core(e.handle);
                let cluster = self.cores[home].cluster;
                let side = match traits.class {
                    Class::Io => Side::Avoid(home),
                    Class::IoLatency => Side::Prefer(home),
                    _ => Side::Any,
                };
                return Ok(self.least_loaded(|c| c.cluster == cluster, traits.class, side).unwrap_or(home));
            }
        }
        self.least_loaded(|_| true, traits.class, Side::Any).ok_or(DOWN)
    }

    // core 0 counts one extra job for the director and console. a core that
    // owns a driver counts extra for everything but io jobs. siblings share one
    // core's units, so a compute job looks at the whole physical core, where
    // other compute jobs count three times since they never leave it idle.
    // other jobs only count the compute jobs on the siblings.
    fn weight(&self, core: usize, class: Class) -> u64 {
        let driver = self
            .registry
            .iter()
            .any(|e| DRIVERS.iter().any(|d| e.name == name32(d)) && rpc::handle_core(e.handle) == core);
        let extra = match (driver, class) {
            (false, _) | (true, Class::Io | Class::IoLatency) => 0,
            (true, Class::Other) => 1,
            (true, Class::Compute) => 2,
        };
        let phys = self.cores[core].phys;
        let shared: u64 = (0..self.cores.len())
            .filter(|&s| self.cores[s].up && self.cores[s].phys == phys)
            .map(|s| {
                let c = &self.cores[s];
                match class {
                    Class::Compute => 2 * c.compute + if s == core { 0 } else { c.jobs + (s == 0) as u64 },
                    _ if s == core => 0,
                    _ => c.compute,
                }
            })
            .sum();
        self.cores[core].jobs + (core == 0) as u64 + extra + shared
    }

    fn least_loaded(&self, fits: impl Fn(&CoreInfo) -> bool, class: Class, side: Side) -> Option<usize> {
        (0..self.cores.len())
            .filter(|&c| self.cores[c].up && !self.cores[c].parked && fits(&self.cores[c]))
            .min_by_key(|&c| {
                let tie = match side {
                    Side::Avoid(h) => c == h,
                    Side::Prefer(h) => c != h,
                    Side::Any => false,
                };
                (self.weight(c, class), tie, c)
            })
    }

    fn finish_job(&mut self, pid: u64, state: State) -> Vec<u64> {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.pid == pid) {
            if matches!(job.state, State::Starting | State::Running) {
                let c = &mut self.cores[job.core];
                c.jobs -= 1;
                c.compute -= (job.class == Class::Compute) as u64;
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

pub fn spawn(name: &[u8], requested: u64, token: u64, start: Option<u64>) -> Result<(), u8> {
    let out = try_spawn(name, requested, token, start);
    if out.is_err() {
        start.map(bufs::free);
    }
    out
}

fn try_spawn(name: &[u8], requested: u64, token: u64, start: Option<u64>) -> Result<(), u8> {
    if jobs::basename(name).is_empty() || name.len() > NAME_MAX {
        return Err(INVALID);
    }
    if let Some(b) = jobs::builtin(name) {
        return spawn_as(name, b.traits, requested, token, start);
    }
    let probe = Box::into_raw(Box::new(Probe { name: name.to_vec(), requested, token, start }));
    if task::spawn(probe_task, probe as u64, false).is_none() {
        drop(unsafe { Box::from_raw(probe) });
        return Err(BUSY);
    }
    Ok(())
}

struct Probe {
    name: Vec<u8>,
    requested: u64,
    token: u64,
    start: Option<u64>,
}

fn probe_task(arg: u64) {
    let p = unsafe { Box::from_raw(arg as *mut Probe) };
    let status = match crate::user::image(&jobs::path(&p.name)) {
        Ok(image) => spawn_as(&p.name, jobs::manifest(&image[..image.len().min(jobs::MANIFEST_SCAN)]), p.requested, p.token, p.start).err(),
        Err(status) => Some(status),
    };
    if let Some(status) = status {
        p.start.map(bufs::free);
        rpc::reply(p.token, status, Body::EMPTY);
    }
}

// on an error `start` is still the caller's
fn spawn_as(name: &[u8], traits: Traits, requested: u64, token: u64, start: Option<u64>) -> Result<(), u8> {
    let base = jobs::basename(name);
    let (pid, core) = with(|d| {
        let core = d.place(&traits, requested)?;
        let pid = d.next_pid;
        d.next_pid += 1;
        let mut short = [0u8; 16];
        let n = base.len().min(16);
        short[..n].copy_from_slice(&base[..n]);
        let restart = traits.restart.then(|| Restart { name: name.to_vec(), start: start.map(start_copy) });
        d.jobs.push(Job { pid, name: short, core, class: traits.class, state: State::Starting, spawn_token: token, restart });
        d.cores[core].jobs += 1;
        d.cores[core].compute += (traits.class == Class::Compute) as u64;
        Ok::<_, u8>((pid, core))
    })?;
    let mut body = Body::words(&[pid]);
    body.set_name(8, NAME_MAX, name);
    let h = rpc::handle(core, rpc::SVC_CORE);
    let sent = match start {
        Some(buf) => rpc::submit_buf(h, rpc::CORE_LOAD, body, buf, Then::Call(loaded, pid)),
        None => rpc::submit(h, rpc::CORE_LOAD, body, Then::Call(loaded, pid)),
    };
    if let Err(status) = sent {
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
        rpc::reply(token, OK, Body::words(&[pid, core as u64, pkt.body.word(0), pkt.body.word(1)]));
    } else {
        rpc::reply(token, pkt.status, Body::EMPTY);
    }
    for t in waiters {
        rpc::reply(t, pkt.status, Body::EMPTY);
    }
}

fn power_off(_: u64, _: &Packet) {
    rpc::oneway(rpc::handle(0, rpc::SVC_CORE), rpc::CORE_POWEROFF, Body::EMPTY);
}

fn forward(token: u64, pkt: &Packet) {
    rpc::reply(token, pkt.status, pkt.body);
}

fn kernel_only(op: u16) -> bool {
    matches!(op, OP_REGISTER | OP_UNREGISTER | OP_EXITED)
}

fn privileged_only(op: u16) -> bool {
    matches!(op, OP_HANG | OP_POWEROFF | OP_KILL | OP_CRASH | OP_POLL | OP_PARK)
}

// pipes die with the core, so a restarted job gets nothing in their place
fn start_copy(buf: u64) -> Vec<u8> {
    let b = crate::bufs::bytes(buf);
    let len = (u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize).clamp(88, 4096);
    let mut start = b[..len].to_vec();
    for i in 0..3 {
        let kind = &mut start[16 + 24 * i];
        if matches!(*kind, 1 | 2 | 4) {
            *kind = 5;
        }
    }
    start
}

pub fn kill(pid: u64) -> u8 {
    let target = with(|d| {
        let j = d.jobs.iter().find(|j| j.pid == pid && matches!(j.state, State::Starting | State::Running))?;
        let privileged = jobs::builtin(trimmed(&j.name)).is_some_and(|b| b.traits.privileged);
        Some((j.core, privileged))
    });
    match target {
        None => NOT_FOUND,
        Some((_, true)) => INVALID,
        Some((core, false)) => {
            rpc::oneway(rpc::handle(core, rpc::SVC_CORE), rpc::CORE_KILL, Body::words(&[pid]));
            OK
        }
    }
}

fn handler(req: &Request) -> Answer {
    let b = &req.body;
    if (kernel_only(req.op) && !req.from_kernel()) || (privileged_only(req.op) && !req.privileged()) {
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
        OP_SPAWN => match spawn(b.name_at(8, NAME_MAX), b.word(0), req.token, req.buf()) {
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
                    Answer::Reply(OK, Body::words(&[c.up as u64 | (c.parked as u64) << 1, c.jobs, age, free, c.cluster | c.phys << 32]))
                }
                None => Answer::Reply(NOT_FOUND, Body::EMPTY),
            })
        }
        // w0 core, w1 0 always, 1 adaptive, 2 never
        OP_POLL => {
            let core = b.word(0) as usize;
            if core >= with(|d| d.cores.len()) || b.word(1) > 2 {
                return Answer::Reply(INVALID, Body::EMPTY);
            }
            rpc::oneway(rpc::handle(core, rpc::SVC_CORE), rpc::CORE_POLL, Body::words(&[b.word(1)]));
            Answer::Reply(OK, Body::EMPTY)
        }
        // w0 core, w1 1 to park, 0 to take it back. a parked core gets no new
        // jobs unless asked for by number, and sleeps instead of polling so its
        // sibling has the whole core.
        OP_PARK => {
            let core = b.word(0) as usize;
            if core == 0 || core >= with(|d| d.cores.len()) || b.word(1) > 1 {
                return Answer::Reply(INVALID, Body::EMPTY);
            }
            let park = b.word(1) == 1;
            with(|d| d.cores[core].parked = park);
            let mode = if park { fabric::Poll::Never } else { fabric::Poll::Adaptive };
            rpc::oneway(rpc::handle(core, rpc::SVC_CORE), rpc::CORE_POLL, Body::words(&[mode as u64]));
            Answer::Reply(OK, Body::EMPTY)
        }
        OP_HANG | OP_CRASH => {
            let core = b.word(0) as usize;
            if core == 0 || core >= with(|d| d.cores.len()) {
                return Answer::Reply(INVALID, Body::EMPTY);
            }
            let op = if req.op == OP_HANG { rpc::CORE_HANG } else { rpc::CORE_CRASH };
            rpc::oneway(rpc::handle(core, rpc::SVC_CORE), op, Body::EMPTY);
            Answer::Reply(OK, Body::EMPTY)
        }
        OP_KILL => Answer::Reply(kill(b.word(0)), Body::EMPTY),
        OP_STATS => {
            let h = rpc::handle(b.word(0) as usize, rpc::SVC_CORE);
            match rpc::submit(h, rpc::CORE_STATS, Body::EMPTY, Then::Call(forward, req.token)) {
                Ok(_) => Answer::Later,
                Err(status) => Answer::Reply(status, Body::EMPTY),
            }
        }
        OP_IMAGES => match jobs::BUILTIN.get(b.word(0) as usize) {
            Some(job) => {
                let mut out = Body::EMPTY;
                out.set_name(0, 16, job.name.as_bytes());
                out.set_name(16, 24, job.traits.needs().unwrap_or(b""));
                Answer::Reply(OK, out)
            }
            None => Answer::Reply(NOT_FOUND, Body::EMPTY),
        },
        OP_POWEROFF => {
            let fs = with(|d| d.registry.iter().find(|e| e.name == name32(b"fs")).map(|e| e.handle));
            let synced = fs.map(|h| rpc::submit(h, crate::fs::OP_SYNC, Body::EMPTY, Then::Call(power_off, 0)));
            if !matches!(synced, Some(Ok(_))) {
                rpc::oneway(rpc::handle(0, rpc::SVC_CORE), rpc::CORE_POWEROFF, Body::EMPTY);
            }
            Answer::Reply(OK, Body::EMPTY)
        }
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
            if count == fabric::DEAD {
                silent.push(core);
            } else if count != c.last_count {
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
    // stopped first, so a half alive kernel cannot touch the rings any more
    crate::smp::stop(core);
    let (waiters, jobs) = with(|d| {
        d.cores[core].up = false;
        d.registry.retain(|e| rpc::handle_core(e.handle) != core);
        let lost: Vec<(u64, Option<Restart>)> = d
            .jobs
            .iter()
            .filter(|j| j.core == core && matches!(j.state, State::Starting | State::Running))
            .map(|j| (j.pid, j.restart.clone()))
            .collect();
        let mut waiters = Vec::new();
        for &(pid, _) in &lost {
            waiters.extend(d.finish_job(pid, State::Lost));
        }
        (waiters, lost.into_iter().filter_map(|(_, r)| r).collect())
    });
    println!("director: cpu {} stopped answering, marked down", core);
    for t in waiters {
        rpc::reply(t, DOWN, Body::EMPTY);
    }
    rpc::peer_down(core);
    let rescue = Box::into_raw(Box::new(Rescue { core, jobs }));
    if task::spawn(rescue_task, rescue as u64, true).is_none() {
        drop(unsafe { Box::from_raw(rescue) });
        println!("director: no task to bring cpu {} back", core);
    }
}

struct Rescue {
    core: usize,
    jobs: Vec<Restart>,
}

static RESCUING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// every other core lets go of the dead one before its rings are cleared and
// it boots again. one rescue at a time, they share the boot page.
fn rescue_task(arg: u64) {
    let Rescue { core, jobs } = *unsafe { Box::from_raw(arg as *mut Rescue) };
    while RESCUING.swap(true, core::sync::atomic::Ordering::Acquire) {
        task::sleep_until(task::ticks() + crate::clock::ms(10));
    }
    let others: Vec<usize> = with(|d| (1..d.cores.len()).filter(|&c| c != core && d.cores[c].up).collect());
    let body = Body::words(&[core as u64]);
    for &c in &others {
        let p = rpc::call(rpc::handle(c, rpc::SVC_CORE), rpc::CORE_PEER_DOWN, body);
        if p.status != OK {
            println!("director: cpu {} did not let go of cpu {} (status {})", c, core, p.status);
        }
    }
    let now = task::ticks();
    let allowed = with(|d| {
        let r = &mut d.cores[core].restarts;
        r.retain(|&t| now.saturating_sub(t) < RESTART_WINDOW);
        r.push(now);
        r.len() <= RESTARTS_MAX
    });
    let back = allowed && {
        fabric::clear_rings(core);
        crate::smp::restart(core)
    };
    if back {
        rpc::peer_up(core);
        for &c in &others {
            rpc::call(rpc::handle(c, rpc::SVC_CORE), rpc::CORE_PEER_UP, body);
        }
        with(|d| {
            let c = &mut d.cores[core];
            c.up = true;
            c.last_beat = task::ticks();
            c.last_count = 0;
            c.jobs = 0;
            c.compute = 0;
        });
        println!("director: cpu {} is back", core);
    } else if allowed {
        println!("director: cpu {} did not come back", core);
    } else {
        println!("director: cpu {} failed {} times in a minute, leaving it down", core, RESTARTS_MAX + 1);
    }
    RESCUING.store(false, core::sync::atomic::Ordering::Release);
    if !back {
        return;
    }
    for job in jobs {
        let start = job.start.as_ref().and_then(|s| {
            let buf = crate::bufs::alloc()?;
            crate::bufs::bytes(buf)[..s.len()].copy_from_slice(s);
            Some(buf)
        });
        match spawn(&job.name, core as u64, 0, start) {
            Ok(()) => println!("director: starting {} again on cpu {}", String::from_utf8_lossy(&job.name), core),
            Err(status) => println!("director: could not restart {} (status {})", String::from_utf8_lossy(&job.name), status),
        }
    }
}
