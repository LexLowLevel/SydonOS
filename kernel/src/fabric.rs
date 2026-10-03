use crate::acpi::MAX_CPUS;
use crate::paging::phys_to_virt;
use crate::sync::IrqCell;
use crate::{apic, cpu, frame, task};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, Ordering};

pub const SLOTS: u64 = 64;
pub const DOORBELL_VEC: u8 = 0x40;
const PER_RING: usize = 32;
// a sender that finds less room than this waits for more, so one doorbell
// fence covers a real burst instead of a slot or two
const BURST_MIN: u64 = SLOTS / 2;

const SLOT_SIZE: u64 = 64;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Msg(pub [u64; 7]);

pub struct Full;

// seq n+1 means the slot holds message n. the receiver never writes a slot:
// it publishes how many it has taken on the ring's own line instead, so a
// slot's cache line only travels from sender to receiver.
#[repr(C, align(64))]
struct Slot {
    seq: AtomicU64,
    body: UnsafeCell<[u64; 7]>,
}

#[repr(C, align(64))]
struct Taken {
    count: AtomicU64,
}

#[repr(C, align(64))]
struct Door {
    asleep: AtomicU64,
    apic_id: AtomicU64,
}

pub struct RingTx {
    slots: *const Slot,
    taken: *const Taken,
    mask: u64,
    next: u64,
    // the receiver's count as last seen, reread only when the ring looks full
    seen: u64,
}

pub struct RingRx {
    slots: *const Slot,
    taken: *const Taken,
    mask: u64,
    next: u64,
}

unsafe impl Send for RingTx {}
unsafe impl Send for RingRx {}

impl RingTx {
    fn new(slots: *const Slot, count: u64) -> Self {
        RingTx { slots, taken: unsafe { slots.add(count as usize) as *const Taken }, mask: count - 1, next: 0, seen: 0 }
    }

    fn room(&mut self, want: u64) -> u64 {
        let size = self.mask + 1;
        if size - (self.next - self.seen) < want {
            // acquire pairs with the receiver's release, so it is done reading what we overwrite
            self.seen = unsafe { (*self.taken).count.load(Ordering::Acquire) };
        }
        size - (self.next - self.seen)
    }

    pub fn try_send(&mut self, msg: &Msg) -> Result<(), Full> {
        if self.room(1) == 0 {
            return Err(Full);
        }
        let slot = unsafe { &*self.slots.add((self.next & self.mask) as usize) };
        unsafe { slot.body.get().write(msg.0) };
        self.next += 1;
        slot.seq.store(self.next, Ordering::Release);
        Ok(())
    }
}

impl RingRx {
    fn new(slots: *const Slot, count: u64) -> Self {
        RingRx { slots, taken: unsafe { slots.add(count as usize) as *const Taken }, mask: count - 1, next: 0 }
    }

    fn slot(&self) -> &Slot {
        unsafe { &*self.slots.add((self.next & self.mask) as usize) }
    }

    pub fn pending(&self) -> bool {
        self.slot().seq.load(Ordering::Acquire) == self.next + 1
    }

    // the sender learns about the freed slots at the next publish
    fn take(&mut self) -> Option<Msg> {
        let slot = self.slot();
        let seq = slot.seq.load(Ordering::Acquire);
        if seq != self.next + 1 {
            // older laps leave smaller numbers behind, a bigger one means the sender lapped us
            assert!(seq < self.next + 1, "fabric: ring out of sequence");
            return None;
        }
        let msg = Msg(unsafe { slot.body.get().read() });
        self.next += 1;
        Some(msg)
    }

    fn publish(&self) {
        unsafe { (*self.taken).count.store(self.next, Ordering::Release) };
    }

    pub fn try_recv(&mut self) -> Option<Msg> {
        let msg = self.take()?;
        self.publish();
        Some(msg)
    }
}

// kept off the door line, which every sender reads
#[repr(C, align(64))]
struct Beat {
    count: AtomicU64,
    free_kib: AtomicU64,
}

#[derive(Clone, Copy)]
pub struct Arena {
    pub phys: u64,
    pub cores: usize,
    pub slots: u64,
}

impl Arena {
    pub fn create(apic_ids: &[u8], slots: u64) -> Arena {
        assert!(slots.is_power_of_two(), "fabric: slot count must be a power of two");
        let cores = apic_ids.len();
        let arena = Arena { phys: 0, cores, slots };
        let frames = arena.size().div_ceil(frame::FRAME);
        let phys = frame::alloc_contiguous(frames).expect("fabric: no room for ring arena");
        unsafe {
            core::ptr::write_bytes(phys_to_virt(phys) as *mut u8, 0, (frames * frame::FRAME) as usize);
        }
        let arena = Arena { phys, ..arena };
        for (core, &id) in apic_ids.iter().enumerate() {
            arena.door(core).apic_id.store(id as u64, Ordering::Relaxed);
        }
        arena
    }

