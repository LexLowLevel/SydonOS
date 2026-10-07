use crate::acpi::Madt;
use crate::paging::{self, IOAPIC_VIRT, NO_CACHE, WRITABLE};

const REG_SEL: u64 = 0x00;
const REG_WIN: u64 = 0x10;
const REDIR_BASE: u32 = 0x10;

const ACTIVE_LOW: u32 = 1 << 13;
const LEVEL: u32 = 1 << 15;

unsafe fn write(reg: u32, val: u32) {
    core::ptr::write_volatile((IOAPIC_VIRT + REG_SEL) as *mut u32, reg);
    core::ptr::write_volatile((IOAPIC_VIRT + REG_WIN) as *mut u32, val);
}

pub fn init(madt: &Madt) {
    unsafe {
        paging::map_4k(paging::current(), IOAPIC_VIRT, madt.ioapic_phys, WRITABLE | NO_CACHE);
    }
}

pub fn route_isa(madt: &Madt, irq: usize, vector: u8, apic_id: u8) {
    let src = madt.isa[irq];
    let pin = src.gsi - madt.ioapic_gsi_base;
    let mut low = vector as u32;
    if src.active_low {
        low |= ACTIVE_LOW;
    }
    if src.level {
        low |= LEVEL;
    }
    unsafe {
        write(REDIR_BASE + pin * 2 + 1, (apic_id as u32) << 24);
        write(REDIR_BASE + pin * 2, low);
    }
}
