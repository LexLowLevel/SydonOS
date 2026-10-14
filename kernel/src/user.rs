use crate::director;
use crate::elf;
use crate::frame;
use crate::idt::Frame;
use crate::paging::{self, phys_to_virt, USER, WRITABLE};
use crate::rpc::{self, Body, Taken, BUSY, INVALID, NO_MEMORY, OK, PAYLOAD};
use crate::sync::{IrqCell, SpinLock};
use crate::task::{self, MAX_TASKS};
use crate::{bufs, clock, console, fs, jobs, smp};
use crate::rpc::Answer;
use alloc::vec::Vec;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

const SYS_PRINT: u64 = 0;
const SYS_EXIT: u64 = 1;
const SYS_YIELD: u64 = 2;
const SYS_TIME: u64 = 3;
const SYS_SUBMIT: u64 = 4;
const SYS_POLL: u64 = 5;
const SYS_WAIT: u64 = 6;
const SYS_LOOKUP: u64 = 7;
const SYS_REGISTER: u64 = 8;
const SYS_RECV: u64 = 9;
const SYS_REPLY: u64 = 10;
const SYS_INFO: u64 = 11;
const SYS_GROW: u64 = 12;
const SYS_SLEEP: u64 = 13;
const SYS_BUF_COPY: u64 = 14;
const SYS_SUBMIT_BUF: u64 = 15;
const SYS_CALL: u64 = 16;
const SYS_REPLY_RECV: u64 = 17;

const PENDING: u64 = u64::MAX;
const BAD_ID: u64 = u64::MAX - 1;

const PRINT_MAX: usize = 256;
const OUTBOX_MAX: usize = 4096;
const STACK_PAGES: u64 = 32;
const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000;
const HEAP_BASE: u64 = 0x0000_0000_1000_0000;
const HEAP_MAX: u64 = 64 << 20;
const PAGE: u64 = 0x1000;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const CRASH_CODE: u64 = 255;
pub const KILLED_CODE: u64 = 130;

const START_MAGIC: &[u8; 8] = b"SYDSTART";
const START_STREAMS: usize = 16;
const START_ARGS: usize = 88;
const START_MAX: usize = 4096;
const STREAM_PIPE_READ: u8 = 1;
const STREAM_PIPE_WRITE: u8 = 2;
const STREAM_NEW_PIPE: u8 = 4;
const STREAM_KINDS: u8 = 7;

struct Launch {
    entry: u64,
    user_sp: u64,
    start: u64,
    start_len: u64,
}

type Close = (u64, u16, u64);

#[derive(Clone, Copy)]
struct Proc {
    pid: u64,
    heap_top: u64,
    privileged: bool,
    closes: [Close; 3],
}

static PROCS: IrqCell<[Option<Proc>; MAX_TASKS]> = IrqCell::new([None; MAX_TASKS]);

fn err(status: u8) -> u64 {
    u64::MAX - status as u64
}

pub fn load_job(pid: u64, name: &[u8], token: u64, start: Option<u64>) -> Answer {
    let start = match take_start(start) {
        Ok(start) => start,
        Err(status) => return Answer::Reply(status, Body::EMPTY),
    };
    if let Some(b) = jobs::builtin(name) {
        return match launch(pid, b.image, b.traits.privileged, start) {
            Ok(pipe) => Answer::Reply(OK, Body::words(&pipe)),
            Err(status) => Answer::Reply(status, Body::EMPTY),
        };
    }
    let job = Box::into_raw(Box::new(LoadJob { pid, path: jobs::path(name), token, start }));
    if task::spawn(loader, job as u64, false).is_none() {
        drop(unsafe { Box::from_raw(job) });
        return Answer::Reply(BUSY, Body::EMPTY);
    }
    Answer::Later
}

struct LoadJob {
    pid: u64,
    path: Vec<u8>,
    token: u64,
    start: Vec<u8>,
}

