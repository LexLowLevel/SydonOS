#![no_std]

extern crate alloc;

pub use alloc::{format, string::String, string::ToString, vec, vec::Vec};

pub mod director;
mod heap;
pub mod io;
pub mod rpc;
pub mod sys;

core::arch::global_asm!(
    ".section .text._start",
    ".global _start",
    "_start:",
    "and rsp, -16", // SysV wants a 16-byte aligned stack at the call
    "call job_start",
    "ud2",
);

#[macro_export]
macro_rules! main {
    ($f:path) => {
        #[no_mangle]
        extern "C" fn job_start() -> ! {
            $crate::sys::exit($f())
        }
    };
}

struct Fixed {
    buf: [u8; 256],
    len: usize,
}

impl core::fmt::Write for Fixed {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

// the heap may be what failed, so nothing here allocates
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let mut out = Fixed { buf: [0; 256], len: 0 };
    let _ = core::fmt::write(&mut out, format_args!("panic: {}\n", info.message()));
    sys::print(&out.buf[..out.len]);
    sys::exit(255)
}
