pub struct Builtin {
    pub name: &'static str,
    pub image: &'static [u8],
    pub needs: &'static str,
    // may hang cores, power off and kill other jobs
    pub privileged: bool,
}

macro_rules! image {
    ($job:literal) => {
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../target/x86_64-unknown-none/release/job-", $job))
    };
}

pub static BUILTIN: &[Builtin] = &[
    Builtin { name: "shell", image: image!("shell"), needs: "console", privileged: true },
    Builtin { name: "hello", image: image!("hello"), needs: "", privileged: false },
    Builtin { name: "echod", image: image!("echod"), needs: "", privileged: false },
    Builtin { name: "ping", image: image!("ping"), needs: "echo", privileged: false },
    Builtin { name: "ticker", image: image!("ticker"), needs: "", privileged: false },
    Builtin { name: "fault", image: image!("fault"), needs: "", privileged: false },
    Builtin { name: "flood", image: image!("flood"), needs: "echo", privileged: false },
    Builtin { name: "kv", image: image!("kv"), needs: "", privileged: false },
    Builtin { name: "kvbench", image: image!("kvbench"), needs: "", privileged: false },
];

pub fn builtin(name: &[u8]) -> Option<&'static Builtin> {
    BUILTIN.iter().find(|j| j.name.as_bytes() == name)
}