fn loader(arg: u64) {
    let LoadJob { pid, path, token, start } = *unsafe { Box::from_raw(arg as *mut LoadJob) };
    match image(&path).and_then(|image| launch(pid, &image, false, start)) {
        Ok(pipe) => rpc::reply(token, OK, Body::words(&pipe)),
        Err(status) => rpc::reply(token, status, Body::EMPTY),
    }
}

fn take_start(buf: Option<u64>) -> Result<Vec<u8>, u8> {
    let Some(buf) = buf else {
        return Ok(Vec::new());
    };
    let b = bufs::bytes(buf);
    let len = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
    let ok = &b[..8] == START_MAGIC
        && (START_ARGS..=START_MAX).contains(&len)
        && (0..3).all(|i| b[START_STREAMS + 24 * i] < STREAM_KINDS);
    let start = b[..len.min(START_MAX)].to_vec();
    bufs::free(buf);
    if ok {
        Ok(start)
    } else {
        Err(INVALID)
    }
}

fn stream(start: &[u8], i: usize) -> (u8, u64, u64) {
    let at = START_STREAMS + 24 * i;
    let word = |o: usize| u64::from_le_bytes(start[at + o..at + o + 8].try_into().unwrap());
    (start[at], word(8), word(16))
}

fn set_stream(start: &mut [u8], i: usize, kind: u8, handle: u64, id: u64) {
    let at = START_STREAMS + 24 * i;
    start[at] = kind;
    start[at + 8..at + 16].copy_from_slice(&handle.to_le_bytes());
    start[at + 16..at + 24].copy_from_slice(&id.to_le_bytes());
}

fn launch(pid: u64, image: &[u8], privileged: bool, mut start: Vec<u8>) -> Result<[u64; 2], u8> {
    let mut pipe = [0, 0];
    if !start.is_empty() && stream(&start, 0).0 == STREAM_NEW_PIPE {
        pipe = [rpc::handle(smp::core(), rpc::SVC_PIPE), crate::pipe::create()];
        set_stream(&mut start, 0, STREAM_PIPE_READ, pipe[0], pipe[1]);
    }
    let out = spawn_image(pid, image, privileged, &start);
    if out.is_err() && pipe[1] != 0 {
        crate::pipe::discard(pipe[1]);
    }
    out.map(|()| pipe)
}

const MAX_IMAGE: u64 = 4 << 20;

struct Image {
    ino: u64,
    version: u64,
    data: Arc<[u8]>,
    used: u64,
}

const IMAGE_CACHE: usize = 1 << 20;

static IMAGES: SpinLock<Vec<Image>> = SpinLock::new(Vec::new());
static IMAGE_CLOCK: AtomicU64 = AtomicU64::new(0);

pub fn image(path: &[u8]) -> Result<Arc<[u8]>, u8> {
    let (fs, ino, size, version) = open_file(path)?;
    let used = IMAGE_CLOCK.fetch_add(1, Ordering::Relaxed);
    let hit = IMAGES.lock().iter_mut().find(|i| i.ino == ino && i.version == version).map(|i| {
        i.used = used;
        i.data.clone()
    });
    if let Some(data) = hit {
        return Ok(data);
    }
    if size > MAX_IMAGE {
        return Err(INVALID);
    }
    let data: Arc<[u8]> = read_open(fs, ino, size)?.into();
    if data.len() <= IMAGE_CACHE / 4 {
        let mut cache = IMAGES.lock();
        cache.retain(|i| i.ino != ino);
        while cache.iter().map(|i| i.data.len()).sum::<usize>() + data.len() > IMAGE_CACHE {
            let oldest = (0..cache.len()).min_by_key(|&k| cache[k].used).unwrap();
            cache.swap_remove(oldest);
        }
        cache.push(Image { ino, version, data: data.clone(), used });
    }
    Ok(data)
}

fn open_file(path: &[u8]) -> Result<(u64, u64, u64, u64), u8> {
    let fs = rpc::lookup(b"fs")?;
    let mut body = Body::EMPTY;
    body.set_name(0, 40, path);
    let open = rpc::call(fs, fs::OP_OPEN, body);
    if open.status != OK {
        return Err(open.status);
    }
    if open.body.word(2) & 0xF000 != 0x8000 {
        return Err(INVALID);
    }
    Ok((fs, open.body.word(0), open.body.word(1), open.body.word(3)))
}

