use core::sync::atomic::{AtomicU64, Ordering};

pub static DOORBELLS_SENT: AtomicU64 = AtomicU64::new(0);
pub static DOORBELLS_TAKEN: AtomicU64 = AtomicU64::new(0);
pub static HALTS: AtomicU64 = AtomicU64::new(0);
pub static SYSCALLS: AtomicU64 = AtomicU64::new(0);
pub static MSGS_IN: AtomicU64 = AtomicU64::new(0);

// per-core counters, so a plain load and store is enough. an interrupt in
// between can lose a count, which is fine for statistics.
pub fn add(c: &AtomicU64, n: u64) {
    c.store(c.load(Ordering::Relaxed) + n, Ordering::Relaxed);
}

pub fn snapshot() -> [u64; 5] {
    [&DOORBELLS_SENT, &DOORBELLS_TAKEN, &HALTS, &SYSCALLS, &MSGS_IN].map(|c| c.load(Ordering::Relaxed))
}
