use crate::cpu;

const LAPIC: u64 = crate::paging::LAPIC_VIRT;
const REG_EOI: u64 = 0xB0;
const REG_SVR: u64 = 0xF0;
const REG_LVT_TIMER: u64 = 0x320;
const REG_LVT_LINT0: u64 = 0x350;
const REG_LVT_LINT1: u64 = 0x360;
const REG_TIMER_INIT: u64 = 0x380;
const REG_TIMER_DIV: u64 = 0x3E0;

pub const TIMER_VEC: u32 = 32;
const PERIODIC: u32 = 1 << 17;
const MASKED: u32 = 1 << 16;

unsafe fn write(off: u64, v: u32) {
    core::ptr::write_volatile((LAPIC + off) as *mut u32, v);
}

pub fn mask_legacy_pic() {
    cpu::outb(0x21, 0xFF);
    cpu::outb(0xA1, 0xFF);
}

pub fn init(initial_count: u32) {
    let base = cpu::rdmsr(0x1B);
    cpu::wrmsr(0x1B, base | (1 << 11));
    unsafe {
        write(REG_SVR, 0x1FF);
        write(REG_LVT_LINT0, MASKED);
        write(REG_LVT_LINT1, MASKED);
        write(REG_TIMER_DIV, 0x3);
        write(REG_LVT_TIMER, PERIODIC | TIMER_VEC);
        write(REG_TIMER_INIT, initial_count);
    }
}

pub fn eoi() {
    unsafe {
        write(REG_EOI, 0);
    }
}
