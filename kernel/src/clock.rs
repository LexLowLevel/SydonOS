use crate::{cpu, pit};
use core::sync::atomic::{AtomicU64, Ordering};

pub const TICK_US: u64 = 1_000;

pub const fn ms(n: u64) -> u64 {
    n * 1000 / TICK_US
}

static TSC_PER_MS: AtomicU64 = AtomicU64::new(1);

pub fn calibrate() -> u64 {
    let t0 = cpu::rdtsc();
    pit::sleep_us(10_000);
    let per_ms = (cpu::rdtsc() - t0) / 10;
    set(per_ms);
    per_ms
}

pub fn set(tsc_per_ms: u64) {
    TSC_PER_MS.store(tsc_per_ms.max(1), Ordering::Relaxed);
}

pub fn tsc_per_ms() -> u64 {
    TSC_PER_MS.load(Ordering::Relaxed)
}

pub fn now_ns() -> u64 {
    (cpu::rdtsc() as u128 * 1_000_000 / tsc_per_ms() as u128) as u64
}
