#![forbid(unsafe_code)]

// Windows gives a main thread 1 MiB of stack unless the binary asks for more, and the editing UI needs
// more: SolidZ's render server (the same host loop) overflowed 1 MiB on its first frame with only a
// software adapter (0xC00000FD).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    if windows && std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
}
