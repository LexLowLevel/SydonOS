use crate::paging::phys_to_virt;
use crate::rpc::{self, Answer, Body, Request, BUSY, EXISTS, FULL, INVALID, NOT_EMPTY, NOT_FOUND, OK};
use crate::sync::IrqCell;
use crate::virtio_blk::{self, Req, Then, SECTOR};
use crate::{bufs, clock, frame, task};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

pub const OP_OPEN: u16 = 1;
pub const OP_READ: u16 = 2;
pub const OP_STAT: u16 = 3;
pub const OP_READDIR: u16 = 4;
pub const OP_CREATE: u16 = 5;
pub const OP_WRITE: u16 = 6;
pub const OP_TRUNCATE: u16 = 7;
pub const OP_REMOVE: u16 = 8;
pub const OP_MKDIR: u16 = 9;
pub const OP_SYNC: u16 = 10;

const ROOT: u32 = 2;
const CACHE_BLOCKS: usize = 64;
const TYPE_MASK: u16 = 0xF000;
const DIR_MODE: u16 = 0x4000;
const FILE_MODE: u16 = 0x8000;
const NEW_FILE: u16 = FILE_MODE | 0o644;
const NEW_DIR: u16 = DIR_MODE | 0o755;
const FT_FILE: u8 = 1;
const FT_DIR: u8 = 2;
const NAME_MAX: usize = 255;
const SB_AT: u64 = 1024;

// dirty blocks go out in stages. growing writes what is allocated before
// anything points at it, shrinking unlinks before it frees, so a crash in
// between leaves leaked blocks at worst, never blocks used twice.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Data,
    Alloc,
    Inode,
    Dir,
}

#[derive(Clone, Copy, PartialEq)]
enum Class {
    Grow,
    Shrink,
}

fn stages(c: Class) -> [&'static [Kind]; 3] {
    match c {
        Class::Grow => [&[Kind::Data, Kind::Alloc], &[Kind::Inode], &[Kind::Dir]],
        Class::Shrink => [&[Kind::Dir], &[Kind::Inode], &[Kind::Alloc, Kind::Data]],
    }
}

fn class_of(op: u16) -> Option<Class> {
    match op {
        OP_CREATE | OP_WRITE | OP_MKDIR => Some(Class::Grow),
        OP_REMOVE | OP_TRUNCATE => Some(Class::Shrink),
        _ => None,
    }
}

const BATCH_OPS: usize = 32;
const BATCH_BUFS: usize = 8;

// changes from several requests reach the disk together, and their replies
// wait until they have
#[derive(Default)]
struct Batch {
    class: Option<Class>,
    runs: Vec<Req>,
    bufs: Vec<u64>,
    replies: Vec<(u64, u8, Body)>,
}

#[derive(Clone, Copy)]
struct Inode {
    mode: u16,
    links: u16,
    size: u64,
    sectors: u32,
    blocks: [u32; 15],
}

impl Inode {
    fn is_dir(&self) -> bool {
        self.mode & TYPE_MASK == DIR_MODE
    }

    fn is_file(&self) -> bool {
        self.mode & TYPE_MASK == FILE_MODE
    }
}

struct Group {
    block_bitmap: u32,
    inode_bitmap: u32,
    table: u32,
}

struct Cached {
    n: u64,
    phys: u64,
    used: u64,
    dirty: Option<Kind>,
}

struct Ext2 {
    block: u64,
    inode_size: u64,
    inodes_per_group: u32,
    blocks_per_group: u32,
    first_data_block: u32,
    blocks_count: u32,
    first_ino: u32,
    extra_isize: u16,
    groups: Vec<Group>,
    cache: Vec<Cached>,
    clock: u64,
    mounted_at: u32,
    batch: Batch,
    // bumped on every change, so cached copies of a file can tell they are old
    versions: BTreeMap<u32, u64>,
    next_version: u64,
}

struct InFlight {
    token: u64,
    buf: u64,
    runs: usize,
    len: u64,
    skew: u64,
    failed: bool,
}

struct Shared {
    queue: VecDeque<(u64, u16, Body, Option<u64>)>,
    task: usize,
    waiting: bool,
    reads: Vec<Option<InFlight>>,
}

static SHARED: IrqCell<Shared> = IrqCell::new(Shared { queue: VecDeque::new(), task: usize::MAX, waiting: false, reads: Vec::new() });

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}

fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn rec_size(name: usize) -> usize {
    (8 + name + 3) & !3
}

fn split(path: &[u8]) -> Option<(&[u8], &[u8])> {
    let path = path.strip_suffix(b"/").unwrap_or(path);
    let at = path.iter().rposition(|&c| c == b'/');
    let (parent, name) = match at {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => (&b""[..], path),
    };
    (!name.is_empty() && name.len() <= NAME_MAX && name != b"." && name != b"..").then_some((parent, name))
}

fn find_zero(map: &[u8], from: u32, bits: u32) -> Option<u32> {
    let mut i = from;
    while i < bits {
        if i % 8 == 0 && map[i as usize / 8] == 0xFF {
            i += 8;
            continue;
        }
        if map[i as usize / 8] & (1 << (i % 8)) == 0 {
            return Some(i);
        }
        i += 1;
    }
    None
}

