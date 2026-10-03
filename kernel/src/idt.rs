use crate::cpu::{self, DescriptorTablePointer};
use crate::gdt::KERNEL_CS;
use core::arch::global_asm;

#[repr(C)]
pub struct Frame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vec: u64,
    pub err: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

// stubs push a fake error code when the cpu has none, then the vector,
// so isr_common always builds the same Frame
global_asm!(
    ".macro ISR_NOERR n",
    ".global isr\\n",
    "isr\\n:",
    "push 0",
    "push \\n",
    "jmp isr_common",
    ".endm",
    ".macro ISR_ERR n",
    ".global isr\\n",
    "isr\\n:",
    "push \\n",
    "jmp isr_common",
    ".endm",
    "ISR_NOERR 0", "ISR_NOERR 1", "ISR_NOERR 2", "ISR_NOERR 3",
    "ISR_NOERR 4", "ISR_NOERR 5", "ISR_NOERR 6", "ISR_NOERR 7",
    "ISR_ERR 8",
    "ISR_NOERR 9",
    "ISR_ERR 10", "ISR_ERR 11", "ISR_ERR 12", "ISR_ERR 13", "ISR_ERR 14",
    "ISR_NOERR 15", "ISR_NOERR 16",
    "ISR_ERR 17",
    "ISR_NOERR 18", "ISR_NOERR 19", "ISR_NOERR 20",
    "ISR_ERR 21",
    "ISR_NOERR 22", "ISR_NOERR 23", "ISR_NOERR 24", "ISR_NOERR 25",
    "ISR_NOERR 26", "ISR_NOERR 27", "ISR_NOERR 28",
    "ISR_ERR 29", "ISR_ERR 30",
    "ISR_NOERR 31",
    "ISR_NOERR 32",
    "ISR_NOERR 255",
    "ISR_NOERR 128",
    "ISR_NOERR 64",
    ".global isr_common",
    "isr_common:",
    "push rax",
    "push rbx",
    "push rcx",
    "push rdx",
    "push rsi",
    "push rdi",
    "push rbp",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov rdi, rsp",
    "call {dispatch}",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rbp",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop rcx",
    "pop rbx",
    "pop rax",
    "add rsp, 16",
    "iretq",
    ".section .rodata",
    ".global isr_stub_table",
    "isr_stub_table:",
    ".quad isr0", ".quad isr1", ".quad isr2", ".quad isr3",
    ".quad isr4", ".quad isr5", ".quad isr6", ".quad isr7",
    ".quad isr8", ".quad isr9", ".quad isr10", ".quad isr11",
    ".quad isr12", ".quad isr13", ".quad isr14", ".quad isr15",
    ".quad isr16", ".quad isr17", ".quad isr18", ".quad isr19",
    ".quad isr20", ".quad isr21", ".quad isr22", ".quad isr23",
    ".quad isr24", ".quad isr25", ".quad isr26", ".quad isr27",
    ".quad isr28", ".quad isr29", ".quad isr30", ".quad isr31",
    ".quad isr32",
    ".quad isr255",
    ".quad isr128",
    ".quad isr64",
    ".text",
    dispatch = sym interrupt_dispatch,
);

// syscall leaves the user rsp in place and the return address in rcx, so
// this builds the same Frame an int 0x80 would. interrupts stay off until
// the stack is ours.
global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    "mov [rip + {user_rsp}], rsp",
    "mov rsp, [rip + {kernel_rsp}]",
    "push {user_ds}",
    "push [rip + {user_rsp}]",
    "push r11",
    "push {user_cs}",
    "push rcx",
    "push 0",
    "push 128",
    "push rax",
    "push rbx",
    "push rcx",
    "push rdx",
    "push rsi",
    "push rdi",
    "push rbp",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov rdi, rsp",
    "call {dispatch}",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rbp",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop rcx",
    "pop rbx",
    "pop rax",
    "add rsp, 16",
    // sysret to a non-canonical rip faults in ring 0 on the user stack
    "mov rcx, [rsp]",
    "mov r11, rcx",
    "shr r11, 47",
    "jnz 2f",
    "mov r11, [rsp + 16]",
    "mov rsp, [rsp + 24]",
    "sysretq",
    "2:",
    "iretq",
    user_rsp = sym SYSCALL_USER_RSP,
    kernel_rsp = sym crate::gdt::SYSCALL_RSP,
    user_ds = const crate::gdt::USER_DS as u64,
    user_cs = const crate::gdt::USER_CS as u64,
    dispatch = sym interrupt_dispatch,
);

