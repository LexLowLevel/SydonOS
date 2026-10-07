fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{dir}/../../rt/user.ld");
    println!("cargo:rerun-if-changed=../../rt/user.ld");
}