    fn size(&self) -> u64 {
        let pairs = (self.cores * (self.cores - 1)) as u64;
        self.lines() + pairs * self.ring_bytes()
    }

    // the slots and the line where the receiver says how many it took
    fn ring_bytes(&self) -> u64 {
        (self.slots + 1) * SLOT_SIZE
    }

    fn lines(&self) -> u64 {
        2 * self.cores as u64 * SLOT_SIZE
    }

    fn beat(&self, core: usize) -> &Beat {
        let at = phys_to_virt(self.phys) + (self.cores + core) as u64 * SLOT_SIZE;
        unsafe { &*(at as *const Beat) }
    }

    fn door(&self, core: usize) -> &Door {
        unsafe { &*(phys_to_virt(self.phys) as *const Door).add(core) }
    }

    fn ring(&self, from: usize, to: usize) -> *const Slot {
        let pair = from * (self.cores - 1) + if to > from { to - 1 } else { to };
        let offset = self.lines() + pair as u64 * self.ring_bytes();
        (phys_to_virt(self.phys) + offset) as *const Slot
    }
}

struct Fabric {
    arena: Arena,
    me: usize,
    tx: [Option<RingTx>; MAX_CPUS],
    rx: [Option<RingRx>; MAX_CPUS],
    turn: usize,
    waiter: Option<usize>,
}

impl Fabric {
    fn pending(&self) -> bool {
        self.rx.iter().flatten().any(|r| r.pending())
    }

    // round robin so one chatty core cannot starve the others
    fn poll(&mut self) -> Option<(usize, Msg)> {
        let n = self.arena.cores;
        for k in 0..n {
            let src = (self.turn + k) % n;
            if let Some(msg) = self.rx[src].as_mut().and_then(|r| r.try_recv()) {
                self.turn = src + 1;
                return Some((src, msg));
            }
        }
        None
    }

    // pairs with the fence in prepare_halt(): either we see the receiver asleep,
    // or it sees our slot before it halts. without both fences x86 may
    // let each side read before its own store lands, and the wakeup is lost.
    // one fence covers all slots written before it.
    fn ring_doorbells(&self, mask: u64) {
        if mask == 0 {
            return;
        }
        cpu::full_fence();
        for dst in (0..self.arena.cores).filter(|&d| mask & (1 << d) != 0) {
            let door = self.arena.door(dst);
            if door.asleep.load(Ordering::Relaxed) != 0 && door.asleep.swap(0, Ordering::Relaxed) != 0 {
                apic::send_fixed(door.apic_id.load(Ordering::Relaxed) as u8, DOORBELL_VEC);
            }
        }
    }
}

static FABRIC: IrqCell<Option<Fabric>> = IrqCell::new(None);

fn with<R>(f: impl FnOnce(&mut Fabric) -> R) -> Option<R> {
    FABRIC.with(|fab| fab.as_mut().map(f))
}

pub fn init(arena: Arena, me: usize) {
    let mut fabric = Fabric {
        arena,
        me,
        tx: [const { None }; MAX_CPUS],
        rx: [const { None }; MAX_CPUS],
        turn: 0,
        waiter: None,
    };
    for peer in (0..arena.cores).filter(|&p| p != me) {
        fabric.tx[peer] = Some(RingTx::new(arena.ring(me, peer), arena.slots));
        fabric.rx[peer] = Some(RingRx::new(arena.ring(peer, me), arena.slots));
    }
    FABRIC.with(|fab| *fab = Some(fabric));
}

pub fn cores() -> usize {
    with(|f| f.arena.cores).unwrap_or(1)
}

pub fn beat(free_kib: u64) {
    with(|f| {
        let b = f.arena.beat(f.me);
        b.free_kib.store(free_kib, Ordering::Relaxed);
        b.count.store(b.count.load(Ordering::Relaxed) + 1, Ordering::Release);
    });
}

pub fn beat_of(core: usize) -> (u64, u64) {
    with(|f| {
        let b = f.arena.beat(core);
        (b.count.load(Ordering::Acquire), b.free_kib.load(Ordering::Relaxed))
    })
    .unwrap_or((0, 0))
}

