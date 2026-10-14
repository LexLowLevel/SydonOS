#[derive(Clone, Copy, PartialEq)]
pub enum Class {
    Other,
    // big transfers: next to the driver, so copying runs beside it
    Io,
    // small requests: on the driver's core, where they never cross a ring
    IoLatency,
    Compute,
}

#[derive(Clone, Copy)]
pub struct Traits {
    pub class: Class,
    needs: [u8; 32],
    pub privileged: bool,
    pub restart: bool,
}

impl Traits {
    pub const PLAIN: Traits = Traits { class: Class::Other, needs: [0; 32], privileged: false, restart: false };

    pub fn needs(&self) -> Option<&[u8]> {
        let n = self.needs.iter().position(|&b| b == 0).unwrap_or(32);
        (n > 0).then(|| &self.needs[..n])
    }
}

pub struct Builtin {
    pub name: &'static str,
    pub image: &'static [u8],
    pub traits: Traits,
}

const fn needs(name: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let b = name.as_bytes();
    let mut i = 0;
    while i < b.len() && i < 32 {
        out[i] = b[i];
        i += 1;
    }
    out
}

pub static BUILTIN: &[Builtin] = &[Builtin {
    name: "shell",
    image: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../target/x86_64-unknown-linux-gnu/release/job-shell")),
    traits: Traits { class: Class::Other, needs: needs("console"), privileged: true, restart: true },
}];

pub fn builtin(name: &[u8]) -> Option<&'static Builtin> {
    BUILTIN.iter().find(|j| j.name.as_bytes() == name)
}

pub fn path(name: &[u8]) -> alloc::vec::Vec<u8> {
    if name.contains(&b'/') {
        name.to_vec()
    } else {
        [b"/bin/", name].concat()
    }
}

pub fn basename(path: &[u8]) -> &[u8] {
    path.rsplit(|&c| c == b'/').next().unwrap_or(path)
}

pub const MANIFEST_MAGIC: &[u8; 8] = b"SYDMAN01";
pub const MANIFEST_SCAN: usize = 8192;

pub fn manifest(head: &[u8]) -> Traits {
    let Some(at) = head.windows(8).position(|w| w == MANIFEST_MAGIC) else {
        return Traits::PLAIN;
    };
    let Some(m) = head.get(at..at + 64) else {
        return Traits::PLAIN;
    };
    let class = match m[8] {
        1 => Class::Io,
        2 => Class::IoLatency,
        3 => Class::Compute,
        _ => Class::Other,
    };
    let mut needs = [0u8; 32];
    needs.copy_from_slice(&m[16..48]);
    Traits { class, needs, privileged: false, restart: m[48] & 1 != 0 }
}
