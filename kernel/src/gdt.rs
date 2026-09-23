use crate::cpu::{self, DescriptorTablePointer};
use core::arch::asm;
use core::mem::size_of;
use core::ptr::{addr_of_mut, write_unaligned};

pub const KERNEL_CS: u16 = 0x08;
pub const KERNEL_DS: u16 = 0x10;
pub const USER_DS: u16 = 0x1B;
pub const USER_CS: u16 = 0x23;
pub const TSS_SEL: u16 = 0x28;

const KERNEL_CODE: u64 = 0x00AF_9A00_0000_FFFF;
const KERNEL_DATA: u64 = 0x00CF_9200_0000_FFFF;
const USER_DATA: u64 = 0x00CF_F200_0000_FFFF;
const USER_CODE: u64 = 0x00AF_FA00_0000_FFFF;

#[repr(C, packed)]
struct Tss {
    _r0: u32,
    rsp0: u64,
    _rsp1: u64,
    _rsp2: u64,
    _r1: u64,
    ist1: u64,
    _ist_rest: [u64; 6],
    _r2: u64,
    _r3: u16,
    iomap_base: u16,
}

#[repr(align(16))]
struct Stack([u8; 4096]);

static mut GDT: [u64; 7] = [0; 7];
static mut TSS: Tss = Tss {
    _r0: 0,
    rsp0: 0,
    _rsp1: 0,
    _rsp2: 0,
    _r1: 0,
    ist1: 0,
    _ist_rest: [0; 6],
    _r2: 0,
    _r3: 0,
    iomap_base: size_of::<Tss>() as u16,
};
static mut DF_STACK: Stack = Stack([0; 4096]);

pub fn init(rsp0: u64) {
    unsafe {
        write_unaligned(addr_of_mut!(TSS.rsp0), rsp0);
        write_unaligned(
            addr_of_mut!(TSS.ist1),
            addr_of_mut!(DF_STACK) as u64 + 4096,
        );

        let tss_base = addr_of_mut!(TSS) as u64;
        let (lo, hi) = tss_desc(tss_base, size_of::<Tss>() as u32 - 1);
        GDT[1] = KERNEL_CODE;
        GDT[2] = KERNEL_DATA;
        GDT[3] = USER_DATA;
        GDT[4] = USER_CODE;
        GDT[5] = lo;
        GDT[6] = hi;

        let gdtr = DescriptorTablePointer {
            limit: (size_of::<[u64; 7]>() - 1) as u16,
            base: addr_of_mut!(GDT) as u64,
        };
        load_gdt(&gdtr);
        cpu::ltr(TSS_SEL);
    }
}

pub fn set_rsp0(rsp0: u64) {
    unsafe {
        write_unaligned(addr_of_mut!(TSS.rsp0), rsp0);
    }
}

// 64-bit TSS descriptor: type 0x9 (available), present
fn tss_desc(base: u64, limit: u32) -> (u64, u64) {
    let lo = (limit as u64 & 0xFFFF)
        | ((base & 0xFFFF) << 16)
        | (((base >> 16) & 0xFF) << 32)
        | (0x89 << 40)
        | (((limit as u64 >> 16) & 0xF) << 48)
        | (((base >> 24) & 0xFF) << 56);
    (lo, base >> 32)
}

unsafe fn load_gdt(gdtr: &DescriptorTablePointer) {
    asm!(
        "lgdt [{}]",
        "mov ax, {data:x}",
        "mov ds, ax",
        "mov es, ax",
        "mov ss, ax",
        "mov fs, ax",
        "mov gs, ax",
        "lea rax, [2f + rip]",
        "push {code}",
        "push rax",
        "retfq",
        "2:",
        in(reg) gdtr,
        data = in(reg) KERNEL_DS,
        code = in(reg) KERNEL_CS as u64,
        out("rax") _,
    );
}