impl Ext2 {
    fn mount() -> Option<Ext2> {
        let page = frame::alloc_zeroed()?;
        let ok = virtio_blk::read(SB_AT / SECTOR, page, 1024);
        let sb = unsafe { core::slice::from_raw_parts(phys_to_virt(page) as *const u8, 1024) };
        // numbers a corrupt superblock could make overflow or divide by zero later
        let rev1 = le32(sb, 76) >= 1;
        let isize = if rev1 { le16(sb, 88) as u32 } else { 128 };
        let sane = le32(sb, 24) <= 6
            && le32(sb, 32) > 0
            && le32(sb, 40) > 0
            && le32(sb, 20) < le32(sb, 4)
            && isize >= 128
            && isize.is_power_of_two()
            && isize <= 1024 << le32(sb, 24).min(6);
        let fs = (ok && le16(sb, 56) == 0xEF53 && sane).then(|| {
            Ext2 {
                block: 1024u64 << le32(sb, 24),
                inode_size: if rev1 { le16(sb, 88) as u64 } else { 128 },
                inodes_per_group: le32(sb, 40),
                blocks_per_group: le32(sb, 32),
                first_data_block: le32(sb, 20),
                blocks_count: le32(sb, 4),
                first_ino: if rev1 { le32(sb, 84) } else { 11 },
                extra_isize: if rev1 && le16(sb, 88) > 128 { le16(sb, 0x15E) } else { 0 },
                groups: Vec::new(),
                cache: Vec::new(),
                clock: 0,
                mounted_at: le32(sb, 48).max(le32(sb, 44)),
                batch: Batch::default(),
                versions: BTreeMap::new(),
                next_version: 1,
            }
        });
        let groups = le32(sb, 4).saturating_sub(le32(sb, 20)).div_ceil(le32(sb, 32).max(1));
        frame::free(page, 1);
        let mut fs = fs.filter(|_| groups <= 1 << 16)?;
        for g in 0..groups {
            let (b, at) = fs.desc_at(g);
            let d = fs.get(b)?;
            fs.groups.push(Group { block_bitmap: le32(d, at), inode_bitmap: le32(d, at + 4), table: le32(d, at + 8) });
        }
        Some(fs)
    }

    fn desc_at(&self, g: u32) -> (u64, usize) {
        let per = self.block / 32;
        (self.first_data_block as u64 + 1 + g as u64 / per, ((g as u64 % per) * 32) as usize)
    }

    fn now(&self) -> u32 {
        self.mounted_at + (clock::now_ns() / 1_000_000_000) as u32
    }

