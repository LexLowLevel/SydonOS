use crate::paging::{self, phys_to_virt};
use crate::pci::Device;
use crate::sync::IrqCell;
use crate::{apic, cpu, frame, rpc, task};
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, AtomicBool, Ordering};

pub const IRQ_VECTOR: u8 = 0x30;
pub const SECTOR: u64 = 512;

const VENDOR: u16 = 0x1AF4;
const DEVICE: u16 = 0x1042;
const QUEUE: u16 = 128;
const SLOTS: usize = QUEUE as usize / 3;

const CAP_VENDOR: u8 = 0x09;
const CAP_MSIX: u8 = 0x11;
const CFG_COMMON: u8 = 1;
const CFG_NOTIFY: u8 = 2;
const CFG_DEVICE: u8 = 4;

const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const NO_VECTOR: u16 = 0xFFFF;

const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;
const REQ_IN: u32 = 0;
const REQ_OUT: u32 = 1;
const REQ_FLUSH: u32 = 4;
const F_RO: u32 = 1 << 5;
const F_FLUSH: u32 = 1 << 9;

#[derive(Clone, Copy)]
pub struct Req {
    kind: u32,
    pub sector: u64,
    pub phys: u64,
    pub len: u32,
}

impl Req {
    pub fn read(sector: u64, phys: u64, len: u32) -> Req {
        Req { kind: REQ_IN, sector, phys, len }
    }

    pub fn write(sector: u64, phys: u64, len: u32) -> Req {
        Req { kind: REQ_OUT, sector, phys, len }
    }
}

#[repr(C)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[derive(Clone, Copy)]
pub enum Then {
    Task(usize),
    Call(fn(u64, bool), u64),
}

#[derive(Clone, Copy)]
struct Slot {
    then: Then,
    done: bool,
    ok: bool,
}

struct Blk {
    notify: u64,
    desc: *mut Desc,
    avail: *mut u16,
    used: *const u16,
    reqs: u64,
    reqs_phys: u64,
    next_avail: u16,
    last_used: u16,
    slots: [Option<Slot>; SLOTS],
    finished: Vec<(fn(u64, bool), u64, bool)>,
    flush: bool,
}

unsafe impl Send for Blk {}

static BLK: IrqCell<Option<Blk>> = IrqCell::new(None);
static PENDING: AtomicBool = AtomicBool::new(false);
static READ_ONLY: AtomicBool = AtomicBool::new(false);

fn with<R>(f: impl FnOnce(&mut Blk) -> R) -> Option<R> {
    BLK.with(|b| b.as_mut().map(f))
}

unsafe fn w8(base: u64, off: u64, v: u8) {
    write_volatile((base + off) as *mut u8, v)
}
unsafe fn w16(base: u64, off: u64, v: u16) {
    write_volatile((base + off) as *mut u16, v)
}
unsafe fn w32(base: u64, off: u64, v: u32) {
    write_volatile((base + off) as *mut u32, v)
}
unsafe fn w64(base: u64, off: u64, v: u64) {
    write_volatile((base + off) as *mut u64, v)
}
unsafe fn r8(base: u64, off: u64) -> u8 {
    read_volatile((base + off) as *const u8)
}
unsafe fn r16(base: u64, off: u64) -> u16 {
    read_volatile((base + off) as *const u16)
}
unsafe fn r32(base: u64, off: u64) -> u32 {
    read_volatile((base + off) as *const u32)
}

fn dma_page() -> Option<(u64, u64)> {
    let p = frame::alloc_zeroed()?;
    Some((p, phys_to_virt(p)))
}

fn region(dev: &Device, cap: u16, bars: &mut [Option<u64>; 6]) -> Option<u64> {
    let bar = dev.read8(cap + 4);
    let off = dev.read32(cap + 8) as u64;
    if bar > 5 {
        return None;
    }
    if bars[bar as usize].is_none() {
        let (phys, size) = dev.bar(bar)?;
        bars[bar as usize] = Some(paging::map_mmio(phys, size));
    }
    Some(bars[bar as usize]? + off)
}

pub fn find(devices: &[Device]) -> Option<Device> {
    devices.iter().copied().find(|d| d.vendor == VENDOR && d.device == DEVICE)
}

