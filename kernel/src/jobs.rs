pub struct Builtin {
    pub name: &'static str,
    pub image: &'static [u8],
    pub needs: &'static str,
}

macro_rules! image {
    ($job:literal) => {
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../target/x86_64-unknown-none/release/job-", $job))
    };
}

pub static BUILTIN: &[Builtin] = &[
    Builtin { name: "hello", image: image!("hello"), needs: "" },
    Builtin { name: "echod", image: image!("echod"), needs: "" },
    Builtin { name: "ping", image: image!("ping"), needs: "echo" },
    Builtin { name: "ticker", image: image!("ticker"), needs: "" },
    Builtin { name: "fault", image: image!("fault"), needs: "" },
    Builtin { name: "flood", image: image!("flood"), needs: "echo" },
];

pub fn builtin(name: &[u8]) -> Option<&'static Builtin> {
    BUILTIN.iter().find(|j| j.name.as_bytes() == name)
}