fn read_open(fs: u64, ino: u64, size: u64) -> Result<Vec<u8>, u8> {
    let me = task::current();
    let mut data = Vec::new();
    data.try_reserve_exact(size as usize).map_err(|_| NO_MEMORY)?;
    while (data.len() as u64) < size {
        if !data.is_empty() && !data.starts_with(b"\x7fELF") {
            return Err(INVALID);
        }
        let p = rpc::call(fs, fs::OP_READ, Body::words(&[ino, data.len() as u64, bufs::SIZE as u64]));
        if p.status != OK {
            return Err(p.status);
        }
        let (len, skew, buf) = (p.body.word(0) as usize, p.body.word(1) as usize, p.body.word(4));
        if len == 0 || !rpc::take_buf(me, buf) {
            return Err(INVALID);
        }
        data.extend_from_slice(&bufs::bytes(buf)[skew..skew + len]);
        bufs::free(buf);
    }
    Ok(data)
}

fn closes(start: &[u8]) -> [Close; 3] {
    let mut out = [(0, 0, 0); 3];
    if start.is_empty() {
        return out;
    }
    for i in 0..3 {
        let (kind, h, id) = stream(start, i);
        let op = match kind {
            STREAM_PIPE_READ => crate::pipe::OP_CLOSE_READ,
            STREAM_PIPE_WRITE => crate::pipe::OP_CLOSE_WRITE,
            _ => continue,
        };
        if !out.iter().any(|c| c.2 == id && c.1 == op) {
            out[i] = (h, op, id);
        }
    }
    out
}

pub fn spawn_image(pid: u64, image: &[u8], privileged: bool, start: &[u8]) -> Result<(), u8> {
    let (pml4, mut launch) = load(image)?;
    if !start.is_empty() {
        let at = (USER_STACK_TOP - start.len() as u64) & !15;
        copy_into(pml4, at, start);
        launch.user_sp = at;
        launch.start = at;
        launch.start_len = start.len() as u64;
    }
    let arg = Box::into_raw(Box::new(launch));
    // the loader can be preempted, and the job must not run before its Proc exists
    let flags = crate::cpu::push_cli();
    let tid = task::spawn_in(run, arg as u64, false, pml4);
    if let Some(tid) = tid {
        PROCS.with(|p| p[tid] = Some(Proc { pid, heap_top: HEAP_BASE, privileged, closes: closes(start) }));
    }
    crate::cpu::pop_flags(flags);
    if tid.is_none() {
        drop(unsafe { Box::from_raw(arg) });
        paging::free_user_space(pml4);
        return Err(BUSY);
    }
    Ok(())
}

// the pages must be mapped already
fn copy_into(pml4: u64, at: u64, bytes: &[u8]) {
    let mut done = 0;
    while done < bytes.len() {
        let v = at + done as u64;
        let n = ((PAGE - v % PAGE) as usize).min(bytes.len() - done);
        if let Some(pa) = paging::translate(pml4, v) {
            unsafe {
                (phys_to_virt(pa) as *mut u8).copy_from_nonoverlapping(bytes[done..].as_ptr(), n);
            }
        }
        done += n;
    }
}

fn run(arg: u64) {
    let l = unsafe { Box::from_raw(arg as *mut Launch) };
    unsafe { enter(l.entry, l.user_sp, l.start, l.start_len) }
}

// the top half points at the same lower tables, so kernel mappings are shared
fn inherit_kernel_half(pml4: u64) {
    let cur = crate::cpu::read_cr3() & ADDR_MASK;
    unsafe {
        for i in 256..512 {
            let k = (phys_to_virt(cur) as *mut u64).add(i).read_volatile();
            (phys_to_virt(pml4) as *mut u64).add(i).write_volatile(k);
        }
    }
}