pub fn init(dev: &Device) -> Option<u64> {
    dev.enable();
    let mut bars = [None; 6];
    let (mut common, mut notify, mut devcfg, mut mult) = (None, None, None, 0);
    for cap in dev.caps() {
        match dev.read8(cap) {
            CAP_VENDOR => match dev.read8(cap + 3) {
                CFG_COMMON => common = region(dev, cap, &mut bars),
                CFG_NOTIFY => {
                    notify = region(dev, cap, &mut bars);
                    mult = dev.read32(cap + 16) as u64;
                }
                CFG_DEVICE => devcfg = region(dev, cap, &mut bars),
                _ => {}
            },
            CAP_MSIX => setup_msix(dev, cap, &mut bars)?,
            _ => {}
        }
    }
    let (common, notify, devcfg) = (common?, notify?, devcfg?);
    unsafe {
        w8(common, 0x14, 0);
        while r8(common, 0x14) != 0 {}
        w8(common, 0x14, STATUS_ACK | STATUS_DRIVER);
        w32(common, 0x00, 1);
        if r32(common, 0x04) & 1 == 0 {
            return None;
        }
        w32(common, 0x00, 0);
        let offered = r32(common, 0x04);
        let wanted = offered & F_FLUSH;
        READ_ONLY.store(offered & F_RO != 0, Ordering::Relaxed);
        w32(common, 0x08, 0);
        w32(common, 0x0C, wanted);
        w32(common, 0x08, 1);
        w32(common, 0x0C, 1);
        w8(common, 0x14, STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK);
        if r8(common, 0x14) & STATUS_FEATURES_OK == 0 {
            return None;
        }

        w16(common, 0x10, NO_VECTOR);
        w16(common, 0x16, 0);
        let size = r16(common, 0x18).min(QUEUE);
        if size < QUEUE {
            return None;
        }
        w16(common, 0x18, QUEUE);
        let (desc_p, desc) = dma_page()?;
        let (avail_p, avail) = dma_page()?;
        let (used_p, used) = dma_page()?;
        let (reqs_p, reqs) = dma_page()?;
        w64(common, 0x20, desc_p);
        w64(common, 0x28, avail_p);
        w64(common, 0x30, used_p);
        w16(common, 0x1A, 0);
        let notify_off = r16(common, 0x1E) as u64;
        w16(common, 0x1C, 1);
        w8(common, 0x14, STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK);

        let sectors = r32(devcfg, 0) as u64 | (r32(devcfg, 4) as u64) << 32;
        BLK.with(|b| {
            *b = Some(Blk {
                notify: notify + notify_off * mult,
                desc: desc as *mut Desc,
                avail: avail as *mut u16,
                used: used as *const u16,
                reqs,
                reqs_phys: reqs_p,
                next_avail: 0,
                last_used: 0,
                slots: [None; SLOTS],
                finished: Vec::new(),
                flush: wanted != 0,
            })
        });
        Some(sectors)
    }
}

fn setup_msix(dev: &Device, cap: u16, bars: &mut [Option<u64>; 6]) -> Option<()> {
    let table = dev.read32(cap + 4);
    let bar = (table & 7) as u8;
    if bars[bar as usize].is_none() {
        let (phys, size) = dev.bar(bar)?;
        bars[bar as usize] = Some(paging::map_mmio(phys, size));
    }
    let entry = bars[bar as usize]? + (table & !7) as u64;
    unsafe {
        w32(entry, 0, 0xFEE0_0000 | (apic::id() as u32) << 12);
        w32(entry, 4, 0);
        w32(entry, 8, IRQ_VECTOR as u32);
        w32(entry, 12, 0);
    }
    let ctrl = dev.read16(cap + 2);
    dev.write16(cap + 2, (ctrl | 1 << 15) & !(1 << 14));
    Some(())
}

pub fn read_only() -> bool {
    READ_ONLY.load(Ordering::Relaxed)
}

