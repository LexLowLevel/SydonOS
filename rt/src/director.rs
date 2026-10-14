use crate::io::{start_block, Stream};
use crate::rpc::{Error, Payload, Service};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

const REGISTER_NAME_AT: usize = 8;
const SERVICES: u16 = 4;
const SPAWN: u16 = 5;
const WAIT: u16 = 6;
const PS: u16 = 7;
const CORES: u16 = 10;
const HANG: u16 = 11;
const CRASH: u16 = 16;
const POLL: u16 = 17;
const PARK: u16 = 18;
const POWEROFF: u16 = 12;
const IMAGES: u16 = 13;
const KILL: u16 = 14;
const STATS: u16 = 15;

pub const KILLED: u64 = 130;

const ANY_CORE: u64 = u64::MAX;
pub const NAME_MAX: usize = 24;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum JobState {
    Starting,
    Running,
    Exited(u64),
    Failed,
    Lost,
}

pub struct Job {
    pub pid: u64,
    pub core: usize,
    pub state: JobState,
    pub name: String,
}

pub struct CoreInfo {
    pub core: usize,
    pub up: bool,
    pub jobs: u64,
    pub silent_ms: u64,
    pub free_kib: u64,
    pub cluster: u64,
    pub phys: u64,
    pub parked: bool,
}

pub struct Image {
    pub name: String,
    pub needs: String,
}

fn director() -> Result<Service, Error> {
    Service::lookup("director")
}

fn list<T>(op: u16, mut f: impl FnMut(usize, &Payload) -> T) -> Result<Vec<T>, Error> {
    let d = director()?;
    let mut out = Vec::new();
    loop {
        match d.call(op, Payload::new().word(0, out.len() as u64).bytes()) {
            Ok(p) => out.push(f(out.len(), &p)),
            Err(Error::NotFound) => return Ok(out),
            Err(e) => return Err(e),
        }
    }
}

pub fn spawn(name: &str, core: Option<usize>) -> Result<(u64, usize), Error> {
    let at = core.map_or(ANY_CORE, |c| c as u64);
    let p = director()?.call(SPAWN, Payload::new().word(0, at).name(8, NAME_MAX, name).bytes())?;
    Ok((p.get_word(0), p.get_word(1) as usize))
}

pub struct Spawned {
    pub pid: u64,
    pub core: usize,
    pub input: Option<Stream>,
}

pub fn spawn_with(name: &str, core: Option<usize>, streams: [Stream; 3], args: &[&str]) -> Result<Spawned, Error> {
    let at = core.map_or(ANY_CORE, |c| c as u64);
    let body = Payload::new().word(0, at).name(8, NAME_MAX, name);
    let p = director()?.submit_buf(SPAWN, body.bytes(), &start_block(streams, args))?.wait()?;
    let (h, id) = (p.get_word(2), p.get_word(3));
    Ok(Spawned { pid: p.get_word(0), core: p.get_word(1) as usize, input: (id != 0).then_some(Stream::PipeWrite(h, id)) })
}

pub fn wait(pid: u64) -> Result<u64, Error> {
    let p = director()?.call_forever(WAIT, Payload::new().word(0, pid).bytes())?;
    Ok(p.get_word(0))
}

pub fn ps() -> Result<Vec<Job>, Error> {
    list(PS, |_, p| {
        let meta = p.get_word(1);
        let code = meta >> 32;
        let state = match (meta >> 16) & 0xFFFF {
            0 => JobState::Starting,
            1 => JobState::Running,
            2 => JobState::Exited(code),
            3 => JobState::Failed,
            _ => JobState::Lost,
        };
        Job { pid: p.get_word(0), core: (meta & 0xFFFF) as usize, state, name: p.get_name(16, 16).to_string() }
    })
}

pub fn services() -> Result<Vec<(String, Service)>, Error> {
    list(SERVICES, |_, p| (p.get_name(REGISTER_NAME_AT, 32).to_string(), Service(p.get_word(0))))
}

pub fn cores() -> Result<Vec<CoreInfo>, Error> {
    list(CORES, |core, p| CoreInfo {
        core,
        up: p.get_word(0) & 1 != 0,
        jobs: p.get_word(1),
        silent_ms: p.get_word(2),
        free_kib: p.get_word(3),
        cluster: p.get_word(4) & 0xFFFF_FFFF,
        phys: p.get_word(4) >> 32,
        parked: p.get_word(0) & 2 != 0,
    })
}

pub fn images() -> Result<Vec<Image>, Error> {
    list(IMAGES, |_, p| Image { name: p.get_name(0, 16).to_string(), needs: p.get_name(16, 24).to_string() })
}

pub fn hang(core: usize) -> Result<(), Error> {
    director()?.call(HANG, Payload::new().word(0, core as u64).bytes()).map(|_| ())
}

// 0 always, 1 adaptive, 2 never
pub fn poll(core: usize, mode: u64) -> Result<(), Error> {
    director()?.call(POLL, Payload::new().word(0, core as u64).word(1, mode).bytes()).map(|_| ())
}

pub fn park(core: usize, park: bool) -> Result<(), Error> {
    director()?.call(PARK, Payload::new().word(0, core as u64).word(1, park as u64).bytes()).map(|_| ())
}

pub fn crash(core: usize) -> Result<(), Error> {
    director()?.call(CRASH, Payload::new().word(0, core as u64).bytes()).map(|_| ())
}

pub fn kill(pid: u64) -> Result<(), Error> {
    director()?.call(KILL, Payload::new().word(0, pid).bytes()).map(|_| ())
}

pub fn stats(core: usize) -> Result<[u64; 5], Error> {
    let p = director()?.call(STATS, Payload::new().word(0, core as u64).bytes())?;
    Ok(core::array::from_fn(|i| p.get_word(i)))
}

pub fn poweroff() -> Result<(), Error> {
    director()?.call(POWEROFF, &[]).map(|_| ())
}
