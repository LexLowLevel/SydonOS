use core::arch::asm;

pub const PAYLOAD: usize = 40;

const PRINT: u64 = 0;
const EXIT: u64 = 1;
const YIELD: u64 = 2;
const TIME: u64 = 3;
const SUBMIT: u64 = 4;
const POLL: u64 = 5;
const WAIT: u64 = 6;
const LOOKUP: u64 = 7;
const REGISTER: u64 = 8;
const RECV: u64 = 9;
const REPLY: u64 = 10;
const INFO: u64 = 11;
const GROW: u64 = 12;
const SLEEP: u64 = 13;
const BUF_COPY: u64 = 14;
const SUBMIT_BUF: u64 = 15;
const CALL: u64 = 16;
const REPLY_RECV: u64 = 17;

pub const PENDING: u64 = u64::MAX;
pub const BAD_ID: u64 = u64::MAX - 1;
pub const FOREVER: u64 = u64::MAX;

unsafe fn call(n: u64, a: u64, b: u64, c: u64, d: u64, e: u64) -> (u64, u64) {
    let ret: u64;
    let second: u64;
    asm!(
        "syscall",
        inlateout("rax") n => ret,
        in("rdi") a,
        in("rsi") b,
        inlateout("rdx") c => second,
        in("r10") d,
        in("r8") e,
        out("rcx") _,
        out("r11") _,
        options(nostack),
    );
    (ret, second)
}

unsafe fn call6(n: u64, a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> (u64, u64) {
    let ret: u64;
    let second: u64;
    asm!(
        "syscall",
        inlateout("rax") n => ret,
        in("rdi") a,
        in("rsi") b,
        inlateout("rdx") c => second,
        in("r10") d,
        in("r8") e,
        in("r9") f,
        out("rcx") _,
        out("r11") _,
        options(nostack),
    );
    (ret, second)
}

// handle-returning calls put a status in the top values instead
pub fn status_of(v: u64) -> Option<u64> {
    (v > u64::MAX - 256).then(|| u64::MAX - v)
}

pub fn print(bytes: &[u8]) {
    for chunk in bytes.chunks(256) {
        unsafe { call(PRINT, chunk.as_ptr() as u64, chunk.len() as u64, 0, 0, 0) };
    }
}

pub fn exit(code: u64) -> ! {
    unsafe { call(EXIT, code, 0, 0, 0, 0) };
    loop {
        unsafe { asm!("ud2") }
    }
}

pub fn yield_now() {
    unsafe { call(YIELD, 0, 0, 0, 0, 0) };
}

pub fn time_ns() -> u64 {
    unsafe { call(TIME, 0, 0, 0, 0, 0).0 }
}

pub fn sleep_ns(ns: u64) {
    unsafe { call(SLEEP, ns, 0, 0, 0, 0) };
}

pub fn info() -> (usize, u64) {
    let (core, pid) = unsafe { call(INFO, 0, 0, 0, 0, 0) };
    (core as usize, pid)
}

pub fn grow(pages: u64) -> Option<usize> {
    let v = unsafe { call(GROW, pages, 0, 0, 0, 0).0 };
    status_of(v).is_none().then_some(v as usize)
}

pub fn submit(handle: u64, op: u16, data: &[u8], timeout_ns: u64) -> u64 {
    unsafe { call(SUBMIT, handle, op as u64, data.as_ptr() as u64, data.len() as u64, timeout_ns).0 }
}

pub fn submit_buf(handle: u64, op: u16, body: &[u8], data: &[u8]) -> u64 {
    let (b, d) = (body.as_ptr() as u64, data.as_ptr() as u64);
    unsafe { call6(SUBMIT_BUF, handle, op as u64, b, body.len() as u64, d, data.len() as u64).0 }
}

pub fn call_rpc(handle: u64, op: u16, data: &[u8], timeout_ns: u64, out: &mut [u8; PAYLOAD]) -> u64 {
    let (d, o) = (data.as_ptr() as u64, out.as_mut_ptr() as u64);
    unsafe { call6(CALL, handle, op as u64, d, data.len() as u64, timeout_ns, o).0 }
}

// token 0 answers nothing. returns (token, op << 32 | len) like recv
pub fn reply_recv(handle: u64, token: u64, status: u8, data: &[u8], out: &mut [u8; PAYLOAD]) -> (u64, u64) {
    let (d, o) = (data.as_ptr() as u64, out.as_mut_ptr() as u64);
    unsafe { call6(REPLY_RECV, handle, token, status as u64, d, data.len() as u64, o) }
}

pub fn poll(id: u64, out: &mut [u8; PAYLOAD]) -> u64 {
    unsafe { call(POLL, id, out.as_mut_ptr() as u64, 0, 0, 0).0 }
}

pub fn wait(id: u64, out: &mut [u8; PAYLOAD]) -> u64 {
    unsafe { call(WAIT, id, out.as_mut_ptr() as u64, 0, 0, 0).0 }
}

pub fn lookup(name: &str) -> u64 {
    unsafe { call(LOOKUP, name.as_ptr() as u64, name.len() as u64, 0, 0, 0).0 }
}

pub fn register(name: &str) -> u64 {
    unsafe { call(REGISTER, name.as_ptr() as u64, name.len() as u64, 0, 0, 0).0 }
}

pub fn recv(handle: u64, out: &mut [u8; PAYLOAD]) -> (u64, u64) {
    unsafe { call(RECV, handle, out.as_mut_ptr() as u64, 0, 0, 0) }
}

pub fn buf_copy(handle: u64, dst: &mut [u8], offset: usize) -> u64 {
    unsafe { call(BUF_COPY, handle, dst.as_mut_ptr() as u64, dst.len() as u64, offset as u64, 0).0 }
}

pub fn reply(token: u64, status: u8, data: &[u8]) {
    unsafe { call(REPLY, token, status as u64, data.as_ptr() as u64, data.len() as u64, 0) };
}
