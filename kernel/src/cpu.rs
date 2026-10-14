use core::arch::asm;

#[repr(C, packed)]
pub struct DescriptorTablePointer {
    pub limit: u16,
    pub base: u64,
}

#[inline]
pub fn outb(port: u16, val: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags))
    }
}

#[inline]
pub fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags))
    }
    val
}

#[inline]
pub fn outw(port: u16, val: u16) {
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack, preserves_flags))
    }
}

#[inline]
pub fn hlt() {
    unsafe {
        asm!("hlt", options(nostack, preserves_flags))
    }
}

pub fn hlt_loop() -> ! {
    loop {
        hlt();
    }
}

#[inline]
pub fn cli() {
    unsafe {
        asm!("cli", options(nostack, preserves_flags))
    }
}

// sti only takes effect after the next instruction, so no interrupt can
// slip in between it and hlt
#[inline]
pub fn sti_hlt() {
    unsafe {
        asm!("sti", "hlt", options(nostack, preserves_flags))
    }
}

pub fn has_mwait() -> bool {
    cpuid(1, 0).2 & (1 << 3) != 0
}

#[inline]
pub fn monitor(addr: *const u8) {
    unsafe { asm!("monitor", in("rax") addr, in("ecx") 0, in("edx") 0, options(nostack, preserves_flags)) }
}

// like sti_hlt, but a write to the line armed with monitor wakes it too.
// hint 0 is the shallowest sleep, the fastest to leave.
#[inline]
pub fn sti_mwait() {
    unsafe { asm!("sti", "mwait", in("eax") 0, in("ecx") 0, options(nostack, preserves_flags)) }
}

#[inline]
pub fn sti() {
    unsafe {
        asm!("sti", options(nostack, preserves_flags))
    }
}

// returns the old flags so nested critical sections restore the right IF
#[inline]
pub fn push_cli() -> u64 {
    let flags: u64;
    unsafe {
        asm!("pushfq", "pop {}", "cli", out(reg) flags)
    }
    flags
}

// only IF can differ, and popf costs far more than sti
#[inline]
pub fn pop_flags(flags: u64) {
    if flags & 0x200 != 0 {
        unsafe { asm!("sti", options(nostack)) }
    }
}

// the same ordering as mfence for the store-then-load cases here, at about
// half the cost on x86
#[inline(always)]
pub fn full_fence() {
    unsafe { asm!("lock or qword ptr [rsp], 0") }
}

pub fn cpuid(leaf: u32, sub: u32) -> (u32, u32, u32, u32) {
    let r = unsafe { core::arch::x86_64::__cpuid_count(leaf, sub) };
    (r.eax, r.ebx, r.ecx, r.edx)
}

// logical cpus sharing the L3: leaf 4 on intel, 0x8000001d on amd
pub fn l3_sharing() -> u32 {
    let max_ext = cpuid(0x8000_0000, 0).0;
    for leaf in [4, 0x8000_001D] {
        if leaf > 0x8000_0000 && max_ext < leaf {
            continue;
        }
        for sub in 0..8 {
            let eax = cpuid(leaf, sub).0;
            if eax & 0x1F == 0 {
                break;
            }
            if (eax >> 5) & 7 == 3 {
                return ((eax >> 14) & 0xFFF) + 1;
            }
        }
    }
    1
}

// low apic id bits that number the threads of one core
pub fn smt_shift() -> u32 {
    if cpuid(0, 0).0 >= 0xB {
        let (eax, _, ecx, _) = cpuid(0xB, 0);
        if (ecx >> 8) & 0xFF == 1 {
            return eax & 0x1F;
        }
    }
    if cpuid(0x8000_0000, 0).0 >= 0x8000_001E {
        let threads = (cpuid(0x8000_001E, 0).1 >> 8) & 0xFF;
        return (threads + 1).next_power_of_two().trailing_zeros();
    }
    0
}

#[inline]
pub fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags))
    }
    ((hi as u64) << 32) | lo as u64
}

#[inline]
pub fn read_cr2() -> u64 {
    let val: u64;
    unsafe {
        asm!("mov {}, cr2", out(reg) val, options(nomem, nostack, preserves_flags))
    }
    val
}

#[inline]
pub fn read_cr3() -> u64 {
    let val: u64;
    unsafe {
        asm!("mov {}, cr3", out(reg) val, options(nomem, nostack, preserves_flags))
    }
    val
}

#[inline]
pub fn write_cr3(val: u64) {
    unsafe {
        asm!("mov cr3, {}", in(reg) val, options(nostack, preserves_flags))
    }
}

#[inline]
pub fn invlpg(addr: u64) {
    unsafe {
        asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags))
    }
}

#[inline]
pub fn rdmsr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags))
    }
    ((high as u64) << 32) | low as u64
}

#[inline]
pub fn wrmsr(msr: u32, val: u64) {
    let low = val as u32;
    let high = (val >> 32) as u32;
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") low, in("edx") high, options(nomem, nostack, preserves_flags))
    }
}

pub unsafe fn lidt(desc: &DescriptorTablePointer) {
    asm!("lidt [{}]", in(reg) desc, options(readonly, nostack, preserves_flags))
}

pub unsafe fn ltr(sel: u16) {
    asm!("ltr {0:x}", in(reg) sel, options(nostack, preserves_flags));
}