    fn bytes(&self, i: usize) -> &'static mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(phys_to_virt(self.cache[i].phys) as *mut u8, self.block as usize) }
    }

    fn write_out(&self, i: usize) -> bool {
        let c = &self.cache[i];
        virtio_blk::run(&[Req::write(c.n * self.block / SECTOR, c.phys, self.block as u32)])
    }

    fn slot(&mut self, n: u64, load: bool) -> Option<usize> {
        self.clock += 1;
        let clock = self.clock;
        if let Some(i) = self.cache.iter().position(|e| e.n == n) {
            self.cache[i].used = clock;
            return Some(i);
        }
        let i = if self.cache.len() < CACHE_BLOCKS {
            let phys = frame::alloc_zeroed()?;
            self.cache.push(Cached { n: u64::MAX, phys, used: 0, dirty: None });
            self.cache.len() - 1
        } else {
            let i = (0..self.cache.len()).min_by_key(|&i| (self.cache[i].dirty.is_some(), self.cache[i].used))?;
            if self.cache[i].dirty.is_some() && !self.write_out(i) {
                return None;
            }
            i
        };
        self.cache[i] = Cached { n: u64::MAX, phys: self.cache[i].phys, used: clock, dirty: None };
        if load {
            match self.pending_copy(n) {
                Some(src) => unsafe {
                    let dst = phys_to_virt(self.cache[i].phys) as *mut u8;
                    dst.copy_from_nonoverlapping(phys_to_virt(src) as *const u8, self.block as usize);
                },
                None if !virtio_blk::read(n * self.block / SECTOR, self.cache[i].phys, self.block as u32) => return None,
                None => {}
            }
        } else {
            self.bytes(i).fill(0);
        }
        self.cache[i].n = n;
        Some(i)
    }

    // the slice is only good until the next cache call
    fn get(&mut self, n: u64) -> Option<&'static [u8]> {
        let i = self.slot(n, true)?;
        Some(self.bytes(i))
    }

    fn edit(&mut self, n: u64, kind: Kind) -> Option<&'static mut [u8]> {
        let i = self.slot(n, true)?;
        self.cache[i].dirty.get_or_insert(kind);
        Some(self.bytes(i))
    }

    fn fresh(&mut self, n: u64, kind: Kind) -> Option<&'static mut [u8]> {
        let i = self.slot(n, false)?;
        self.bytes(i).fill(0);
        self.cache[i].dirty = Some(kind);
        Some(self.bytes(i))
    }

    fn forget(&mut self, n: u64) {
        if let Some(e) = self.cache.iter_mut().find(|e| e.n == n) {
            e.n = u64::MAX;
            e.dirty = None;
        }
    }

    // a block written straight from a request buffer is not on the disk until
    // the commit, so a later partial write in the batch starts from the buffer
    fn pending_copy(&self, n: u64) -> Option<u64> {
        let sector = n * self.block / SECTOR;
        let end = sector + self.block / SECTOR;
        let r = self.batch.runs.iter().find(|r| r.sector <= sector && end <= r.sector + r.len as u64 / SECTOR)?;
        Some(r.phys + (sector - r.sector) * SECTOR)
    }

    fn busy(&self) -> bool {
        self.batch.class.is_some() || !self.batch.replies.is_empty()
    }

    fn full(&self) -> bool {
        let dirty = self.cache.iter().filter(|c| c.dirty.is_some()).count();
        self.batch.replies.len() >= BATCH_OPS || self.batch.bufs.len() >= BATCH_BUFS || dirty >= CACHE_BLOCKS / 2
    }

    fn commit(&mut self) -> bool {
        let mut ok = true;
        let mut runs = core::mem::take(&mut self.batch.runs);
        if let Some(class) = self.batch.class.take() {
            for stage in stages(class) {
                let mut reqs = Vec::new();
                if stage.contains(&Kind::Data) && !runs.is_empty() {
                    // a cached copy of a block in `runs` is newer and must land last
                    let overlap = self.cache.iter().any(|c| {
                        let sector = c.n.wrapping_mul(self.block / SECTOR);
                        c.dirty == Some(Kind::Data) && runs.iter().any(|r| r.sector <= sector && sector < r.sector + r.len as u64 / SECTOR)
                    });
                    if overlap {
                        ok &= virtio_blk::run(&runs);
                    } else {
                        reqs.append(&mut runs);
                    }
                }
                for c in self.cache.iter().filter(|c| c.dirty.is_some_and(|k| stage.contains(&k))) {
                    reqs.push(Req::write(c.n * self.block / SECTOR, c.phys, self.block as u32));
                }
                if !reqs.is_empty() {
                    ok &= virtio_blk::run(&reqs);
                }
                for c in self.cache.iter_mut().filter(|c| c.dirty.is_some_and(|k| stage.contains(&k))) {
                    c.dirty = None;
                }
            }
        }
        for b in core::mem::take(&mut self.batch.bufs) {
            bufs::free(b);
        }
        for (token, status, body) in core::mem::take(&mut self.batch.replies) {
            match status {
                OK if !ok => rpc::reply(token, INVALID, Body::EMPTY),
                _ => rpc::reply(token, status, body),
            }
        }
        ok
    }

    fn counts(&mut self, g: u32, blocks: i32, inodes: i32, dirs: i32) -> Option<()> {
        let (b, at) = self.desc_at(g);
        let d = self.edit(b, Kind::Alloc)?;
        put16(d, at + 12, (le16(d, at + 12) as i32 + blocks) as u16);
        put16(d, at + 14, (le16(d, at + 14) as i32 + inodes) as u16);
        put16(d, at + 16, (le16(d, at + 16) as i32 + dirs) as u16);
        let sb = self.edit(SB_AT / self.block, Kind::Alloc)?;
        let at = (SB_AT % self.block) as usize;
        put32(sb, at + 12, (le32(sb, at + 12) as i64 + blocks as i64) as u32);
        put32(sb, at + 16, (le32(sb, at + 16) as i64 + inodes as i64) as u32);
        Some(())
    }

    fn alloc_block(&mut self, goal: u32) -> Option<u32> {
        let groups = self.groups.len() as u32;
        let goal = goal.clamp(self.first_data_block, self.blocks_count - 1) - self.first_data_block;
        let (g0, start) = (goal / self.blocks_per_group, goal % self.blocks_per_group);
        for k in 0..=groups {
            let g = (g0 + k) % groups;
            let first = g * self.blocks_per_group;
            let bits = self.blocks_per_group.min(self.blocks_count - self.first_data_block - first);
            let map = self.groups[g as usize].block_bitmap as u64;
            let from = if k == 0 { start } else { 0 };
            let Some(bit) = find_zero(self.get(map)?, from, bits) else { continue };
            let m = self.edit(map, Kind::Alloc)?;
            m[bit as usize / 8] |= 1 << (bit % 8);
            self.counts(g, -1, 0, 0)?;
            return Some(self.first_data_block + first + bit);
        }
        None
    }

    fn free_block(&mut self, n: u32) -> Option<()> {
        let rel = n.checked_sub(self.first_data_block)?;
        let (g, bit) = (rel / self.blocks_per_group, rel % self.blocks_per_group);
        self.forget(n as u64);
        let m = self.edit(self.groups.get(g as usize)?.block_bitmap as u64, Kind::Alloc)?;
        m[bit as usize / 8] &= !(1 << (bit % 8));
        self.counts(g, 1, 0, 0)
    }

    fn alloc_inode(&mut self, dir: bool, near: u32) -> Option<u32> {
        let groups = self.groups.len() as u32;
        let g0 = (near - 1) / self.inodes_per_group;
        for k in 0..groups {
            let g = (g0 + k) % groups;
            let map = self.groups[g as usize].inode_bitmap as u64;
            let from = if g == 0 { self.first_ino - 1 } else { 0 };
            let Some(bit) = find_zero(self.get(map)?, from, self.inodes_per_group) else { continue };
            let m = self.edit(map, Kind::Alloc)?;
            m[bit as usize / 8] |= 1 << (bit % 8);
            self.counts(g, 0, -1, dir as i32)?;
            return Some(g * self.inodes_per_group + bit + 1);
        }
        None
    }

    fn free_inode(&mut self, ino: u32, dir: bool) -> Option<()> {
        let (b, at) = self.inode_at(ino)?;
        let now = self.now();
        let rec = self.edit(b, Kind::Inode)?;
        put32(rec, at + 20, now);
        put16(rec, at + 26, 0);
        let (g, bit) = ((ino - 1) / self.inodes_per_group, (ino - 1) % self.inodes_per_group);
        let m = self.edit(self.groups[g as usize].inode_bitmap as u64, Kind::Alloc)?;
        m[bit as usize / 8] &= !(1 << (bit % 8));
        self.counts(g, 0, 1, -(dir as i32))
    }

    fn inode_at(&self, ino: u32) -> Option<(u64, usize)> {
        let idx = ino.checked_sub(1)?;
        let table = self.groups.get((idx / self.inodes_per_group) as usize)?.table;
        let off = (idx % self.inodes_per_group) as u64 * self.inode_size;
        Some((table as u64 + off / self.block, (off % self.block) as usize))
    }

    fn inode(&mut self, ino: u32) -> Option<Inode> {
        let (b, at) = self.inode_at(ino)?;
        let b = self.get(b)?;
        let mut blocks = [0u32; 15];
        for (i, blk) in blocks.iter_mut().enumerate() {
            *blk = le32(b, at + 40 + i * 4);
        }
        let mode = le16(b, at);
        let high = if mode & TYPE_MASK == FILE_MODE { le32(b, at + 108) as u64 } else { 0 };
        Some(Inode { mode, links: le16(b, at + 26), size: le32(b, at + 4) as u64 | high << 32, sectors: le32(b, at + 28), blocks })
    }

    fn put_inode(&mut self, ino: u32, i: &Inode) -> Option<()> {
        let (b, at) = self.inode_at(ino)?;
        let now = self.now();
        let rec = self.edit(b, Kind::Inode)?;
        put16(rec, at, i.mode);
        put32(rec, at + 4, i.size as u32);
        put32(rec, at + 12, now);
        put32(rec, at + 16, now);
        put16(rec, at + 26, i.links);
        put32(rec, at + 28, i.sectors);
        for (k, blk) in i.blocks.iter().enumerate() {
            put32(rec, at + 40 + k * 4, *blk);
        }
        if i.is_file() {
            put32(rec, at + 108, (i.size >> 32) as u32);
        }
        Some(())
    }

    fn new_inode(&mut self, ino: u32, i: &Inode) -> Option<()> {
        let (b, at) = self.inode_at(ino)?;
        let now = self.now();
        let size = self.inode_size as usize;
        let rec = self.edit(b, Kind::Inode)?;
        rec[at..at + size].fill(0);
        put32(rec, at + 8, now);
        if size > 128 {
            put16(rec, at + 128, self.extra_isize);
        }
        self.put_inode(ino, i)
    }

    fn group_start(&self, ino: u32) -> u32 {
        self.first_data_block + (ino - 1) / self.inodes_per_group * self.blocks_per_group
    }

    fn new_block(&mut self, file: &mut Inode, goal: &mut u32, table: bool) -> Option<u32> {
        let b = self.alloc_block(*goal)?;
        *goal = b + 1;
        file.sectors += (self.block / SECTOR) as u32;
        if table {
            self.fresh(b as u64, Kind::Alloc)?;
        }
        Some(b)
    }

    fn direct(&mut self, file: &mut Inode, i: usize, grow: Option<&mut u32>, table: bool) -> Option<(u32, bool)> {
        match grow {
            Some(goal) if file.blocks[i] == 0 => {
                file.blocks[i] = self.new_block(file, goal, table)?;
                Some((file.blocks[i], true))
            }
            _ => Some((file.blocks[i], false)),
        }
    }

    fn entry(&mut self, file: &mut Inode, t: u32, i: u64, grow: Option<&mut u32>, table: bool) -> Option<(u32, bool)> {
        let v = le32(self.get(t as u64)?, (i * 4) as usize);
        match grow {
            Some(goal) if v == 0 => {
                let b = self.new_block(file, goal, table)?;
                put32(self.edit(t as u64, Kind::Alloc)?, (i * 4) as usize, b);
                Some((b, true))
            }
            _ => Some((v, false)),
        }
    }

    fn bmap(&mut self, file: &mut Inode, n: u64, mut grow: Option<&mut u32>) -> Option<(u32, bool)> {
        let per = self.block / 4;
        if n < 12 {
            return self.direct(file, n as usize, grow, false);
        }
        let (top, mut n) = if n - 12 < per { (12, n - 12) } else { (13, n - 12 - per) };
        if top == 13 && n >= per * per {
            return None;
        }
        let (mut t, _) = self.direct(file, top, grow.as_deref_mut(), true)?;
        if t == 0 {
            return Some((0, false));
        }
        if top == 13 {
            (t, _) = self.entry(file, t, n / per, grow.as_deref_mut(), true)?;
            if t == 0 {
                return Some((0, false));
            }
            n %= per;
        }
        self.entry(file, t, n, grow, false)
    }

    fn map(&mut self, file: &Inode, n: u64) -> Option<u32> {
        let mut f = *file;
        self.bmap(&mut f, n, None).map(|(b, _)| b)
    }

    fn cut_table(&mut self, file: &mut Inode, t: u32, from: u64, depth: u32) -> Option<bool> {
        let per = self.block / 4;
        let span = if depth == 1 { 1 } else { per };
        let spb = (self.block / SECTOR) as u32;
        for i in 0..per {
            if (i + 1) * span <= from {
                continue;
            }
            let v = le32(self.get(t as u64)?, (i * 4) as usize);
            if v == 0 {
                continue;
            }
            let gone = if depth == 1 {
                self.free_block(v)?;
                file.sectors -= spb;
                true
            } else {
                self.cut_table(file, v, from.saturating_sub(i * span), 1)?
            };
            if gone && from > 0 {
                put32(self.edit(t as u64, Kind::Alloc)?, (i * 4) as usize, 0);
            }
        }
        if from > 0 {
            return Some(false);
        }
        self.free_block(t)?;
        file.sectors -= spb;
        Some(true)
    }

    fn cut(&mut self, file: &mut Inode, keep: u64) -> Option<()> {
        let per = self.block / 4;
        let spb = (self.block / SECTOR) as u32;
        for i in keep.min(12) as usize..12 {
            if file.blocks[i] != 0 {
                self.free_block(file.blocks[i])?;
                file.blocks[i] = 0;
                file.sectors -= spb;
            }
        }
        for (top, depth, base) in [(12, 1, 12), (13, 2, 12 + per)] {
            let t = file.blocks[top];
            if t != 0 && self.cut_table(file, t, keep.saturating_sub(base), depth)? {
                file.blocks[top] = 0;
            }
        }
        Some(())
    }

    fn each_entry(&mut self, dir: &Inode, mut f: impl FnMut(&[u8], u32, u8) -> bool) -> Option<()> {
        for n in 0..dir.size.div_ceil(self.block) {
            let blk = self.map(dir, n)?;
            if blk == 0 {
                continue;
            }
            let b = self.get(blk as u64)?;
            let mut at = 0usize;
            while at + 8 <= b.len() {
                let (ino, rec, len, kind) = (le32(b, at), le16(b, at + 4) as usize, b[at + 6] as usize, b[at + 7]);
                if rec < 8 + len || at + rec > b.len() {
                    break;
                }
                if ino != 0 && !f(&b[at + 8..at + 8 + len], ino, kind) {
                    return Some(());
                }
                at += rec;
            }
        }
        Some(())
    }

    fn lookup(&mut self, path: &[u8]) -> Option<u32> {
        let mut ino = ROOT;
        for part in path.split(|&c| c == b'/').filter(|p| !p.is_empty()) {
            let dir = self.inode(ino)?;
            if !dir.is_dir() {
                return None;
            }
            let mut found = None;
            self.each_entry(&dir, |name, child, _| {
                if name == part {
                    found = Some(child);
                }
                found.is_none()
            })?;
            ino = found?;
        }
        Some(ino)
    }

    fn write_entry(b: &mut [u8], at: usize, rec: usize, ino: u32, name: &[u8], kind: u8) {
        put32(b, at, ino);
        put16(b, at + 4, rec as u16);
        b[at + 6] = name.len() as u8;
        b[at + 7] = kind;
        b[at + 8..at + 8 + name.len()].copy_from_slice(name);
    }

    fn add_entry(&mut self, dir_ino: u32, dir: &mut Inode, name: &[u8], ino: u32, kind: u8) -> Option<()> {
        let need = rec_size(name.len());
        let blocks = dir.size.div_ceil(self.block);
        for n in 0..blocks {
            let blk = self.map(dir, n)? as u64;
            if blk == 0 {
                continue;
            }
            let b = self.get(blk)?;
            let mut at = 0usize;
            while at + 8 <= b.len() {
                let (cur, rec, len) = (le32(b, at), le16(b, at + 4) as usize, b[at + 6] as usize);
                let used = if cur == 0 { 0 } else { rec_size(len) };
                if rec < 8 || rec < used || at + rec > b.len() {
                    break;
                }
                if rec - used >= need {
                    let b = self.edit(blk, Kind::Dir)?;
                    if used > 0 {
                        put16(b, at + 4, used as u16);
                    }
                    Self::write_entry(b, at + used, rec - used, ino, name, kind);
                    return Some(());
                }
                at += rec;
            }
        }
        let mut goal = self.group_start(dir_ino);
        let (blk, _) = self.bmap(dir, blocks, Some(&mut goal))?;
        let b = self.fresh(blk as u64, Kind::Dir)?;
        Self::write_entry(b, 0, self.block as usize, ino, name, kind);
        dir.size = (blocks + 1) * self.block;
        self.put_inode(dir_ino, dir)
    }

    fn remove_entry(&mut self, dir: &Inode, name: &[u8]) -> Option<u32> {
        for n in 0..dir.size.div_ceil(self.block) {
            let blk = self.map(dir, n)? as u64;
            if blk == 0 {
                continue;
            }
            let b = self.get(blk)?;
            let (mut at, mut prev) = (0usize, None);
            while at + 8 <= b.len() {
                let (ino, rec, len) = (le32(b, at), le16(b, at + 4) as usize, b[at + 6] as usize);
                if rec < 8 + len || at + rec > b.len() {
                    break;
                }
                if ino != 0 && &b[at + 8..at + 8 + len] == name {
                    let b = self.edit(blk, Kind::Dir)?;
                    match prev {
                        Some(p) => put16(b, p + 4, (le16(b, p + 4) as usize + rec) as u16),
                        None => put32(b, at, 0),
                    }
                    return Some(ino);
                }
                prev = Some(at);
                at += rec;
            }
        }
        None
    }

    fn parent_of(&mut self, path: &[u8]) -> Result<(u32, Inode, Vec<u8>), u8> {
        let (parent, name) = split(path).ok_or(INVALID)?;
        let pino = self.lookup(parent).ok_or(NOT_FOUND)?;
        let p = self.inode(pino).ok_or(NOT_FOUND)?;
        if !p.is_dir() {
            return Err(INVALID);
        }
        Ok((pino, p, name.to_vec()))
    }

    fn create(&mut self, path: &[u8]) -> Result<(u32, Inode), u8> {
        if let Some(ino) = self.lookup(path) {
            return self.inode(ino).map(|i| (ino, i)).ok_or(NOT_FOUND);
        }
        let (pino, mut p, name) = self.parent_of(path)?;
        let ino = self.alloc_inode(false, pino).ok_or(FULL)?;
        let file = Inode { mode: NEW_FILE, links: 1, size: 0, sectors: 0, blocks: [0; 15] };
        self.changed(ino);
        self.new_inode(ino, &file).ok_or(INVALID)?;
        let added = self.add_entry(pino, &mut p, &name, ino, FT_FILE);
        self.done(Class::Grow, added.map(|()| (ino, file)).ok_or(FULL))
    }

    fn mkdir(&mut self, path: &[u8]) -> Result<(), u8> {
        if self.lookup(path).is_some() {
            return Err(EXISTS);
        }
        let (pino, mut p, name) = self.parent_of(path)?;
        let ino = self.alloc_inode(true, pino).ok_or(FULL)?;
        let mut dir = Inode { mode: NEW_DIR, links: 2, size: self.block, sectors: 0, blocks: [0; 15] };
        let mut goal = self.group_start(ino);
        let out = (|| {
            let blk = self.new_block(&mut dir, &mut goal, false)?;
            dir.blocks[0] = blk;
            let b = self.fresh(blk as u64, Kind::Dir)?;
            Self::write_entry(b, 0, 12, ino, b".", FT_DIR);
            Self::write_entry(b, 12, self.block as usize - 12, pino, b"..", FT_DIR);
            self.new_inode(ino, &dir)?;
            p.links += 1;
            self.put_inode(pino, &p)?;
            self.add_entry(pino, &mut p, &name, ino, FT_DIR)
        })();
        self.done(Class::Grow, out.ok_or(FULL))
    }

    fn remove(&mut self, path: &[u8]) -> Result<(), u8> {
        let ino = self.lookup(path).ok_or(NOT_FOUND)?;
        let (pino, mut p, name) = self.parent_of(path)?;
        let mut node = self.inode(ino).ok_or(NOT_FOUND)?;
        if ino == ROOT {
            return Err(INVALID);
        }
        self.changed(ino);
        if node.is_dir() {
            let mut empty = true;
            self.each_entry(&node, |n, _, _| {
                empty = n == b"." || n == b"..";
                empty
            });
            if !empty {
                return Err(NOT_EMPTY);
            }
        }
        let out = (|| {
            self.remove_entry(&p, &name)?;
            if node.is_dir() {
                p.links -= 1;
                self.put_inode(pino, &p)?;
                node.links = 0;
            } else {
                node.links = node.links.saturating_sub(1);
            }
            if node.links > 0 {
                return self.put_inode(ino, &node);
            }
            self.cut(&mut node, 0)?;
            node.size = 0;
            self.put_inode(ino, &node)?;
            self.free_inode(ino, node.is_dir())
        })();
        self.done(Class::Shrink, out.ok_or(INVALID))
    }

    fn truncate(&mut self, ino: u32, size: u64) -> Result<u64, u8> {
        let mut file = self.inode(ino).ok_or(NOT_FOUND)?;
        if !file.is_file() {
            return Err(INVALID);
        }
        self.changed(ino);
        let out = (|| {
            if size < file.size {
                self.cut(&mut file, size.div_ceil(self.block))?;
                let tail = (size % self.block) as usize;
                if tail > 0 {
                    let blk = self.map(&file, size / self.block)?;
                    if blk != 0 {
                        self.edit(blk as u64, Kind::Data)?[tail..].fill(0);
                    }
                }
            }
            file.size = size;
            self.put_inode(ino, &file)
        })();
        self.done(Class::Shrink, out.map(|()| size).ok_or(INVALID))
    }

    fn write(&mut self, ino: u32, offset: u64, len: u64, buf: u64) -> Result<(u64, u64), u8> {
        let mut file = self.inode(ino).ok_or(NOT_FOUND)?;
        if !file.is_file() || offset.checked_add(len).is_none() {
            return Err(INVALID);
        }
        self.changed(ino);
        let b = self.block;
        let mut goal = match offset / b {
            0 => self.group_start(ino),
            n => self.map(&file, n - 1).filter(|&x| x != 0).map_or(self.group_start(ino), |x| x + 1),
        };
        let mut runs: Vec<Req> = Vec::new();
        let base = bufs::phys(buf);
        let mut done = 0;
        while done < len {
            let at = offset + done;
            let n = at / b;
            let Some((disk, fresh)) = self.bmap(&mut file, n, Some(&mut goal)) else { break };
            let lo = (at % b) as usize;
            let chunk = (b - lo as u64).min(len - done);
            if chunk == b {
                self.forget(disk as u64);
                let (sector, phys) = (disk as u64 * b / SECTOR, base + done);
                match runs.last_mut() {
                    Some(r) if r.sector + r.len as u64 / SECTOR == sector && r.phys + r.len as u64 == phys => r.len += b as u32,
                    _ => runs.push(Req::write(sector, phys, b as u32)),
                }
            } else {
                let block = if fresh { self.fresh(disk as u64, Kind::Data) } else { self.edit(disk as u64, Kind::Data) };
                let Some(block) = block else { break };
                block[lo..lo + chunk as usize].copy_from_slice(&bufs::bytes(buf)[done as usize..(done + chunk) as usize]);
            }
            done += chunk;
        }
        if done > 0 {
            file.size = file.size.max(offset + done);
        }
        self.batch.runs.append(&mut runs);
        let ok = self.put_inode(ino, &file).is_some();
        let out = match (ok, done) {
            (false, _) => Err(INVALID),
            (true, 0) if len > 0 => Err(FULL),
            _ => Ok((done, file.size)),
        };
        self.done(Class::Grow, out)
    }

    fn changed(&mut self, ino: u32) {
        self.versions.insert(ino, self.next_version);
        self.next_version += 1;
    }

    fn version(&self, ino: u32) -> u64 {
        self.versions.get(&ino).copied().unwrap_or(0)
    }

    fn done<T>(&mut self, class: Class, out: Result<T, u8>) -> Result<T, u8> {
        self.batch.class = Some(class);
        out
    }
}