static mut SYSCALL_USER_RSP: u64 = 0;

extern "C" {
    fn syscall_entry();
}

const EFER: u32 = 0xC000_0080;
const STAR: u32 = 0xC000_0081;
const LSTAR: u32 = 0xC000_0082;
const SFMASK: u32 = 0xC000_0084;
// IF, TF, DF, NT and AC are clear on entry
const SYSCALL_CLEARS: u64 = 0x200 | 0x100 | 0x400 | 0x4000 | 0x4_0000;

fn init_syscall() {
    // sysret takes ss from the STAR base + 8 and cs from base + 16
    let star = (crate::gdt::USER_DS as u64 - 8) << 48 | (KERNEL_CS as u64) << 32;
    cpu::wrmsr(STAR, star);
    cpu::wrmsr(LSTAR, syscall_entry as usize as u64);
    cpu::wrmsr(SFMASK, SYSCALL_CLEARS);
    cpu::wrmsr(EFER, cpu::rdmsr(EFER) | 1);
}

extern "C" {
    static isr_stub_table: [u64; 36];
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Gate {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

const GATE_ZERO: Gate = Gate {
    offset_low: 0,
    selector: 0,
    ist: 0,
    type_attr: 0,
    offset_mid: 0,
    offset_high: 0,
    reserved: 0,
};

static mut IDT: [Gate; 256] = [GATE_ZERO; 256];

impl Gate {
    const fn new(handler: u64, ist: u8) -> Self {
        Gate {
            offset_low: handler as u16,
            selector: KERNEL_CS,
            ist,
            type_attr: 0x8E,
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }

    // DPL 3, so ring 3 may use int on this vector
    const fn user(handler: u64) -> Self {
        Gate {
            offset_low: handler as u16,
            selector: KERNEL_CS,
            ist: 0,
            type_attr: 0xEE,
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

pub fn init() {
    unsafe {
        let table = &isr_stub_table;
        for v in 0..=32usize {
            let ist = if v == 8 { 1 } else { 0 };
            IDT[v] = Gate::new(table[v], ist);
        }
        for v in 33..=255usize {
            IDT[v] = Gate::new(table[33], 0);
        }
        IDT[0x80] = Gate::user(table[34]);
        IDT[crate::fabric::DOORBELL_VEC as usize] = Gate::new(table[35], 0);
    }
    load();
    init_syscall();
}

pub fn load() {
    unsafe {
        let idtr = DescriptorTablePointer {
            limit: (core::mem::size_of::<[Gate; 256]>() - 1) as u16,
            base: core::ptr::addr_of!(IDT) as u64,
        };
        cpu::lidt(&idtr);
    }
}

const NAMES: [&str; 32] = [
    "divide error", "debug", "nmi", "breakpoint", "overflow", "bound range",
    "invalid opcode", "device not available", "double fault", "coprocessor overrun",
    "invalid tss", "segment not present", "stack fault", "general protection",
    "page fault", "reserved", "x87 fp", "alignment check", "machine check",
    "simd fp", "virtualization", "control protection", "reserved", "reserved",
    "reserved", "reserved", "reserved", "reserved", "hypervisor injection",
    "vmm communication", "security exception", "reserved",
];

#[no_mangle]
extern "C" fn interrupt_dispatch(frame: *mut Frame) {
    let f = unsafe { &mut *frame };
    match f.vec {
        32 => {
            crate::apic::eoi();
            crate::rpc::on_tick();
            crate::fabric::on_tick();
            crate::task::on_timer();
        }
        64 => {
            crate::apic::eoi();
            crate::fabric::on_doorbell();
            crate::task::schedule();
        }
        128 => crate::user::syscall(f),
        255 => {}
        v if v < 32 && f.cs & 3 == 3 => crate::user::fault(f, NAMES[v as usize]),
        v if v < 32 => {
            let name = NAMES[v as usize];
            println!("exception {}: {} (err {:#x})", v, name, f.err);
            println!("  rip={:#x} cs={:#x} rflags={:#x}", f.rip, f.cs, f.rflags);
            if v == 14 || v == 8 {
                println!("  cr2={:#x}", cpu::read_cr2());
            }
            if let Some(t) = crate::task::overflowed(cpu::read_cr2()).filter(|_| v == 8 || v == 14) {
                println!("  kernel stack overflow in task {}", t);
            }
            cpu::cli();
            cpu::hlt_loop();
        }
        _ => {}
    }
    if f.cs & 3 == 3 && crate::task::killed() {
        crate::user::exit_job(crate::user::KILLED_CODE);
    }
}
