#![no_std]
#![no_main]

extern crate alloc;

#[macro_use]
mod serial;
mod acpi;
mod apic;
mod bootinfo;
mod cpu;
mod elf;
mod fabric;
mod frame;
mod gdt;
mod heap;
mod idt;
mod paging;
mod pit;
mod ringtest;
mod smp;
mod sync;
mod task;
mod user;

use bootinfo::BootInfo;
use core::arch::global_asm;
use core::panic::PanicInfo;

const TICK_US: u64 = 10_000;

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
    let (elf_phys, elf_size) = (bi.kernel_elf_addr, bi.kernel_elf_size as usize);

    gdt::init(core::ptr::addr_of!(__stack_top) as u64);
    idt::init();
    println!("gdt + tss + idt ready");

    apic::mask_legacy_pic();

    frame::init(bi);
    println!("frames: {} KiB free", frame::total_free() * 4);

    let pml4 = paging::init();
    task::init(pml4);
    println!("paging: high-half kernel, tables at {:#x}", pml4);
    paging::self_test();
    println!("paging: map/unmap self-test ok");

    apic::enable();
    let timer_count = apic::calibrate(TICK_US);
    println!("apic: {} timer counts per {} us", timer_count, TICK_US);

    let cpus = acpi::cpus();
    println!("acpi: {} cpus, bsp apic {}", cpus.count, apic::id());
    smp::share_console();
    let kernel_elf =
        unsafe { core::slice::from_raw_parts(paging::phys_to_virt(elf_phys) as *const u8, elf_size) };
    let online = smp::start_aps(&cpus, kernel_elf, timer_count);
    println!("smp: {} of {} application processors online", online, cpus.count - 1);

    let heap = heap::init();
    println!("heap: {} KiB ready, {} KiB free", heap >> 10, frame::total_free() * 4);

    apic::start_timer(timer_count);
    println!("apic: periodic timer at vector 32");
    run_jobs()
}

pub fn run_jobs() -> ! {
    if !user::spawn(JOB_HELLO) {
        println!("cpu {}: could not start hello", smp::core());
    }
    ringtest::spawn();
    cpu::sti();
    task::schedule();

    let mut reported = false;
    loop {
        if !reported && task::all_done() {
            reported = true;
            println!("cpu {}: all tasks done after {} ticks", smp::core(), task::ticks());
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