pub fn start() {
    let Some(fs) = Ext2::mount() else {
        println!("fs: no ext2 file system on the disk");
        return;
    };
    let tid = task::spawn(run, alloc::boxed::Box::into_raw(alloc::boxed::Box::new(fs)) as u64, true)
        .expect("fs: no room for its task");
    SHARED.with(|s| s.task = tid);
    rpc::register_kernel(rpc::SVC_FS, handler);
    let mut body = Body::words(&[rpc::handle(crate::smp::core(), rpc::SVC_FS)]);
    body.set_name(8, 32, b"fs");
    rpc::oneway(rpc::handle(0, rpc::SVC_DIRECTOR), crate::director::OP_REGISTER, body);
}

fn handler(req: &Request) -> Answer {
    SHARED.with(|s| {
        s.queue.push_back((req.token, req.op, req.body, req.buf()));
        if s.waiting {
            s.waiting = false;
            task::wake(s.task);
        }
    });
    Answer::Later
}

fn run(arg: u64) {
    let fs = unsafe { &mut *(arg as *mut Ext2) };
    loop {
        let Some((token, op, body, buf)) = SHARED.with(|s| s.queue.pop_front()) else {
            if fs.busy() {
                fs.commit();
                continue;
            }
            let flags = crate::cpu::push_cli();
            if SHARED.with(|s| {
                s.waiting = s.queue.is_empty();
                s.waiting
            }) {
                task::block_current();
            }
            crate::cpu::pop_flags(flags);
            continue;
        };
        let class = class_of(op);
        let conflict = match class {
            Some(c) => fs.batch.class.is_some_and(|b| b != c),
            // reads go to the disk and must not overtake writes still in the batch
            None => matches!(op, OP_READ | OP_SYNC),
        };
        if fs.busy() && (conflict || fs.full()) {
            fs.commit();
        }
        let answer = match (op, buf) {
            (OP_WRITE, Some(buf)) => {
                let out = fs.write(body.word(0) as u32, body.word(1), body.word(2).min(bufs::SIZE as u64), buf);
                fs.batch.bufs.push(buf);
                Some(reply_of(out.map(|(n, size)| Body::words(&[n, size]))))
            }
            (_, Some(buf)) => {
                bufs::free(buf);
                Some((INVALID, Body::EMPTY))
            }
            _ => serve(fs, token, op, &body),
        };
        match answer {
            Some((status, reply)) if class.is_some() => fs.batch.replies.push((token, status, reply)),
            Some((status, reply)) => rpc::reply(token, status, reply),
            None => {}
        }
    }
}

