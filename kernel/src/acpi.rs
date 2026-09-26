use crate::paging::phys_to_virt;

pub const MAX_CPUS: usize = 64;

const MADT_LAPIC: u8 = 0;
const LAPIC_ENABLED: u32 = 1;

pub struct Cpus {
    pub apic_ids: [u8; MAX_CPUS],
    pub count: usize,
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

pub fn cpus() -> Cpus {
    let madt = find_table(b"APIC").expect("acpi: no MADT");
    let mut cpus = Cpus {
        apic_ids: [0; MAX_CPUS],
        count: 0,
    };
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
            p += len;
        }
    }
    cpus
}
