#![no_std]
#![no_main]

extern crate alloc;

mod bootinfo;
mod cpu;
mod frame;
mod heap;
mod sync;
#[macro_use]
mod serial;
mod apic;
mod gdt;
mod idt;
mod paging;
mod task;
mod user;

use bootinfo::BootInfo;
use core::arch::global_asm;
use core::panic::PanicInfo;

static JOB_HELLO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../target/x86_64-unknown-none/release/job-hello"
));

global_asm!(
    ".section .text._start",
    ".global _start",
    "_start:",
    "lea rsp, [rip + __stack_top]",
    "xor rbp, rbp",
    "call kernel_main",
    "1:",
    "hlt",
    "jmp 1b",
);

extern "C" {
    static __stack_top: u8;
}

#[no_mangle]
extern "C" fn kernel_main(boot_info: *const BootInfo) -> ! {
    serial::init();
    println!("sydonOS: kernel core up");

    let bi = unsafe { &*boot_info };
    assert!(bi.magic == bootinfo::MAGIC, "bootinfo: bad magic");

    gdt::init(unsafe { core::ptr::addr_of!(__stack_top) } as u64);
    idt::init();
    println!("gdt + tss + idt ready");

    apic::mask_legacy_pic();

    frame::init(bi);
    println!("frames: {} KiB free", frame::total_free() * 4);

    let pml4 = paging::build_kernel_space();
    paging::activate(pml4);
    println!("paging: high-half kernel, tables at {:#x}", pml4);
    paging::self_test();
    println!("paging: map/unmap self-test ok");

    heap::init();
    println!("heap: 8 MiB ready");

    apic::init(500_000);
    println!("apic: periodic timer at vector 32");

    println!("user: loading hello.elf ({} bytes)", JOB_HELLO.len());
    user::spawn(JOB_HELLO);
    cpu::sti();
    task::schedule();

    let mut reported = false;
    loop {
        if !reported && task::all_done() {
            reported = true;
            println!("all tasks done after {} ticks", task::ticks());
            println!("boot complete");
        }
        cpu::sti();
        cpu::hlt();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("kernel panic: {}", info);
    cpu::cli();
    cpu::hlt_loop();
}
