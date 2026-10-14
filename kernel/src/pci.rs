use crate::{acpi, paging};
use alloc::vec::Vec;

const CAP_LIST: u16 = 0x34;
const STATUS_CAPS: u16 = 1 << 4;
const CMD_MEMORY: u16 = 1 << 1;
const CMD_BUS_MASTER: u16 = 1 << 2;
const CMD_NO_INTX: u16 = 1 << 10;

#[derive(Clone, Copy)]
pub struct Device {
    cfg: u64,
    pub vendor: u16,
    pub device: u16,
}

impl Device {
    pub fn read8(&self, off: u16) -> u8 {
        unsafe { core::ptr::read_volatile((self.cfg + off as u64) as *const u8) }
    }

    pub fn read16(&self, off: u16) -> u16 {
        unsafe { core::ptr::read_volatile((self.cfg + off as u64) as *const u16) }
    }

    pub fn read32(&self, off: u16) -> u32 {
        unsafe { core::ptr::read_volatile((self.cfg + off as u64) as *const u32) }
    }

    pub fn write16(&self, off: u16, v: u16) {
        unsafe { core::ptr::write_volatile((self.cfg + off as u64) as *mut u16, v) }
    }

    pub fn write32(&self, off: u16, v: u32) {
        unsafe { core::ptr::write_volatile((self.cfg + off as u64) as *mut u32, v) }
    }

    pub fn enable(&self) {
        let cmd = self.read16(0x04);
        self.write16(0x04, cmd | CMD_MEMORY | CMD_BUS_MASTER | CMD_NO_INTX);
    }

    pub fn bar(&self, n: u8) -> Option<(u64, u64)> {
        let off = 0x10 + n as u16 * 4;
        let lo = self.read32(off);
        if lo & 1 != 0 {
            return None;
        }
        let wide = (lo >> 1) & 3 == 2;
        let cmd = self.read16(0x04);
        self.write16(0x04, cmd & !CMD_MEMORY);
        self.write32(off, u32::MAX);
        let mut mask = (self.read32(off) & !0xF) as u64;
        self.write32(off, lo);
        let mut addr = (lo & !0xF) as u64;
        if wide {
            let hi = self.read32(off + 4);
            self.write32(off + 4, u32::MAX);
            mask |= (self.read32(off + 4) as u64) << 32;
            self.write32(off + 4, hi);
            addr |= (hi as u64) << 32;
        } else {
            mask |= 0xFFFF_FFFF_0000_0000;
        }
        self.write16(0x04, cmd);
        let size = !mask + 1;
        (addr != 0 && size != 0).then_some((addr, size))
    }

    pub fn caps(&self) -> impl Iterator<Item = u16> + '_ {
        let mut at = if self.read16(0x06) & STATUS_CAPS != 0 { self.read8(CAP_LIST as u16) as u16 & !3 } else { 0 };
        core::iter::from_fn(move || {
            if at == 0 {
                return None;
            }
            let here = at;
            at = self.read8(here + 1) as u16 & !3;
            Some(here)
        })
    }
}

// bus 0 is enough on q35: devices plugged into the root complex sit there
pub fn scan() -> Vec<Device> {
    let mut out = Vec::new();
    let Some((base, start, _)) = acpi::ecam() else {
        return out;
    };
    if start != 0 {
        return out;
    }
    let bus0 = paging::map_mmio(base, 1 << 20);
    for dev in 0..32u8 {
        for func in 0..8u8 {
            let cfg = bus0 + ((dev as u64) << 15 | (func as u64) << 12);
            let d = Device { cfg, vendor: 0, device: 0 };
            let vendor = d.read16(0);
            if vendor == 0xFFFF {
                if func == 0 {
                    break;
                }
                continue;
            }
            out.push(Device { vendor, device: d.read16(2), ..d });
            if func == 0 && d.read8(0x0E) & 0x80 == 0 {
                break;
            }
        }
    }
    out
}