pub fn asleep(core: usize) -> bool {
    with(|f| f.arena.door(core).asleep.load(Ordering::Relaxed) != 0).unwrap_or(false)
}

pub fn pending() -> bool {
    with(|f| f.pending()).unwrap_or(false)
}

// no doorbell, the caller rings once per burst
pub fn try_push(dst: usize, msg: &Msg) -> Result<(), Full> {
    with(|f| f.tx[dst].as_mut().ok_or(Full)?.try_send(msg)).expect("fabric: not initialised")
}

pub fn doorbells(mask: u64) {
    with(|f| f.ring_doorbells(mask));
}

pub fn push_burst(dst: usize, msgs: &[Msg]) -> usize {
    with(|f| {
        let Some(tx) = f.tx[dst].as_mut() else { return 0 };
        let want = (msgs.len() as u64).min(BURST_MIN);
        if tx.room(want) < want {
            return 0;
        }
        let n = msgs.iter().take_while(|m| tx.try_send(m).is_ok()).count();
        if n > 0 {
            f.ring_doorbells(1 << dst);
        }
        n
    })
    .expect("fabric: not initialised")
}

pub fn poll_many(out: &mut [(usize, Msg)]) -> usize {
    with(|f| {
        let cores = f.arena.cores;
        let mut got = 0;
        let mut empty = 0;
        while got < out.len() && empty < cores {
            let src = if f.turn < cores { f.turn } else { 0 };
            f.turn = src + 1;
            let mut took = 0;
            if let Some(rx) = f.rx[src].as_mut() {
                while took < PER_RING && got < out.len() {
                    let Some(m) = rx.take() else { break };
                    out[got] = (src, m);
                    got += 1;
                    took += 1;
                }
                if took > 0 {
                    rx.publish();
                }
            }
            empty = if took == 0 { empty + 1 } else { 0 };
        }
        got
    })
    .unwrap_or(0)
}

pub fn try_send(dst: usize, msg: &Msg) -> Result<(), Full> {
    try_push(dst, msg)?;
    doorbells(1 << dst);
    Ok(())
}

// while the ring is full, keep draining ours through on_msg, or two cores
// sending to each other deadlock
pub fn send(dst: usize, msg: &Msg, mut on_msg: impl FnMut(usize, Msg)) {
    while try_send(dst, msg).is_err() {
        match poll() {
            Some((src, m)) => on_msg(src, m),
            None => core::hint::spin_loop(),
        }
    }
}

pub fn poll() -> Option<(usize, Msg)> {
    with(|f| f.poll()).flatten()
}

// without `arm` senders do not ring, so someone else on this core must poll
pub fn wait(arm: bool) {
    let flags = cpu::push_cli();
    let sleep = with(|f| {
        if arm {
            f.arena.door(f.me).asleep.store(1, Ordering::Relaxed);
            cpu::full_fence();
        }
        if f.pending() {
            f.arena.door(f.me).asleep.store(0, Ordering::Relaxed);
            return false;
        }
        f.waiter = Some(task::current());
        true
    })
    .unwrap_or(false);
    if sleep {
        task::block_current();
        with(|f| {
            f.waiter = None;
            f.arena.door(f.me).asleep.store(0, Ordering::Relaxed);
        });
    }
    cpu::pop_flags(flags);
}

fn kick(f: &Fabric) -> bool {
    match f.waiter {
        Some(t) if f.pending() => {
            task::wake(t);
            true
        }
        _ => false,
    }
}

pub fn start_polling() -> bool {
    with(|f| {
        if f.waiter.is_none() {
            return false;
        }
        f.arena.door(f.me).asleep.store(0, Ordering::Relaxed);
        true
    })
    .unwrap_or(false)
}

pub fn idle_poll(ns: u64) -> bool {
    if !start_polling() {
        return false;
    }
    let until = crate::clock::now_ns() + ns;
    while crate::clock::now_ns() < until && !task::others_ready() {
        if with(|f| kick(f)).unwrap_or(false) {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

pub fn stop_polling() -> bool {
    with(|f| {
        if f.waiter.is_none() {
            return true;
        }
        f.arena.door(f.me).asleep.store(1, Ordering::Relaxed);
        cpu::full_fence();
        if kick(f) {
            f.arena.door(f.me).asleep.store(0, Ordering::Relaxed);
            return false;
        }
        true
    })
    .unwrap_or(true)
}

pub fn on_doorbell() {
    with(|f| {
        if let Some(t) = f.waiter {
            task::wake(t);
        }
    });
}

// safety net: a missed doorbell costs at most one timer tick
pub fn on_tick() {
    with(|f| kick(f));
}
