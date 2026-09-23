use crate::bootinfo::BootInfo;
use crate::paging::{phys_to_virt, KERNEL_OFFSET};
use crate::sync::SpinLock;

pub const FRAME: u64 = 4096;
const LOW_WATER: u64 = 0x10_0000;
const PHYS_CAP: u64 = 0x8000_0000;
const MAX_RANGES: usize = 64;

#[derive(Clone, Copy)]
struct Range {
    base: u64,
    frames: u64,
}

struct FrameAlloc {
    ranges: [Range; MAX_RANGES],
    n: usize,
    total: u64,
}

impl FrameAlloc {
    const fn new() -> Self {
        FrameAlloc {
            ranges: [Range { base: 0, frames: 0 }; MAX_RANGES],
            n: 0,
            total: 0,
        }
    }

    fn insert(&mut self, base: u64, frames: u64) {
        if frames == 0 || self.n == MAX_RANGES {
            return;
        }
        self.ranges[self.n] = Range { base, frames };
        self.n += 1;
        self.total += frames;
    }

    fn alloc_contiguous(&mut self, n: u64) -> Option<u64> {
        for i in 0..self.n {
            if self.ranges[i].frames >= n {
                let base = self.ranges[i].base;
                self.ranges[i].base += n * FRAME;
                self.ranges[i].frames -= n;
                if self.ranges[i].frames == 0 {
                    self.ranges[i] = self.ranges[self.n - 1];
                    self.n -= 1;
                }
                self.total -= n;
                return Some(base);
            }
        }
        None
    }

    fn free(&mut self, base: u64, frames: u64) {
        self.insert(base, frames);
    }
}

static FRAMES: SpinLock<FrameAlloc> = SpinLock::new(FrameAlloc::new());

extern "C" {
    static _kernel_start: u8;
    static _kernel_end: u8;
}

pub fn init(bi: &BootInfo) {
    let k0 = (unsafe { core::ptr::addr_of!(_kernel_start) } as u64 - KERNEL_OFFSET) & !(FRAME - 1);
    let k1 = (unsafe { core::ptr::addr_of!(_kernel_end) } as u64 - KERNEL_OFFSET + FRAME - 1)
        & !(FRAME - 1);
    let mut f = FRAMES.lock();
    for e in &bi.e820[..bi.e820_count as usize] {
        if e.kind != 1 {
            continue;
        }
        let start = (e.base + FRAME - 1) & !(FRAME - 1);
        let end = ((e.base + e.len) & !(FRAME - 1)).min(PHYS_CAP);
        let start = start.max(LOW_WATER);
        if end <= start {
            continue;
        }
        // skip the kernel image if it overlaps this range
        if k1 > start && k0 < end {
            if k0 > start {
                f.insert(start, (k0.min(end) - start) / FRAME);
            }
            if k1 < end {
                f.insert(k1.max(start), (end - k1.max(start)) / FRAME);
            }
        } else {
            f.insert(start, (end - start) / FRAME);
        }
    }
}

pub fn alloc() -> Option<u64> {
    FRAMES.lock().alloc_contiguous(1)
}

pub fn alloc_zeroed() -> Option<u64> {
    let p = alloc()?;
    unsafe {
        core::ptr::write_bytes(phys_to_virt(p) as *mut u8, 0, FRAME as usize);
    }
    Some(p)
}

pub fn alloc_contiguous(n: u64) -> Option<u64> {
    FRAMES.lock().alloc_contiguous(n)
}

pub fn free(base: u64, frames: u64) {
    FRAMES.lock().free(base, frames);
}

pub fn total_free() -> u64 {
    FRAMES.lock().total
}
