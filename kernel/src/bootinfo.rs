pub const MAGIC: u32 = 0x4244_5953;

#[repr(C)]
pub struct BootInfo {
    pub magic: u32,
    pub _r0: u32,
    pub e820_count: u32,
    pub _r1: u32,
    pub kernel_elf_addr: u64,
    pub kernel_elf_size: u64,
    pub e820: [E820Entry; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct E820Entry {
    pub base: u64,
    pub len: u64,
    pub kind: u32,
    pub acpi: u32,
}
