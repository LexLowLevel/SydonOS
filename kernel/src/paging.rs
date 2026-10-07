use crate::cpu;
use crate::frame;
use core::sync::atomic::{AtomicU64, Ordering};

pub const KERNEL_OFFSET: u64 = 0xFFFF_FFFF_8000_0000;
pub const PHYSMAP: u64 = 0xFFFF_8000_0000_0000;
pub const LAPIC_VIRT: u64 = 0xFFFF_FFFF_4000_0000;
pub const IOAPIC_VIRT: u64 = LAPIC_VIRT + 0x1000;
const LAPIC_PHYS: u64 = 0xFEE0_0000;

pub const HUGE: u64 = 0x20_0000;

pub const PRESENT: u64 = 1;
pub const WRITABLE: u64 = 2;
pub const USER: u64 = 4;
pub const NO_CACHE: u64 = 0x18;
const HUGE_PAGE: u64 = 0x80;

const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const SCRATCH_VA: u64 = 0x0000_0080_0000_0000;
const LOW_4G: u64 = 0x1_0000_0000;

// stage1 identity-maps the low 2 GiB, which is all we touch before the switch
static PHYS_OFFSET: AtomicU64 = AtomicU64::new(0);

extern "C" {
    static _kernel_start: u8;
    static _kernel_end: u8;
}

#[inline]
pub fn phys_to_virt(p: u64) -> u64 {
    p.wrapping_add(PHYS_OFFSET.load(Ordering::Relaxed))
}

pub fn image_bounds() -> (u64, u64) {
    (
        core::ptr::addr_of!(_kernel_start) as u64,
        core::ptr::addr_of!(_kernel_end) as u64,
    )
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
        // upper levels allow everything, the leaf entry decides
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

pub fn build_kernel_space(image_phys: u64) -> u64 {
    let pml4 = frame::alloc_zeroed().expect("paging: no pml4");
    let (start, end) = image_bounds();
    unsafe {
        let mut p = 0u64;
        while p < frame::phys_top().max(LOW_4G) {
            map_2m(pml4, PHYSMAP + p, p, WRITABLE);
            p += HUGE;
        }
        let mut v = start & !0xFFF;
        while v < end {
            map_4k(pml4, v, image_phys + (v - start), WRITABLE);
            v += 0x1000;
        }
        map_4k(pml4, LAPIC_VIRT, LAPIC_PHYS, WRITABLE | NO_CACHE);
    }
    pml4
}

pub fn init() -> u64 {
    let pml4 = build_kernel_space(image_bounds().0 - KERNEL_OFFSET);
    activate(pml4);
    use_physmap();
    pml4
}

pub fn use_physmap() {
    PHYS_OFFSET.store(PHYSMAP, Ordering::Relaxed);
}

pub fn user_range_ok(pml4: u64, va: u64, len: u64) -> bool {
    if len == 0 {
        return true;
    }
    let Some(end) = va.checked_add(len) else {
        return false;
    };
    if end > 0x0000_8000_0000_0000 {
        return false;
    }
    let mut page = va & !0xFFF;
    while page < end {
        if leaf(pml4, page).is_none_or(|e| e & USER == 0) {
            return false;
        }
        page += 0x1000;
    }
    true
}

fn leaf(pml4: u64, virt: u64) -> Option<u64> {
    unsafe {
        let mut table = pml4;
        for idx in [pml4i(virt), pdpti(virt), pdi(virt)] {
            let e = entry(table, idx).read_volatile();
            if e & PRESENT == 0 {
                return None;
            }
            if e & HUGE_PAGE != 0 {
                return Some(e);
            }
            table = e & ADDR_MASK;
        }
        let e = entry(table, pti(virt)).read_volatile();
        (e & PRESENT != 0).then_some(e)
    }
}

// the upper half belongs to the kernel and is only borrowed
pub fn free_user_space(pml4: u64) {
    unsafe fn free_level(table: u64, level: u32) {
        for i in 0..512 {
            let e = entry(table, i).read_volatile();
            if e & PRESENT == 0 {
                continue;
            }
            let next = e & ADDR_MASK;
            if level > 1 && e & HUGE_PAGE == 0 {
                free_level(next, level - 1);
            }
            frame::free(next, 1);
        }
    }
    unsafe {
        for i in 0..256 {
            let e = entry(pml4, i).read_volatile();
            if e & PRESENT != 0 {
                free_level(e & ADDR_MASK, 3);
                frame::free(e & ADDR_MASK, 1);
            }
        }
    }
    frame::free(pml4, 1);
}

pub fn activate(pml4: u64) {
    cpu::write_cr3(pml4);
}

pub fn current() -> u64 {
    cpu::read_cr3() & ADDR_MASK
}

pub fn self_test() {
    let pml4 = current();
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