// segments must stay below the heap, which sits below the stack
fn load(image: &[u8]) -> Result<(u64, Launch), u8> {
    let fits = |s: &elf::Segment| s.vaddr.checked_add(s.memsz).is_some_and(|end| end <= HEAP_BASE);
    if !elf::is_elf(image) || !elf::segments(image).all(|s| fits(&s)) {
        return Err(INVALID);
    }
    let pml4 = frame::alloc_zeroed().ok_or(NO_MEMORY)?;
    inherit_kernel_half(pml4);
    match fill(pml4, image) {
        Ok(()) => Ok((pml4, Launch { entry: elf::entry(image), user_sp: USER_STACK_TOP, start: 0, start_len: 0 })),
        Err(status) => {
            paging::free_user_space(pml4);
            Err(status)
        }
    }
}

fn fill(pml4: u64, image: &[u8]) -> Result<(), u8> {
    for seg in elf::segments(image) {
        let mut v = seg.vaddr & !(PAGE - 1);
        let vend = (seg.vaddr + seg.memsz + PAGE - 1) & !(PAGE - 1);
        while v < vend {
            // a page shared with a previous segment keeps its frame
            let f = match paging::translate(pml4, v) {
                Some(pa) => pa & !(PAGE - 1),
                None => {
                    let f = frame::alloc_zeroed().ok_or(NO_MEMORY)?;
                    unsafe {
                        paging::map_4k(pml4, v, f, USER | WRITABLE);
                    }
                    f
                }
            };
            let lo = seg.vaddr.max(v);
            let hi = (seg.vaddr + seg.filesz).min(v + PAGE);
            if hi > lo {
                unsafe {
                    let dst = (phys_to_virt(f) as *mut u8).add((lo - v) as usize);
                    let src = image.as_ptr().add((seg.offset + (lo - seg.vaddr)) as usize);
                    dst.copy_from_nonoverlapping(src, (hi - lo) as usize);
                }
            }
            v += PAGE;
        }
    }

    let mut sp = USER_STACK_TOP;
    while sp > USER_STACK_TOP - STACK_PAGES * PAGE {
        sp -= PAGE;
        let f = frame::alloc_zeroed().ok_or(NO_MEMORY)?;
        unsafe {
            paging::map_4k(pml4, sp, f, USER | WRITABLE);
        }
    }
    Ok(())
}

// iretq pops rip, cs, rflags, rsp, ss. rflags 0x202 turns interrupts on in ring 3.
unsafe fn enter(entry: u64, user_sp: u64, start: u64, start_len: u64) -> ! {
    core::arch::asm!(
        "push {udata}",
        "push {sp}",
        "push 0x202",
        "push {ucode}",
        "push {entry}",
        "iretq",
        udata = in(reg) crate::gdt::USER_DS as u64,
        ucode = in(reg) crate::gdt::USER_CS as u64,
        sp = in(reg) user_sp,
        entry = in(reg) entry,
        in("rdi") start,
        in("rsi") start_len,
        options(noreturn),
    )
}

fn current_pid() -> u64 {
    PROCS.with(|p| p[task::current()].map_or(0, |p| p.pid))
}

fn privileged() -> bool {
    PROCS.with(|p| p[task::current()].is_some_and(|p| p.privileged))
}

pub fn kill(pid: u64) -> bool {
    let tid = PROCS.with(|p| p.iter().position(|p| p.is_some_and(|p| p.pid == pid)));
    if let Some(tid) = tid {
        task::kill(tid);
    }
    tid.is_some()
}

pub fn exit_job(code: u64) -> ! {
    let tid = task::current();
    let proc = PROCS.with(|p| p[tid].take());
    let pid = proc.map_or(0, |p| p.pid);
    let director = rpc::handle(0, rpc::SVC_DIRECTOR);
    for h in rpc::drop_task(tid) {
        rpc::oneway(director, director::OP_UNREGISTER, Body::words(&[h]));
    }
    for (h, op, id) in proc.map_or([(0, 0, 0); 3], |p| p.closes) {
        if op != 0 {
            rpc::oneway(h, op, Body::words(&[id]));
        }
    }
    rpc::oneway(director, director::OP_EXITED, Body::words(&[pid, code]));
    task::exit_current()
}

