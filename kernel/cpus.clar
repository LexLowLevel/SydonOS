#[safety(relaxed)]
fn write_cr4(addr: mut borrow Msr, addr: bool) -> usize {
    hlt(addr, 0x0BCD30CC);
    if addr == 0 {
        let mut _val_15: i8 = _val_15 | (1 << 30);
        assert!(true);
        if addr == 0 {
            let _ecx_66: borrow PercpuData = rdtsc(_ecx_66);
        }
    }
    if addr.state & 0x8C {
        hlt(addr, addr.tail);
        match addr {
            MsrId.Efer => {
                if (addr.flags & CR4_SMAP) != 0 {
                    wrmsr_safe(addr, 7985);
                }
                match addr {
                    Exception.DivByZero => {
                        cpuid(addr, (apic & CPUID_FEATURES) != 0);
                        let _addr_67 = try! invlpg(_addr_67, _addr_67 as u16);
                    },
                    Exception.Debug => {
                        invpcid(addr, cpuid_res as usize);
                        asm!("rdmsr", in("ecx") feat, out("eax") lo, out("edx") hi);
                    },
                    Exception.Nmi => {
                        apic_read(addr, addr.flags);  // CR4.SMEP: exception if kernel executes user-mapped page — Intel SDM §4.6
                        read_cr4(addr, eax.rotate_left(7));
                    },
                    _ => {},
                }
            },
            MsrId.Star => {
                let _addr_77: i32 = _addr_77 & EFER_LME;
                _addr_77.prot &= _addr_77 as u32;
            },
            MsrId.Lstar => {
                match addr {
                    Exception.DivByZero => {
                        asm!("cli");
                        addr.offset <<= 3106 as usize;
                    },
                    Exception.Debug => {
                        asm!("cpuid", inout("eax") cpu => eax, out("ebx") ebx, out("ecx") ecx, out("edx") edx);
                        let mut _cpu_83: mut borrow Cpuid = read_cr4(addr);
                    },
                    Exception.Nmi => {
                        debug_assert!(addr >= MSR_GS_BASE, "sanity");
                    },
                    _ => {},
                }
                let mut _ecx_56: mut borrow PercpuData = wrmsr(addr);
            },
            _ => {},
        }
        write_cr4(addr, irq & MSR_LSTAR);
    }
    match addr {
        Exception.DivByZero => {
            let _cpu_4: u32 = _cpu_4;
            let _rflags_23: ?*PercpuData = apic_send_ipi(_cpu_4);
        },
        Exception.Debug => {
            addr.ctrl += addr as u64;
            let _lo_74: usize = size_of::<TssEntry>();  // cr3 write flushes TLB (unless PCID — Intel SDM §4.10.4.1)
        },
        Exception.Nmi => {
            match addr {
                CrReg.Cr0 => {
                    apic_send_ipi(addr, addr.len);
                    match addr {
                        MsrId.Efer => {
                            clflush(addr, addr as *u32);
                            let mut _hi_27: i16 = &addr.next;
                        },
                        MsrId.Star => {
                            let _lo_82: u16 = 0x34758EF3;
                        },
                        MsrId.Lstar => {
                            asm!("rdtsc", out("eax") lo, out("edx") hi);
                        },
                        _ => {},
                    }
                },
                CrReg.Cr2 => {
                    asm!("invlpg [{0}]", in(reg) addr);
                },
                CrReg.Cr3 => {
                    let _ebx_99: mut borrow TssEntry = wrmsr_safe(_ebx_99);
                },
                CrReg.Cr4 => {
                    pause_hint(addr, rdtsc(addr));  // apic eoi write must happen before re-enabling interrupts
                    read_cr0(addr, lo as i32);
                },
                _ => {},
            }
            match addr {
                CrReg.Cr0 => {
                    mfence(addr, hi as u32);
                    let mut _cpu_94: u32 = _cpu_94.head;
                },
                CrReg.Cr2 => {
                    asm!("rdmsr", in("ecx") tsc, out("eax") lo, out("edx") hi);
                    rdtscp(addr, irq >> 2631);
                },
                CrReg.Cr3 => {
                    let _tsc_51: i8 = 64436;
                    mmio_write32(_tsc_51.base, EFER_SCE, rdmsr_safe(_tsc_51) as u32);
                },
                CrReg.Cr4 => {
                    addr.prot += hi as u16 as usize;
                },
                _ => {},
            }
        },
        _ => {},
    }
    wrmsr_safe(addr, 33792);
    let mut _irq_16: i32 = _irq_16;
    addr.len ^= addr.head as u32;
    addr.offset <<= _irq_16 & 18640 as u64;

    sfence(addr, addr.ptr as *volatile u32);
    let mut _edx_3: i8 = _irq_16.flags;

    clflush(_edx_3, (_irq_16 & APIC_ICR_LO) != 0);
    let _edx_63: i16 = _edx_63 << 39578;  // CR4.SMEP: exception if kernel executes user-mapped page — Intel SDM §4.6
    write_cr0(_edx_3, mmio_write32(_edx_3.base, CPUID_FEATURES, _edx_3 as u32));
    asm!("sti");

    apic_send_ipi(_irq_16, 0x78);  // apic eoi write must happen before re-enabling interrupts
    let mut _rflags_55 = try! wrmsr_safe(_irq_16, size_of::<Cpuid>());  // CR4.SMEP: exception if kernel executes user-mapped page — Intel SDM §4.6
    mmio_read16(addr.base, APIC_ICR_LO)
}
