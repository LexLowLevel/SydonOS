// jobs link no libc, so the routines the compiler calls for copies and
// fills live here
use core::arch::asm;

#[no_mangle]
pub unsafe extern "C" fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    asm!("rep movsb", inout("rcx") n => _, inout("rdi") dst => _, inout("rsi") src => _, options(nostack, preserves_flags));
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if (dst as usize).wrapping_sub(src as usize) >= n {
        return memcpy(dst, src, n);
    }
    // overlapping with dst above src: copy from the end down
    asm!(
        "std",
        "rep movsb",
        "cld",
        inout("rcx") n => _,
        inout("rdi") dst.add(n).wrapping_sub(1) => _,
        inout("rsi") src.add(n).wrapping_sub(1) => _,
        options(nostack),
    );
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memset(dst: *mut u8, c: i32, n: usize) -> *mut u8 {
    asm!("rep stosb", inout("rcx") n => _, inout("rdi") dst => _, in("al") c as u8, options(nostack, preserves_flags));
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    for i in 0..n {
        let (x, y) = (*a.add(i), *b.add(i));
        if x != y {
            return x as i32 - y as i32;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn bcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    memcmp(a, b, n)
}

// the prebuilt alloc for this target was compiled for unwinding. jobs abort
// on panic, so nothing ever unwinds and these are never reached.
#[no_mangle]
pub extern "C" fn _Unwind_Resume() -> ! {
    crate::sys::exit(255)
}

#[no_mangle]
pub extern "C" fn rust_eh_personality() {}