pub fn submit(r: Req, then: Then) -> Result<usize, ()> {
    with(|b| {
        let i = b.slots.iter().position(|s| s.is_none()).ok_or(())?;
        b.slots[i] = Some(Slot { then, done: false, ok: false });
        unsafe {
            let hdr = b.reqs + i as u64 * 16;
            w32(hdr, 0, r.kind);
            w32(hdr, 4, 0);
            w64(hdr, 8, r.sector);
            let status = b.reqs_phys + 2048 + i as u64;
            let d = (i * 3) as u16;
            let last = if r.kind == REQ_FLUSH { d + 1 } else { d + 2 };
            b.desc.add(d as usize).write(Desc { addr: b.reqs_phys + i as u64 * 16, len: 16, flags: DESC_NEXT, next: d + 1 });
            if r.kind != REQ_FLUSH {
                let flags = if r.kind == REQ_IN { DESC_NEXT | DESC_WRITE } else { DESC_NEXT };
                b.desc.add(d as usize + 1).write(Desc { addr: r.phys, len: r.len, flags, next: d + 2 });
            }
            b.desc.add(last as usize).write(Desc { addr: status, len: 1, flags: DESC_WRITE, next: 0 });
            let at = b.next_avail % QUEUE;
            write_volatile(b.avail.add(2 + at as usize), d);
            b.next_avail = b.next_avail.wrapping_add(1);
            fence(Ordering::SeqCst);
            write_volatile(b.avail.add(1), b.next_avail);
            fence(Ordering::SeqCst);
            w16(b.notify, 0, 0);
        }
        Ok(i)
    })
    .ok_or(())?
}

pub fn read(sector: u64, phys: u64, len: u32) -> bool {
    run(&[Req::read(sector, phys, len)])
}

fn take(i: usize) -> Option<bool> {
    with(|b| match b.slots[i] {
        Some(s) if s.done => {
            b.slots[i] = None;
            Some(s.ok)
        }
        _ => None,
    })
    .flatten()
}

pub fn run(reqs: &[Req]) -> bool {
    if with(|_| ()).is_none() {
        return false;
    }
    let flags = cpu::push_cli();
    let me = task::current();
    let (mut next, mut ok) = (0, true);
    let mut busy: Vec<usize> = Vec::new();
    while next < reqs.len() || !busy.is_empty() {
        while next < reqs.len() {
            match submit(reqs[next], Then::Task(me)) {
                Ok(i) => busy.push(i),
                Err(()) => break,
            }
            next += 1;
        }
        let before = busy.len();
        busy.retain(|&i| match take(i) {
            Some(done) => {
                ok &= done;
                false
            }
            None => true,
        });
        if busy.is_empty() && next < reqs.len() {
            // every slot is taken by someone else's reads, which never wake us
            task::sleep_until(task::ticks() + 1);
        } else if busy.len() == before {
            task::block_current();
        }
    }
    cpu::pop_flags(flags);
    ok
}

pub fn flush() -> bool {
    if with(|b| b.flush) != Some(true) {
        return true;
    }
    run(&[Req { kind: REQ_FLUSH, sector: 0, phys: 0, len: 0 }])
}

pub fn on_irq() {
    with(|b| unsafe {
        let used_idx = read_volatile(b.used.add(1));
        while b.last_used != used_idx {
            let at = (b.last_used % QUEUE) as usize;
            let id = read_volatile((b.used as *const u32).add(1 + at * 2)) as usize;
            b.last_used = b.last_used.wrapping_add(1);
            let i = id / 3;
            let ok = read_volatile((b.reqs + 2048 + i as u64) as *const u8) == 0;
            let Some(slot) = b.slots[i].as_mut() else { continue };
            match slot.then {
                Then::Task(t) => {
                    slot.done = true;
                    slot.ok = ok;
                    task::wake(t);
                }
                Then::Call(f, ctx) => {
                    b.slots[i] = None;
                    b.finished.push((f, ctx, ok));
                    PENDING.store(true, Ordering::Release);
                }
            }
        }
    });
    if PENDING.load(Ordering::Acquire) {
        rpc::wake_executor();
    }
}

pub fn pending() -> bool {
    PENDING.load(Ordering::Acquire)
}

pub fn reap() {
    if !PENDING.swap(false, Ordering::AcqRel) {
        return;
    }
    let done = with(|b| core::mem::take(&mut b.finished)).unwrap_or_default();
    for (f, ctx, ok) in done {
        f(ctx, ok);
    }
}