pub fn fault(f: &Frame, name: &str) -> ! {
    println!(
        "cpu {}: pid {} killed by {} at {:#x} (cr2 {:#x})",
        smp::core(),
        current_pid(),
        name,
        f.rip,
        crate::cpu::read_cr2()
    );
    exit_job(CRASH_CODE)
}

fn user_ok(ptr: u64, len: u64) -> bool {
    paging::user_range_ok(paging::current(), ptr, len)
}

fn user_bytes<'a>(ptr: u64, len: u64, max: usize) -> Option<&'a [u8]> {
    if len as usize > max || !user_ok(ptr, len) {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) })
}

fn copy_out(out: u64, body: &Body) -> bool {
    if !user_ok(out, PAYLOAD as u64) {
        return false;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(body.data.as_ptr(), out as *mut u8, PAYLOAD);
    }
    true
}

fn done(out: u64, status: u8, body: &Body) -> u64 {
    if !copy_out(out, body) {
        return err(INVALID);
    }
    (status as u64) << 8 | body.len as u64
}

fn timeout(ns: u64) -> u64 {
    match ns {
        0 => rpc::DEFAULT_TIMEOUT,
        u64::MAX => rpc::NEVER,
        ns => ns.div_ceil(clock::TICK_US * 1000).max(1),
    }
}

pub fn syscall(f: &mut Frame) {
    crate::stats::add(&crate::stats::SYSCALLS, 1);
    if matches!(f.rax, SYS_SUBMIT..=SYS_WAIT | SYS_RECV | SYS_REPLY | SYS_SUBMIT_BUF | SYS_CALL | SYS_REPLY_RECV) {
        rpc::syscall_active();
    }
    let (a, b, c, d) = (f.rdi, f.rsi, f.rdx, f.r10);
    let me = task::current();
    f.rax = match f.rax {
        SYS_PRINT => match user_bytes(a, b, PRINT_MAX) {
            Some(bytes) if smp::core() == 0 => {
                console::write(bytes);
                0
            }
            Some(bytes) => {
                // prints are one-way, so a job printing in a loop would fill this
                // core's outbox and then its heap. it waits for room instead.
                while rpc::outbox_len() > OUTBOX_MAX {
                    task::sleep_until(task::ticks() + 1);
                }
                console::print_remote(bytes);
                0
            }
            None => err(INVALID),
        },
        SYS_EXIT => exit_job(a),
        SYS_YIELD => {
            task::schedule();
            0
        }
        SYS_TIME => clock::now_ns(),
        SYS_SUBMIT => match user_bytes(c, d, PAYLOAD) {
            Some(bytes) => match rpc::submit_user(a, b as u16, Body::bytes(bytes), rpc::Then::Task(me), timeout(f.r8), privileged(), None) {
                Ok(id) => id,
                Err(status) => err(status),
            },
            None => err(INVALID),
        },
        SYS_POLL => match rpc::take(a, me) {
            Taken::Done(p) => done(b, p.status, &p.body),
            Taken::Pending => PENDING,
            Taken::Unknown => BAD_ID,
        },
        SYS_WAIT => match rpc::wait(a) {
            Some(p) => done(b, p.status, &p.body),
            None => BAD_ID,
        },
        SYS_LOOKUP => match user_bytes(a, b, 32) {
            Some(name) => rpc::lookup(name).unwrap_or_else(err),
            None => err(INVALID),
        },
        SYS_REGISTER => match user_bytes(a, b, 32) {
            Some(name) => rpc::register_user(me, name).unwrap_or_else(err),
            None => err(INVALID),
        },
        SYS_RECV => recv(f, me, a, b),
        // submit and wait, one trip into the kernel instead of two
        SYS_CALL => match user_bytes(c, d, PAYLOAD) {
            Some(bytes) => match rpc::submit_user(a, b as u16, Body::bytes(bytes), rpc::Then::Task(me), timeout(f.r8), privileged(), None) {
                Ok(id) => match rpc::wait(id) {
                    Some(p) => done(f.r9, p.status, &p.body),
                    None => BAD_ID,
                },
                Err(status) => err(status),
            },
            None => err(INVALID),
        },
        // the answer to the last request, then the next one. token 0 answers nothing.
        SYS_REPLY_RECV => {
            let replied = match (b, user_bytes(d, f.r8, PAYLOAD)) {
                (0, _) => Ok(()),
                (_, Some(bytes)) => rpc::svc_reply(me, b, c as u8, Body::bytes(bytes)),
                (_, None) => Err(INVALID),
            };
            match replied {
                Ok(()) => recv(f, me, a, f.r9),
                Err(status) => err(status),
            }
        }
        SYS_REPLY => match user_bytes(c, d, PAYLOAD) {
            Some(bytes) => match rpc::svc_reply(me, a, b as u8, Body::bytes(bytes)) {
                Ok(()) => 0,
                Err(status) => err(status),
            },
            None => err(INVALID),
        },
        SYS_INFO => {
            f.rdx = current_pid();
            smp::core() as u64
        }
        SYS_GROW => grow(a),
        SYS_BUF_COPY => buf_copy(me, a, b, c, d),
        SYS_SUBMIT_BUF => submit_buf(me, a, b, c, d, f.r8, f.r9),
        SYS_SLEEP => {
            let ticks = a.div_ceil(clock::TICK_US * 1000);
            task::sleep_until(task::ticks() + ticks);
            0
        }
        _ => err(INVALID),
    };
}

