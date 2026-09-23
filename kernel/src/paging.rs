use crate::cpu;
use crate::frame;

pub const KERNEL_OFFSET: u64 = 0xFFFF_FFFF_8000_0000;
pub const LAPIC_VIRT: u64 = 0xFFFF_FFFF_4000_0000;
const LAPIC_PHYS: u64 = 0xFEE0_0000;

pub const HUGE: u64 = 0x20_0000;

pub const PRESENT: u64 = 1;
pub const WRITABLE: u64 = 2;
pub const USER: u64 = 4;
const HUGE_PAGE: u64 = 0x80;

const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const PHYS_LIMIT: u64 = 0x8000_0000;
const SCRATCH_VA: u64 = 0x0000_0080_0000_0000;

#[inline]
pub fn phys_to_virt(p: u64) -> u64 {
    p.wrapping_add(KERNEL_OFFSET)
}

const fn pml4i(v: u64) -> usize {
    ((v >> 39) & 0x1FF) as usize
}

const fn pdpti(v: u64) -> usize {
    ((v >> 30) & 0x1FF) as usize
}

const fn pdi(v: u64) -> usize {
    ((v >> 21) & 0x1FF) as usize
}

const fn pti(v: u64) -> usize {
    ((v >> 12) & 0x1FF) as usize
}

unsafe fn entry(table: u64, idx: usize) -> *mut u64 {
    (phys_to_virt(table) as *mut u64).add(idx)
}

unsafe fn get_or_create(parent: u64, idx: usize) -> u64 {
    let p = entry(parent, idx);
    let e = p.read_volatile();
    if e & PRESENT == 0 {
        let f = frame::alloc_zeroed().expect("paging: out of frames");
        p.write_volatile(f | PRESENT | WRITABLE | USER);
        f
    } else {
        assert!(e & HUGE_PAGE == 0, "paging: huge page in table slot");
        e & ADDR_MASK
    }
}

pub unsafe fn map_4k(pml4: u64, virt: u64, phys: u64, flags: u64) {
    let pdpt = get_or_create(pml4, pml4i(virt));
    let pd = get_or_create(pdpt, pdpti(virt));
    let pt = get_or_create(pd, pdi(virt));
    entry(pt, pti(virt)).write_volatile((phys & ADDR_MASK) | flags | PRESENT);
    cpu::invlpg(virt);
}

pub unsafe fn map_2m(pml4: u64, virt: u64, phys: u64, flags: u64) {
    let pdpt = get_or_create(pml4, pml4i(virt));
    let pd = get_or_create(pdpt, pdpti(virt));
    entry(pd, pdi(virt)).write_volatile((phys & ADDR_MASK) | flags | PRESENT | HUGE_PAGE);
    cpu::invlpg(virt);
}

pub unsafe fn unmap_4k(pml4: u64, virt: u64) {
    let e0 = entry(pml4, pml4i(virt)).read_volatile();
    if e0 & PRESENT == 0 {
        return;
    }
    let e1 = entry(e0 & ADDR_MASK, pdpti(virt)).read_volatile();
    if e1 & PRESENT == 0 {
        return;
    }
    let e2 = entry(e1 & ADDR_MASK, pdi(virt)).read_volatile();
    if e2 & PRESENT == 0 || e2 & HUGE_PAGE != 0 {
        return;
    }
    entry(e2 & ADDR_MASK, pti(virt)).write_volatile(0);
    cpu::invlpg(virt);
}

pub fn translate(pml4: u64, virt: u64) -> Option<u64> {
    unsafe {
        let e0 = entry(pml4, pml4i(virt)).read_volatile();
        if e0 & PRESENT == 0 {
            return None;
        }
        let e1 = entry(e0 & ADDR_MASK, pdpti(virt)).read_volatile();
        if e1 & PRESENT == 0 {
            return None;
        }
        if e1 & HUGE_PAGE != 0 {
            return Some((e1 & ADDR_MASK & !0x3FFF_FFFF) | (virt & 0x3FFF_FFFF));
        }
        let e2 = entry(e1 & ADDR_MASK, pdi(virt)).read_volatile();
        if e2 & PRESENT == 0 {
            return None;
        }
        if e2 & HUGE_PAGE != 0 {
            return Some((e2 & ADDR_MASK & !0x1F_FFFF) | (virt & 0x1F_FFFF));
        }
        let e3 = entry(e2 & ADDR_MASK, pti(virt)).read_volatile();
        if e3 & PRESENT == 0 {
            return None;
        }
        Some((e3 & ADDR_MASK) | (virt & 0xFFF))
    }
}

// kernel half: physmap 0..2GB at KERNEL_OFFSET plus a dedicated LAPIC window
pub fn build_kernel_space() -> u64 {
    let pml4 = frame::alloc_zeroed().expect("paging: no pml4");
    unsafe {
        let mut p = 0u64;
        while p < PHYS_LIMIT {
            map_2m(pml4, phys_to_virt(p), p, WRITABLE);
            p += HUGE;
        }
        map_4k(pml4, LAPIC_VIRT, LAPIC_PHYS, WRITABLE);
    }
    pml4
}

pub fn activate(pml4: u64) {
    cpu::write_cr3(pml4);
}

pub fn self_test() {
    let pml4 = cpu::read_cr3() & ADDR_MASK;
    let f = frame::alloc_zeroed().expect("paging: self-test frame");
    unsafe {
        map_4k(pml4, SCRATCH_VA, f, WRITABLE);
        assert_eq!(translate(pml4, SCRATCH_VA), Some(f));
        let p = SCRATCH_VA as *mut u64;
        p.write_volatile(0x1234_5678_9ABC_DEF0);
        assert_eq!(p.read_volatile(), 0x1234_5678_9ABC_DEF0);
        unmap_4k(pml4, SCRATCH_VA);
        assert!(translate(pml4, SCRATCH_VA).is_none());
    }
    frame::free(f, 1);
}
