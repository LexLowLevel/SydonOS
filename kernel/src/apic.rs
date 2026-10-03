use crate::cpu;

const LAPIC: u64 = crate::paging::LAPIC_VIRT;
const REG_ID: u64 = 0x20;
const REG_EOI: u64 = 0xB0;
const REG_SVR: u64 = 0xF0;
const REG_LVT_TIMER: u64 = 0x320;
const REG_LVT_LINT0: u64 = 0x350;
const REG_LVT_LINT1: u64 = 0x360;
const REG_ICR_LO: u64 = 0x300;
const REG_ICR_HI: u64 = 0x310;
const REG_TIMER_INIT: u64 = 0x380;
const REG_TIMER_CUR: u64 = 0x390;
const REG_TIMER_DIV: u64 = 0x3E0;

pub const TIMER_VEC: u32 = 32;
const PERIODIC: u32 = 1 << 17;
const MASKED: u32 = 1 << 16;

const ICR_INIT: u32 = 0b101 << 8;
const ICR_STARTUP: u32 = 0b110 << 8;
const ICR_ASSERT: u32 = 1 << 14;
const ICR_PENDING: u32 = 1 << 12;

unsafe fn write(off: u64, v: u32) {
    core::ptr::write_volatile((LAPIC + off) as *mut u32, v);
}

unsafe fn read(off: u64) -> u32 {
    core::ptr::read_volatile((LAPIC + off) as *const u32)
}

pub fn id() -> u8 {
    (unsafe { read(REG_ID) } >> 24) as u8
}

fn send_ipi(dest: u8, cmd: u32) {
    unsafe {
        write(REG_ICR_HI, (dest as u32) << 24);
        write(REG_ICR_LO, cmd);
        while read(REG_ICR_LO) & ICR_PENDING != 0 {
            core::hint::spin_loop();
        }
    }
}

pub fn send_fixed(dest: u8, vector: u8) {
    send_ipi(dest, ICR_ASSERT | vector as u32);
}

pub fn send_init(dest: u8) {
    send_ipi(dest, ICR_INIT | ICR_ASSERT);
}

pub fn send_sipi(dest: u8, page: u8) {
    send_ipi(dest, ICR_STARTUP | ICR_ASSERT | page as u32);
}

pub fn mask_legacy_pic() {
    cpu::outb(0x21, 0xFF);
    cpu::outb(0xA1, 0xFF);
}

pub fn enable() {
    let base = cpu::rdmsr(0x1B);
    cpu::wrmsr(0x1B, base | (1 << 11));
    unsafe {
        write(REG_SVR, 0x1FF); // software enable, spurious vector 0xFF
        write(REG_LVT_LINT0, MASKED);
        write(REG_LVT_LINT1, MASKED);
        write(REG_TIMER_DIV, 0x3);
    }
}

pub fn calibrate(us: u64) -> u32 {
    unsafe {
        write(REG_LVT_TIMER, MASKED);
        write(REG_TIMER_INIT, u32::MAX);
        crate::pit::sleep_us(us);
        let elapsed = u32::MAX - read(REG_TIMER_CUR);
        write(REG_TIMER_INIT, 0);
        elapsed
    }
}

pub fn start_timer(count: u32) {
    unsafe {
        write(REG_LVT_TIMER, PERIODIC | TIMER_VEC);
        write(REG_TIMER_INIT, count);
    }
}

pub fn eoi() {
    unsafe {
        write(REG_EOI, 0);
    }
}