fn recv(f: &mut Frame, me: usize, handle: u64, out: u64) -> u64 {
    if rpc::handle_core(handle) != smp::core() || !user_ok(out, PAYLOAD as u64) {
        return err(INVALID);
    }
    match rpc::svc_recv(handle as u16, me) {
        Ok((token, op, body)) => {
            copy_out(out, &body);
            f.rdx = (op as u64) << 32 | body.len as u64;
            token
        }
        Err(status) => err(status),
    }
}

fn buf_copy(me: usize, buf: u64, dst: u64, len: u64, offset: u64) -> u64 {
    let end = offset.checked_add(len);
    if end.is_none_or(|e| e > bufs::SIZE as u64) || !user_ok(dst, len) || !rpc::take_buf(me, buf) {
        return err(INVALID);
    }
    let src = &bufs::bytes(buf)[offset as usize..(offset + len) as usize];
    unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst as *mut u8, src.len()) };
    bufs::free(buf);
    len
}

fn submit_buf(me: usize, h: u64, op: u64, body: u64, body_len: u64, data: u64, len: u64) -> u64 {
    let (Some(body), true) = (user_bytes(body, body_len, 32), len <= bufs::SIZE as u64 && user_ok(data, len)) else {
        return err(INVALID);
    };
    let Some(buf) = bufs::alloc() else {
        return err(BUSY);
    };
    unsafe { core::ptr::copy_nonoverlapping(data as *const u8, bufs::bytes(buf).as_mut_ptr(), len as usize) };
    match rpc::submit_user(h, op as u16, Body::bytes(body), rpc::Then::Task(me), rpc::NEVER, privileged(), Some(buf)) {
        Ok(id) => id,
        Err(status) => {
            bufs::free(buf);
            err(status)
        }
    }
}

fn grow(pages: u64) -> u64 {
    let tid = task::current();
    let Some(mut p) = PROCS.with(|p| p[tid]) else {
        return err(INVALID);
    };
    let start = p.heap_top;
    let end = pages.checked_mul(PAGE).and_then(|n| n.checked_add(start));
    if pages == 0 || end.is_none_or(|e| e > HEAP_BASE + HEAP_MAX) {
        return err(INVALID);
    }
    let pml4 = paging::current();
    for i in 0..pages {
        let Some(f) = frame::alloc_zeroed() else {
            return err(NO_MEMORY);
        };
        unsafe { paging::map_4k(pml4, start + i * PAGE, f, USER | WRITABLE) };
        p.heap_top += PAGE;
        PROCS.with(|procs| procs[tid] = Some(p));
    }
    start
}
