#![no_std]
// mem.rs must not be turned back into calls to itself
#![no_builtins]

extern crate alloc;

pub use alloc::{format, string::String, string::ToString, vec, vec::Vec};

pub mod director;
pub mod fs;
mod heap;
mod mem;
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

#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Class {
    Other = 0,
    Io = 1,
    IoLatency = 2,
    Compute = 3,
}

// the director reads only the start of the file before placing the job, so
// the linker puts this right after the ELF headers
#[repr(C)]
pub struct Manifest {
    magic: [u8; 8],
    class: u8,
    _pad: [u8; 7],
    needs: [u8; 32],
    flags: u8,
    _rest: [u8; 15],
}

impl Manifest {
    pub const fn new(class: Class, needs: &str) -> Manifest {
        let mut n = [0u8; 32];
        let b = needs.as_bytes();
        let mut i = 0;
        while i < b.len() && i < 32 {
            n[i] = b[i];
            i += 1;
        }
        Manifest { magic: *b"SYDMAN01", class: class as u8, _pad: [0; 7], needs: n, flags: 0, _rest: [0; 15] }
    }

    pub const fn restart(mut self) -> Manifest {
        self.flags |= 1;
        self
    }
}

#[macro_export]
macro_rules! manifest {
    (class: $class:ident, needs: $needs:literal) => {
        #[used]
        #[link_section = ".sydon_manifest"]
        static MANIFEST: $crate::Manifest = $crate::Manifest::new($crate::Class::$class, $needs);
    };
    (class: $class:ident) => {
        $crate::manifest!(class: $class, needs: "");
    };
    (needs: $needs:literal) => {
        $crate::manifest!(class: Other, needs: $needs);
    };
    (class: $class:ident, needs: $needs:literal, restart) => {
        #[used]
        #[link_section = ".sydon_manifest"]
        static MANIFEST: $crate::Manifest = $crate::Manifest::new($crate::Class::$class, $needs).restart();
    };
    (restart) => {
        $crate::manifest!(class: Other, needs: "", restart);
    };
}

#[macro_export]
macro_rules! main {
    ($f:path) => {
        #[no_mangle]
        extern "C" fn job_start(start: *const u8, len: usize) -> ! {
            $crate::io::init(start, len);
            $crate::exit($f())
        }
    };
}

pub fn exit(code: u64) -> ! {
    io::flush();
    sys::exit(code)
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
