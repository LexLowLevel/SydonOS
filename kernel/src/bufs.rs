use crate::paging::phys_to_virt;
use crate::rpc::{self, Body};
use crate::sync::IrqCell;
use crate::{frame, smp};

pub const SIZE: usize = 64 * 1024;
const PER_CORE: usize = 16;
const TAG: u64 = 1 << 63;

// a core allocates only from its own slice, so allocating never waits on
// another core
struct Pool {
    phys: u64,
    free: [u16; PER_CORE],
    n: usize,
    // core + 1 that each of our buffers went to, so a core that dies with
    // some of them can be made to give them back
    lent: [u8; PER_CORE],
}

static POOL: IrqCell<Option<Pool>> = IrqCell::new(None);

pub fn create(cores: usize) -> u64 {
    let frames = (cores * PER_CORE * SIZE) as u64 / frame::FRAME;
    frame::alloc_contiguous(frames).expect("bufs: no room for the buffer arena")
}

pub fn init(phys: u64) {
    POOL.with(|p| *p = Some(Pool { phys, free: core::array::from_fn(|i| i as u16), n: PER_CORE, lent: [0; PER_CORE] }));
}

pub fn origin_of(h: u64) -> usize {
    origin(h)
}

fn origin(h: u64) -> usize {
    ((h >> 16) & 0xFFFF) as usize
}

pub fn valid(h: u64) -> bool {
    h & TAG != 0 && (h & 0xFFFF) < PER_CORE as u64
}

pub fn alloc() -> Option<u64> {
    let me = smp::core() as u64;
    POOL.with(|p| {
        let p = p.as_mut()?;
        p.n = p.n.checked_sub(1)?;
        Some(TAG | me << 16 | p.free[p.n] as u64)
    })
}

pub fn phys(h: u64) -> u64 {
    let base = POOL.with(|p| p.as_ref().map_or(0, |p| p.phys));
    base + ((origin(h) * PER_CORE + (h & 0xFFFF) as usize) * SIZE) as u64
}

pub fn bytes(h: u64) -> &'static mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(phys_to_virt(phys(h)) as *mut u8, SIZE) }
}

pub fn free(h: u64) {
    if origin(h) == smp::core() {
        free_local(h);
    } else {
        rpc::oneway(rpc::handle(origin(h), rpc::SVC_CORE), rpc::CORE_BUF_FREE, Body::words(&[h]));
    }
}

pub fn lend(h: u64, to: usize) {
    if valid(h) && origin(h) == smp::core() && to != smp::core() {
        POOL.with(|p| p.as_mut().map(|p| p.lent[(h & 0xFFFF) as usize] = to as u8 + 1));
    }
}

pub fn reclaim(from: usize) {
    let me = smp::core() as u64;
    for i in 0..PER_CORE {
        let lent = POOL.with(|p| p.as_ref().is_some_and(|p| p.lent[i] as usize == from + 1));
        if lent {
            free_local(TAG | me << 16 | i as u64);
        }
    }
}

pub fn free_local(h: u64) {
    if valid(h) && origin(h) == smp::core() {
        POOL.with(|p| {
            if let Some(p) = p.as_mut() {
                p.lent[(h & 0xFFFF) as usize] = 0;
                if p.n < PER_CORE && !p.free[..p.n].contains(&(h as u16)) {
                    p.free[p.n] = h as u16;
                    p.n += 1;
                }
            }
        });
    }
}