fn reply_of(out: Result<Body, u8>) -> (u8, Body) {
    match out {
        Ok(body) => (OK, body),
        Err(status) => (status, Body::EMPTY),
    }
}

fn writable() -> Result<(), u8> {
    if virtio_blk::read_only() {
        Err(INVALID)
    } else {
        Ok(())
    }
}

fn serve(fs: &mut Ext2, token: u64, op: u16, b: &Body) -> Option<(u8, Body)> {
    let not_found = Some((NOT_FOUND, Body::EMPTY));
    let path = b.name_at(0, 40);
    match op {
        OP_OPEN => {
            let Some(ino) = fs.lookup(path) else { return not_found };
            let Some(i) = fs.inode(ino) else { return not_found };
            Some((OK, Body::words(&[ino as u64, i.size, i.mode as u64, fs.version(ino)])))
        }
        OP_STAT => match fs.inode(b.word(0) as u32) {
            Some(i) => Some((OK, Body::words(&[i.size, i.mode as u64]))),
            None => not_found,
        },
        OP_READDIR => {
            let Some(dir) = fs.inode(b.word(0) as u32) else { return not_found };
            let want = b.word(1);
            let mut seen = 0;
            let mut hit = None;
            fs.each_entry(&dir, |name, ino, kind| {
                if name == b"." || name == b".." {
                    return true;
                }
                if seen == want {
                    let mut n = [0u8; 24];
                    let len = name.len().min(24);
                    n[..len].copy_from_slice(&name[..len]);
                    hit = Some((ino, kind, n, len));
                    return false;
                }
                seen += 1;
                true
            });
            let Some((ino, kind, name, len)) = hit else { return not_found };
            let size = fs.inode(ino).map_or(0, |i| i.size);
            let mut out = Body::words(&[ino as u64, size << 8 | kind as u64]);
            out.set_name(16, 24, &name[..len]);
            Some((OK, out))
        }
        OP_READ => read(fs, token, b.word(0) as u32, b.word(1), b.word(2)),
        OP_CREATE => Some(reply_of(
            writable().and_then(|()| fs.create(path)).map(|(ino, i)| Body::words(&[ino as u64, i.size, i.mode as u64])),
        )),
        OP_TRUNCATE => Some(reply_of(
            writable().and_then(|()| fs.truncate(b.word(0) as u32, b.word(1))).map(|size| Body::words(&[size])),
        )),
        OP_REMOVE => Some(reply_of(writable().and_then(|()| fs.remove(path)).map(|()| Body::EMPTY))),
        OP_MKDIR => Some(reply_of(writable().and_then(|()| fs.mkdir(path)).map(|()| Body::EMPTY))),
        OP_SYNC => {
            let ok = fs.commit() && virtio_blk::flush();
            Some((if ok { OK } else { INVALID }, Body::EMPTY))
        }
        _ => Some((INVALID, Body::EMPTY)),
    }
}

