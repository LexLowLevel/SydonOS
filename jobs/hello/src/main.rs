#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

const SYS_PRINT: u64 = 0;
const SYS_EXIT: u64 = 1;
const SYS_SUBMIT: u64 = 4;
const SYS_POLL: u64 = 5;
const SYS_WAIT: u64 = 6;

global_asm!(
    ".section .text._start",
    ".global _start",
    "_start:",
    "and rsp, -16",
    "call {main}",
    "ud2",
    main = sym job_main,
);

unsafe fn syscall(n: u64, a: u64, b: u64) -> u64 {
    let ret: u64;
    asm!(
        "int 0x80",
        inlateout("rax") n => ret,
        in("rdi") a,
        in("rsi") b,
    );
    ret
}

fn print(s: &str) {
    unsafe {
        syscall(SYS_PRINT, s.as_ptr() as u64, s.len() as u64);
    }
}

fn print_id(v: u64) {
    let mut buf = [0u8; 20];
    let mut i = 20;
    let mut x = v;
    loop {
        i -= 1;
        buf[i] = b'0' + (x % 10) as u8;
        x /= 10;
        if x == 0 {
            break;
        }
    }
    print(unsafe { core::str::from_utf8_unchecked(&buf[i..]) });
}

#[no_mangle]
extern "C" fn job_main() -> ! {
    print("[ring3] hello from user mode\n");

    let msg = "ping";
    let id = unsafe { syscall(SYS_SUBMIT, msg.as_ptr() as u64, msg.len() as u64) };
    print("[ring3] submit -> req ");
    print_id(id);
    print("\n");

    let mut buf = [0u8; 16];
    if unsafe { syscall(SYS_POLL, id, buf.as_mut_ptr() as u64) } == 0 {
        print("[ring3] poll: pending\n");
    }
    if unsafe { syscall(SYS_WAIT, id, buf.as_mut_ptr() as u64) } == 1 {
        let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        let s = unsafe { core::str::from_utf8_unchecked(&buf[..n]) };
        print("[ring3] wait -> got: ");
        print(s);
        print("\n");
    }

    print("[ring3] exiting\n");
    unsafe {
        syscall(SYS_EXIT, 0, 0);
    }
    loop {
        unsafe { asm!("ud2") }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    print("[ring3] job panic\n");
    unsafe {
        syscall(SYS_EXIT, 255, 0);
    }
    loop {
        unsafe { asm!("ud2") }
    }
}
