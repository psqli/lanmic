//! Android link configuration. A no-op everywhere else.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }

    // `oboe-sys` links libc++ statically, but `libc++_static.a` leaves the C++
    // ABI runtime to `libc++abi.a`. Without it the vtable slot for every pure
    // virtual in Oboe references an undefined `__cxa_pure_virtual`.
    //
    // Passed as a link arg rather than `rustc-link-lib` so it lands at the end
    // of the link line, after the archive that needs it.
    println!("cargo:rustc-link-arg=-lc++abi");

    // And make that whole class of mistake a build failure instead of a crash
    // on first launch. A shared object may carry undefined symbols, so the link
    // above succeeds either way and `dlopen` is the first thing to complain -
    // on a device, which CI does not have. The NDK's CMake toolchain passes
    // this by default, which is why the C++ build this replaced never shipped
    // a library that could not load.
    println!("cargo:rustc-link-arg=-Wl,--no-undefined");
}