// the disk reads whole blocks, so the data starts `skew` bytes into the
// buffer
fn read(fs: &mut Ext2, token: u64, ino: u32, offset: u64, len: u64) -> Option<(u8, Body)> {
    let Some(file) = fs.inode(ino) else { return Some((NOT_FOUND, Body::EMPTY)) };
    let skew = offset % fs.block;
    let len = len.min(file.size.saturating_sub(offset)).min(bufs::SIZE as u64 - skew);
    if len == 0 {
        return Some((OK, Body::words(&[0, 0])));
    }
    let Some(buf) = bufs::alloc() else { return Some((BUSY, Body::EMPTY)) };
    let first = offset / fs.block;
    let count = (skew + len).div_ceil(fs.block);
    let mut runs: Vec<(u64, u64, u64)> = Vec::new();
    for i in 0..count {
        let Some(disk) = fs.map(&file, first + i) else {
            bufs::free(buf);
            return Some((INVALID, Body::EMPTY));
        };
        if disk == 0 {
            bufs::bytes(buf)[(i * fs.block) as usize..((i + 1) * fs.block) as usize].fill(0);
            continue;
        }
        match runs.last_mut() {
            Some(r) if r.0 + r.2 == disk as u64 && r.1 + r.2 == i => r.2 += 1,
            _ => runs.push((disk as u64, i, 1)),
        }
    }
    let slot = SHARED.with(|s| {
        let flight = InFlight { token, buf, runs: runs.len(), len, skew, failed: false };
        match s.reads.iter().position(|r| r.is_none()) {
            Some(i) => {
                s.reads[i] = Some(flight);
                i
            }
            None => {
                s.reads.push(Some(flight));
                s.reads.len() - 1
            }
        }
    });
    if runs.is_empty() {
        finish(slot);
        return None;
    }
    let base = bufs::phys(buf);
    for (disk, at, n) in runs {
        let r = Req::read(disk * fs.block / SECTOR, base + at * fs.block, (n * fs.block) as u32);
        if virtio_blk::submit(r, Then::Call(read_done, slot as u64)).is_err() {
            read_done(slot as u64, virtio_blk::run(&[r]));
        }
    }
    None
}

fn read_done(slot: u64, ok: bool) {
    let done = SHARED.with(|s| {
        let r = s.reads[slot as usize].as_mut()?;
        r.runs -= 1;
        r.failed |= !ok;
        Some(r.runs == 0)
    });
    if done == Some(true) {
        finish(slot as usize);
    }
}

fn finish(slot: usize) {
    let Some(r) = SHARED.with(|s| s.reads[slot].take()) else { return };
    if r.failed {
        bufs::free(r.buf);
        rpc::reply(r.token, INVALID, Body::EMPTY);
    } else {
        rpc::reply_buf(r.token, OK, Body::words(&[r.len, r.skew]), r.buf);
    }
}
