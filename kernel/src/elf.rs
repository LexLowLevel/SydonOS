const PT_LOAD: u32 = 1;

pub struct Segment {
    pub offset: u64,
    pub vaddr: u64,
    pub filesz: u64,
    pub memsz: u64,
}

fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn rd64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

// the other functions trust the image once this passed
pub fn is_elf(image: &[u8]) -> bool {
    if image.len() < 0x40 || &image[0..4] != b"\x7fELF" || image[4] != 2 || image[5] != 1 {
        return false;
    }
    let phoff = rd64(image, 0x20) as usize;
    let phentsize = rd16(image, 0x36) as usize;
    let phnum = rd16(image, 0x38) as usize;
    let table = phentsize.checked_mul(phnum).and_then(|n| n.checked_add(phoff));
    if phentsize < 0x38 || table.is_none_or(|end| end > image.len()) {
        return false;
    }
    segments(image).all(|s| {
        s.filesz <= s.memsz && s.offset.checked_add(s.filesz).is_some_and(|end| end <= image.len() as u64)
    })
}

pub fn entry(image: &[u8]) -> u64 {
    rd64(image, 0x18)
}

pub fn segments(image: &[u8]) -> impl Iterator<Item = Segment> + '_ {
    let phoff = rd64(image, 0x20) as usize;
    let phentsize = rd16(image, 0x36) as usize;
    let phnum = rd16(image, 0x38) as usize;
    (0..phnum)
        .map(move |i| phoff + i * phentsize)
        .filter(move |&p| rd32(image, p) == PT_LOAD)
        .map(move |p| Segment {
            offset: rd64(image, p + 0x08),
            vaddr: rd64(image, p + 0x10),
            filesz: rd64(image, p + 0x20),
            memsz: rd64(image, p + 0x28),
        })
}
