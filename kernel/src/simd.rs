use crate::cpu;
use alloc::alloc::{alloc_zeroed, dealloc};
use core::alloc::Layout;
use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

static XSAVE: AtomicBool = AtomicBool::new(false);
static XSAVEOPT: AtomicBool = AtomicBool::new(false);
static SIZE: AtomicUsize = AtomicUsize::new(512);
static MASK: AtomicU64 = AtomicU64::new(0);

const CR0_MP: u64 = 1 << 1;
const CR0_EM: u64 = 1 << 2;
const CR0_TS: u64 = 1 << 3;
const CR4_OSFXSR: u64 = 1 << 9;
const CR4_OSXMMEXCPT: u64 = 1 << 10;
const CR4_OSXSAVE: u64 = 1 << 18;
const XCR0_X87_SSE: u64 = 0b11;
const XCR0_AVX: u64 = 1 << 2;
const XCR0_AVX512: u64 = 0b111 << 5;

fn read_cr0() -> u64 {
    let v: u64;
    unsafe { asm!("mov {}, cr0", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

fn read_cr4() -> u64 {
    let v: u64;
    unsafe { asm!("mov {}, cr4", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

// the kernel itself is built without sse, so this is only for jobs
pub fn init() -> (u64, usize) {
    let (_, _, ecx, _) = cpu::cpuid(1, 0);
    let cr0 = (read_cr0() & !(CR0_EM | CR0_TS)) | CR0_MP;
    let mut cr4 = read_cr4() | CR4_OSFXSR | CR4_OSXMMEXCPT;
    let xsave = ecx & (1 << 26) != 0;
    if xsave {
        cr4 |= CR4_OSXSAVE;
    }
    unsafe {
        asm!("mov cr0, {}", in(reg) cr0, options(nostack));
        asm!("mov cr4, {}", in(reg) cr4, options(nostack));
    }
    if !xsave {
        return (XCR0_X87_SSE, 512);
    }
    let (lo, _, _, hi) = cpu::cpuid(0xD, 0);
    let supported = lo as u64 | (hi as u64) << 32;
    let mut mask = XCR0_X87_SSE;
    if ecx & (1 << 28) != 0 {
        mask |= XCR0_AVX;
    }
    if cpu::cpuid(7, 0).1 & (1 << 16) != 0 {
        mask |= XCR0_AVX512;
    }
    mask &= supported;
    unsafe { asm!("xsetbv", in("ecx") 0u32, in("eax") mask as u32, in("edx") (mask >> 32) as u32, options(nostack)) };
    // the size is for what xcr0 has turned on, so ask after setting it
    let size = cpu::cpuid(0xD, 0).1 as usize;
    XSAVE.store(true, Ordering::Relaxed);
    XSAVEOPT.store(cpu::cpuid(0xD, 1).0 & 1 != 0, Ordering::Relaxed);
    SIZE.store(size, Ordering::Relaxed);
    MASK.store(mask, Ordering::Relaxed);
    (mask, size)
}

fn layout() -> Layout {
    Layout::from_size_align(SIZE.load(Ordering::Relaxed), 64).unwrap()
}

// everything in its reset state. the header says nothing is in use, so xsaveopt
// can skip it until the job touches the registers. mxcsr is loaded either way.
pub fn new_area() -> Option<*mut u8> {
    let p = unsafe { alloc_zeroed(layout()) };
    if p.is_null() {
        return None;
    }
    unsafe {
        (p as *mut u16).write(0x037F);
        (p.add(24) as *mut u32).write(0x1F80);
    }
    Some(p)
}

pub fn free_area(p: *mut u8) {
    unsafe { dealloc(p, layout()) };
}

pub unsafe fn save(p: *mut u8) {
    let mask = MASK.load(Ordering::Relaxed);
    let (lo, hi) = (mask as u32, (mask >> 32) as u32);
    if !XSAVE.load(Ordering::Relaxed) {
        asm!("fxsave64 [{}]", in(reg) p, options(nostack));
    } else if XSAVEOPT.load(Ordering::Relaxed) {
        asm!("xsaveopt64 [{}]", in(reg) p, in("eax") lo, in("edx") hi, options(nostack));
    } else {
        asm!("xsave64 [{}]", in(reg) p, in("eax") lo, in("edx") hi, options(nostack));
    }
}

pub unsafe fn restore(p: *mut u8) {
    let mask = MASK.load(Ordering::Relaxed);
    if XSAVE.load(Ordering::Relaxed) {
        asm!("xrstor64 [{}]", in(reg) p, in("eax") mask as u32, in("edx") (mask >> 32) as u32, options(nostack));
    } else {
        asm!("fxrstor64 [{}]", in(reg) p, options(nostack));
    }
}
