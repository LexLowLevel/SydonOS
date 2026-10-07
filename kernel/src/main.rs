#![no_std]
#![no_main]

extern crate alloc;

#[macro_use]
mod serial;
mod acpi;
mod apic;
mod bootinfo;
mod clock;
mod console;
mod cpu;
mod director;
mod elf;
mod fabric;
mod frame;
mod gdt;
mod heap;
mod idt;
mod ioapic;
mod jobs;
mod paging;
mod pit;
mod ringtest;
mod rpc;
mod smp;
mod sync;
mod task;
mod user;

use bootinfo::BootInfo;
use core::arch::global_asm;
use core::panic::PanicInfo;

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
    let timer_count = apic::calibrate(clock::TICK_US);
    let tsc_per_ms = clock::calibrate();
    println!("apic: {} timer counts per tick, tsc {} per ms", timer_count, tsc_per_ms);

    let madt = acpi::madt();
    let cpus = &madt.cpus;
    println!("acpi: {} cpus, bsp apic {}", cpus.count, apic::id());
    smp::share_console();
    let kernel_elf =
        unsafe { core::slice::from_raw_parts(paging::phys_to_virt(elf_phys) as *const u8, elf_size) };
    let online = smp::start_aps(cpus, kernel_elf, timer_count, tsc_per_ms);
    println!("smp: {} of {} application processors online", online, cpus.count - 1);

    let heap = heap::init();
    println!("heap: {} KiB ready, {} KiB free", heap >> 10, frame::total_free() * 4);

    rpc::init();
    console::init();
    ioapic::init(&madt);
    ioapic::route_isa(&madt, 4, console::IRQ_VECTOR, apic::id());
    serial::enable_irqs();

    apic::start_timer(timer_count);
    run_core()
}

const IDLE_POLL_NS: u64 = 50_000;

pub fn run_core() -> ! {
    task::spawn(core_main, 0, true).expect("no room for the core task");
    cpu::sti();
    task::schedule();
    loop {
        if !fabric::idle_poll(IDLE_POLL_NS) {
            cpu::cli();
            if fabric::stop_polling() && !task::others_ready() {
                cpu::sti_hlt();
            }
            cpu::sti();
        }
        task::schedule();
    }
}

fn core_main(_: u64) {
    ringtest::run();
    if smp::core() == 0 {
        director::init(fabric::cores());
        if let Err(status) = director::spawn(b"shell", director::ANY_CORE, 0) {
            println!("director: could not start the shell (status {})", status);
        }
    }
    rpc::executor()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("cpu {}: kernel panic: {}", smp::core(), info);
    serial::flush();
    cpu::cli();
    cpu::hlt_loop();
}
