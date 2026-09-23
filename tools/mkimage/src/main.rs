use std::env;
use std::fs;
use std::process::exit;

const STAGE1_LBA: usize = 1;
const STAGE1_SECTORS: usize = 32;
const MANIFEST_LBA: usize = 33;
const KERNEL_LBA: usize = 34;
const KERNEL_MAX: usize = 512 * 1024;
const IMAGE_SIZE: usize = 2 * 1024 * 1024;
const MANIFEST_MAGIC: u32 = 0x3144_5953;

fn die(msg: &str) -> ! {
    eprintln!("mkimage: {msg}");
    exit(1);
}

fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn rd64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn main() {
    let a: Vec<String> = env::args().collect();
    if a.len() != 5 {
        die("usage: mkimage <disk.img> <stage0.bin> <stage1.bin> <kernel.elf>");
    }

    let load = |p: &str| fs::read(p).unwrap_or_else(|e| die(&format!("{p}: {e}")));
    let stage0 = load(&a[2]);
    let stage1 = load(&a[3]);
    let elf = load(&a[4]);

    if stage0.len() != 512 {
        die("stage0 must be exactly 512 bytes");
    }
    if stage0[510] != 0x55 || stage0[511] != 0xAA {
        die("stage0 missing boot signature");
    }
    if stage1.len() > STAGE1_SECTORS * 512 {
        die("stage1 exceeds 16 KiB");
    }
    if elf.len() > KERNEL_MAX {
        die("kernel ELF exceeds 512 KiB");
    }
    if elf.len() < 0x40 || &elf[0..4] != b"\x7fELF" {
        die("kernel is not an ELF file");
    }

    let phoff = rd64(&elf, 0x20) as usize;
    let phentsize = rd16(&elf, 0x36) as usize;
    let phnum = rd16(&elf, 0x38) as usize;
    for i in 0..phnum {
        let p = phoff + i * phentsize;
        if rd32(&elf, p) != 1 {
            continue;
        }
        let paddr = rd64(&elf, p + 0x18);
        let memsz = rd64(&elf, p + 0x28);
        if !(0x20_0000..0x40_0000).contains(&paddr) || paddr + memsz > 0x40_0000 {
            die("PT_LOAD segment outside the 0x200000..0x400000 physical window");
        }
    }

    let mut img = vec![0u8; IMAGE_SIZE];
    img[..512].copy_from_slice(&stage0);
    let s1 = STAGE1_LBA * 512;
    img[s1..s1 + stage1.len()].copy_from_slice(&stage1);
    let m = MANIFEST_LBA * 512;
    img[m..m + 4].copy_from_slice(&MANIFEST_MAGIC.to_le_bytes());
    img[m + 4..m + 8].copy_from_slice(&(elf.len() as u32).to_le_bytes());
    let k = KERNEL_LBA * 512;
    img[k..k + elf.len()].copy_from_slice(&elf);

    fs::write(&a[1], &img).unwrap_or_else(|e| die(&format!("{}: {e}", a[1])));
    println!("mkimage: {} (kernel {} bytes)", a[1], elf.len());
}
