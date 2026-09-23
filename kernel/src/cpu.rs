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
pub fn outl(port: u16, val: u32) {
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack, preserves_flags))
    }
}

#[inline]
pub fn inl(port: u16) -> u32 {
    let val: u32;
    unsafe {
        asm!("in eax, dx", out("eax") val, in("dx") port, options(nomem, nostack, preserves_flags))
    }
    val
}

#[inline]
pub fn hlt() {
    unsafe {
        asm!("hlt", options(nomem, nostack, preserves_flags))
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
        asm!("cli", options(nomem, nostack, preserves_flags))
    }
}

#[inline]
pub fn sti() {
    unsafe {
        asm!("sti", options(nomem, nostack, preserves_flags))
    }
}

#[inline]
pub fn push_cli() -> u64 {
    let flags: u64;
    unsafe {
        asm!("pushfq", "pop {}", "cli", out(reg) flags)
    }
    flags
}

#[inline]
pub fn pop_flags(flags: u64) {
    unsafe {
        asm!("push {}", "popfq", in(reg) flags)
    }
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

pub unsafe fn lgdt(desc: &DescriptorTablePointer) {
    asm!("lgdt [{}]", in(reg) desc, options(readonly, nostack, preserves_flags))
}

pub unsafe fn lidt(desc: &DescriptorTablePointer) {
    asm!("lidt [{}]", in(reg) desc, options(readonly, nostack, preserves_flags))
}

pub unsafe fn ltr(sel: u16) {
    asm!("ltr {0:x}", in(reg) sel, options(nostack, preserves_flags));
}
