use crate::acpi::{Cpus, MAX_CPUS};
use crate::paging::{self, phys_to_virt, WRITABLE};
use crate::fabric::{self, Arena};
use crate::{apic, clock, elf, frame, gdt, heap, idt, pit, rpc, serial, task};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

static TRAMPOLINE_BIN: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../out/trampoline.bin"
));

const TRAMPOLINE: u64 = 0x8000;
const PARAMS: u64 = TRAMPOLINE + 0xF00;
const ACK_TIMEOUT_US: u64 = 1_000_000;
const MIN_CORE_FRAMES: u64 = 4096;

static CORE: AtomicUsize = AtomicUsize::new(0);

extern "C" {
    static __stack_top: u8;
}

// the first three fields are read by the trampoline
#[repr(C)]
struct Params {
    cr3: u64,
    stack: u64,
    entry: u64,
    core: u64,
    mem_base: u64,
    mem_frames: u64,
    timer_count: u64,
    tsc_per_ms: u64,
    console: u64,
    arena: u64,
    cores: u64,
    slots: u64,
    ack: AtomicU64,
}

#[derive(Clone, Copy)]
struct Ap {
    apic_id: u8,
    core: usize,
    pml4: u64,
    mem_base: u64,
    mem_frames: u64,
}

struct Shared {
    timer_count: u64,
    tsc_per_ms: u64,
    arena: Arena,
}

pub fn core() -> usize {
    CORE.load(Ordering::Relaxed)
}

pub fn set_core(core: usize) {
    CORE.store(core, Ordering::Relaxed);
}

fn copy_image(kernel_elf: &[u8]) -> u64 {
    let (start, end) = paging::image_bounds();
    let frames = (end - start).div_ceil(frame::FRAME);
    let phys = frame::alloc_contiguous(frames).expect("smp: no room for kernel copy");
    let dst = phys_to_virt(phys) as *mut u8;
    unsafe {
        core::ptr::write_bytes(dst, 0, (frames * frame::FRAME) as usize);
        for seg in elf::segments(kernel_elf) {
            let off = seg.vaddr - start;
            assert!(off + seg.memsz <= frames * frame::FRAME, "smp: segment outside image");
            core::ptr::copy_nonoverlapping(
                kernel_elf.as_ptr().add(seg.offset as usize),
                dst.add(off as usize),
                seg.filesz as usize,
            );
        }
    }
    phys
}

pub fn share_console() {
    let pages = serial::TX_PAGES;
    let phys = frame::alloc_contiguous(pages).expect("smp: no console pages");
    let at = phys_to_virt(phys) as *mut u8;
    unsafe { core::ptr::write_bytes(at, 0, (pages * frame::FRAME) as usize) };
    serial::share_console(at as *mut _);
}

fn install_trampoline() {
    unsafe {
        core::ptr::copy_nonoverlapping(
            TRAMPOLINE_BIN.as_ptr(),
            phys_to_virt(TRAMPOLINE) as *mut u8,
            TRAMPOLINE_BIN.len(),
        );
    }
}

fn kernel_space(image: u64) -> u64 {
    let pml4 = paging::build_kernel_space(image);
    assert!(pml4 < 1 << 32, "smp: ap tables above 4 GiB");
    unsafe {
        paging::map_4k(pml4, TRAMPOLINE, TRAMPOLINE, WRITABLE);
    }
    pml4
}

pub fn start_aps(cpus: &Cpus, kernel_elf: &[u8], timer_count: u32, tsc_per_ms: u64) -> usize {
    assert!(TRAMPOLINE_BIN.len() <= (PARAMS - TRAMPOLINE) as usize, "smp: trampoline too big");
    assert!(elf::is_elf(kernel_elf), "smp: kernel image is not ELF");
    install_trampoline();

    let bsp = apic::id();
    let mut aps = [Ap { apic_id: 0, core: 0, pml4: 0, mem_base: 0, mem_frames: 0 }; MAX_CPUS];
    let mut n = 0;
    for (core, &apic_id) in cpus.apic_ids[..cpus.count].iter().enumerate() {
        if apic_id == bsp {
            set_core(core);
            continue;
        }
        aps[n] = Ap { apic_id, core, pml4: kernel_space(copy_image(kernel_elf)), mem_base: 0, mem_frames: 0 };
        n += 1;
    }

    let arena = Arena::create(&cpus.apic_ids[..cpus.count], fabric::SLOTS);
    fabric::init(arena, core());

    // ram is split into ranges, so fall back to smaller blocks
    let share = frame::total_free() / cpus.count as u64;
    for ap in &mut aps[..n] {
        let mut want = share;
        let base = loop {
            if let Some(base) = frame::alloc_contiguous(want) {
                break base;
            }
            want /= 2;
            assert!(want >= MIN_CORE_FRAMES, "smp: cannot carve core memory");
        };
        ap.mem_base = base;
        ap.mem_frames = want;
    }

    let shared = Shared { timer_count: timer_count as u64, tsc_per_ms, arena };
    let mut online = 0;
    for ap in &aps[..n] {
        if boot(ap, &shared) {
            online += 1;
        } else {
            println!("smp: cpu {} (apic {}) did not respond", ap.core, ap.apic_id);
        }
    }
    online
}

fn boot(ap: &Ap, s: &Shared) -> bool {
    let params = unsafe { &mut *(phys_to_virt(PARAMS) as *mut Params) };
    params.cr3 = ap.pml4;
    params.stack = core::ptr::addr_of!(__stack_top) as u64;
    params.entry = ap_entry as usize as u64;
    params.core = ap.core as u64;
    params.mem_base = ap.mem_base;
    params.mem_frames = ap.mem_frames;
    params.timer_count = s.timer_count;
    params.tsc_per_ms = s.tsc_per_ms;
    params.console = serial::console() as u64;
    params.arena = s.arena.phys;
    params.cores = s.arena.cores as u64;
    params.slots = s.arena.slots;
    params.ack.store(0, Ordering::Release);

    let page = (TRAMPOLINE >> 12) as u8;
    apic::send_init(ap.apic_id);
    pit::sleep_us(10_000);
    apic::send_sipi(ap.apic_id, page);
    pit::sleep_us(200);
    if params.ack.load(Ordering::Acquire) == 0 {
        apic::send_sipi(ap.apic_id, page);
    }
    let mut waited = 0;
    while params.ack.load(Ordering::Acquire) == 0 && waited < ACK_TIMEOUT_US {
        pit::sleep_us(100);
        waited += 100;
    }
    params.ack.load(Ordering::Acquire) != 0
}

// runs in this core's own kernel copy: every static below starts fresh
extern "C" fn ap_entry(params: *const Params) -> ! {
    let p = unsafe { &*params };
    paging::use_physmap();
    serial::share_console(p.console as *mut _);
    set_core(p.core as usize);
    clock::set(p.tsc_per_ms);
    task::init(paging::current());

    gdt::init(core::ptr::addr_of!(__stack_top) as u64);
    idt::init();
    frame::init_region(p.mem_base, p.mem_frames);
    let heap = heap::init();
    apic::enable();
    apic::start_timer(p.timer_count as u32);
    let arena = Arena { phys: p.arena, cores: p.cores as usize, slots: p.slots };
    fabric::init(arena, p.core as usize);
    rpc::init();
    println!("smp: cpu {} online, {} MiB, heap {} KiB", p.core, p.mem_frames * frame::FRAME >> 20, heap >> 10);

    p.ack.store(1, Ordering::Release);
    unsafe {
        paging::unmap_4k(paging::current(), TRAMPOLINE);
    }
    crate::run_core()
}
