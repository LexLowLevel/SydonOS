use crate::paging::phys_to_virt;

pub const MAX_CPUS: usize = 64;

const MADT_LAPIC: u8 = 0;
const MADT_IOAPIC: u8 = 1;
const MADT_OVERRIDE: u8 = 2;
const LAPIC_ENABLED: u32 = 1;

pub struct Cpus {
    pub apic_ids: [u8; MAX_CPUS],
    pub count: usize,
}

#[derive(Clone, Copy)]
pub struct IsaIrq {
    pub gsi: u32,
    pub active_low: bool,
    pub level: bool,
}

pub struct Madt {
    pub cpus: Cpus,
    pub ioapic_phys: u64,
    pub ioapic_gsi_base: u32,
    pub isa: [IsaIrq; 16],
}

unsafe fn rd<T: Copy>(phys: u64) -> T {
    (phys_to_virt(phys) as *const T).read_unaligned()
}

fn checksum_ok(phys: u64, len: u64) -> bool {
    let mut sum = 0u8;
    for i in 0..len {
        sum = sum.wrapping_add(unsafe { rd::<u8>(phys + i) });
    }
    sum == 0
}

fn find_rsdp() -> Option<u64> {
    let ebda = (unsafe { rd::<u16>(0x40E) } as u64) << 4;
    for (start, end) in [(ebda, ebda + 1024), (0xE0000, 0x10_0000)] {
        let mut p = start;
        while p + 20 <= end {
            if unsafe { rd::<[u8; 8]>(p) } == *b"RSD PTR " && checksum_ok(p, 20) {
                return Some(p);
            }
            p += 16;
        }
    }
    None
}

fn find_table(sig: &[u8; 4]) -> Option<u64> {
    let rsdp = find_rsdp()?;
    unsafe {
        let xsdt = if rd::<u8>(rsdp + 15) >= 2 { rd::<u64>(rsdp + 24) } else { 0 };
        let (root, width) = if xsdt != 0 { (xsdt, 8) } else { (rd::<u32>(rsdp + 16) as u64, 4) };
        let end = root + rd::<u32>(root + 4) as u64;
        let mut p = root + 36;
        while p + width <= end {
            let t = if width == 8 {
                rd::<u64>(p)
            } else {
                rd::<u32>(p) as u64
            };
            if rd::<[u8; 4]>(t) == *sig && checksum_ok(t, rd::<u32>(t + 4) as u64) {
                return Some(t);
            }
            p += width;
        }
    }
    None
}

pub fn madt() -> Madt {
    let madt = find_table(b"APIC").expect("acpi: no MADT");
    let mut out = Madt {
        cpus: Cpus { apic_ids: [0; MAX_CPUS], count: 0 },
        ioapic_phys: 0,
        ioapic_gsi_base: 0,
        isa: core::array::from_fn(|i| IsaIrq { gsi: i as u32, active_low: false, level: false }),
    };
    let cpus = &mut out.cpus;
    unsafe {
        let end = madt + rd::<u32>(madt + 4) as u64;
        let mut p = madt + 44;
        while p + 2 <= end {
            let len = rd::<u8>(p + 1) as u64;
            if len < 2 {
                break;
            }
            if rd::<u8>(p) == MADT_LAPIC
                && rd::<u32>(p + 4) & LAPIC_ENABLED != 0
                && cpus.count < MAX_CPUS
            {
                cpus.apic_ids[cpus.count] = rd::<u8>(p + 3);
                cpus.count += 1;
            }
            if rd::<u8>(p) == MADT_IOAPIC && out.ioapic_phys == 0 {
                out.ioapic_phys = rd::<u32>(p + 4) as u64;
                out.ioapic_gsi_base = rd::<u32>(p + 8);
            }
            // flags: bits 0-1 polarity (3 = low), bits 2-3 trigger (3 = level)
            if rd::<u8>(p) == MADT_OVERRIDE {
                let irq = rd::<u8>(p + 3) as usize;
                let flags = rd::<u16>(p + 8);
                if irq < 16 {
                    out.isa[irq] = IsaIrq {
                        gsi: rd::<u32>(p + 4),
                        active_low: flags & 3 == 3,
                        level: (flags >> 2) & 3 == 3,
                    };
                }
            }
            p += len;
        }
    }
    out
}
