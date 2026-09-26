use crate::acpi::MAX_CPUS;
use crate::paging::phys_to_virt;
use crate::sync::SpinLock;
use crate::{apic, cpu, frame, task};
use core::cell::UnsafeCell;
use core::sync::atomic::{fence, AtomicU64, Ordering};

pub const SLOTS: u64 = 64;
pub const DOORBELL_VEC: u8 = 0x40;

const SLOT_SIZE: u64 = 64;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Msg(pub [u64; 7]);

pub struct Full;

// one cache line. seq 0 means free, seq n+1 means it holds message n.
// the line moves to the receiver with header and payload together.
#[repr(C, align(64))]
struct Slot {
    seq: AtomicU64,
    body: UnsafeCell<[u64; 7]>,
}

// per-core line in the arena. only the doorbell path touches it.
#[repr(C, align(64))]
struct Door {
    asleep: AtomicU64,
    apic_id: AtomicU64,
}

// one end of one ring. there is exactly one of each per ordered core pair,
// and &mut self keeps even tasks on the same core from sharing it.
pub struct RingTx {
    slots: *const Slot,
    mask: u64,
    next: u64,
}

pub struct RingRx {
    slots: *const Slot,
    mask: u64,
    next: u64,
}

unsafe impl Send for RingTx {}
unsafe impl Send for RingRx {}

impl RingTx {
    fn new(slots: *const Slot, count: u64) -> Self {
        RingTx { slots, mask: count - 1, next: 0 }
    }

    pub fn try_send(&mut self, msg: &Msg) -> Result<(), Full> {
        let slot = unsafe { &*self.slots.add((self.next & self.mask) as usize) };
        // acquire pairs with the receiver's release, so it is done reading before we overwrite
        if slot.seq.load(Ordering::Acquire) != 0 {
            return Err(Full);
        }
        unsafe { slot.body.get().write(msg.0) };
        self.next += 1;
        slot.seq.store(self.next, Ordering::Release);
        Ok(())
    }
}

impl RingRx {
    fn new(slots: *const Slot, count: u64) -> Self {
        RingRx { slots, mask: count - 1, next: 0 }
    }

    fn slot(&self) -> &Slot {
        unsafe { &*self.slots.add((self.next & self.mask) as usize) }
    }

    pub fn pending(&self) -> bool {
        self.slot().seq.load(Ordering::Acquire) != 0
    }

    pub fn try_recv(&mut self) -> Option<Msg> {
        let slot = self.slot();
        let seq = slot.seq.load(Ordering::Acquire);
        if seq == 0 {
            return None;
        }
        // the sender cannot lap us, so anything but our next number is corruption
        assert!(seq == self.next + 1, "fabric: ring out of sequence");
        let msg = Msg(unsafe { slot.body.get().read() });
        slot.seq.store(0, Ordering::Release);
        self.next += 1;
        Some(msg)
    }
}

// layout: one door line per core, then a ring for every ordered pair (from, to)
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
        self.lines() + pairs * self.slots * SLOT_SIZE
    }

    fn lines(&self) -> u64 {
        self.cores as u64 * SLOT_SIZE
    }

    fn door(&self, core: usize) -> &Door {
        unsafe { &*(phys_to_virt(self.phys) as *const Door).add(core) }
    }

    // pairs are packed without the diagonal: from * (n - 1) + to, skipping to == from
    fn ring(&self, from: usize, to: usize) -> *const Slot {
        let pair = from * (self.cores - 1) + if to > from { to - 1 } else { to };
        let offset = self.lines() + pair as u64 * self.slots * SLOT_SIZE;
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

    // pairs with the fence in wait(): either we see the receiver asleep,
    // or it sees our slot before it halts. without both fences x86 may
    // let each side read before its own store lands, and the wakeup is lost.
    fn ring_doorbell(&self, dst: usize) {
        fence(Ordering::SeqCst);
        let door = self.arena.door(dst);
        if door.asleep.load(Ordering::Relaxed) != 0 && door.asleep.swap(0, Ordering::Relaxed) != 0 {
            apic::send_fixed(door.apic_id.load(Ordering::Relaxed) as u8, DOORBELL_VEC);
        }
    }
}

static FABRIC: SpinLock<Option<Fabric>> = SpinLock::new(None);

fn with<R>(f: impl FnOnce(&mut Fabric) -> R) -> Option<R> {
    FABRIC.lock().as_mut().map(f)
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
    *FABRIC.lock() = Some(fabric);
}

pub fn cores() -> usize {
    with(|f| f.arena.cores).unwrap_or(1)
}

pub fn asleep(core: usize) -> bool {
    with(|f| f.arena.door(core).asleep.load(Ordering::Relaxed) != 0).unwrap_or(false)
}

pub fn try_send(dst: usize, msg: &Msg) -> Result<(), Full> {
    with(|f| {
        f.tx[dst].as_mut().expect("fabric: no ring to that core").try_send(msg)?;
        f.ring_doorbell(dst);
        Ok(())
    })
    .expect("fabric: not initialised")
}

// blocks while the ring is full, but keeps draining our own rings through
// on_msg. two cores stuck sending to each other would deadlock otherwise.
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

// sleeps the calling task until some ring has a message.
// there is one waiter slot per core, a second caller would take it over.
// interrupts stay off from the last check until the task is blocked.
pub fn wait() {
    let flags = cpu::push_cli();
    let sleep = with(|f| {
        let door = f.arena.door(f.me);
        door.asleep.store(1, Ordering::Relaxed);
        fence(Ordering::SeqCst);
        if f.pending() {
            door.asleep.store(0, Ordering::Relaxed);
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

pub fn on_doorbell() {
    with(|f| {
        if let Some(t) = f.waiter {
            task::wake(t);
        }
    });
}

// safety net: a missed doorbell costs at most one timer tick
pub fn on_tick() {
    with(|f| {
        if let Some(t) = f.waiter {
            if f.pending() {
                task::wake(t);
            }
        }
    });
}
