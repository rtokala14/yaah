//! Build script: raise the Windows main-thread stack.
//!
//! MSVC reserves only 1 MiB for the main thread. GPUI's render/layout/paint
//! passes recurse once per element, and an unoptimized build gives every
//! frame its own un-inlined stack slot, so a moderately deep view (the
//! settings overlay with several provider/model rows) overflows 1 MiB while
//! handling ordinary text input. The failure mode is
//! `STATUS_STACK_OVERFLOW` (0xc00000fd) with no Rust panic and no
//! backtrace, which reads like a driver crash rather than a stack limit.
//!
//! Zed hits the same wall and links its own binary with `/stack:8388608`
//! (see `crates/zed/build.rs`, "todo(windows): This is to avoid stack
//! overflow"). We reserve more than that because we do it in debug builds
//! too, where frames are fattest. Reserve is address space, not committed
//! memory — the commit charge stays at one page until a thread actually
//! grows into it, so a large reserve is close to free on 64-bit.
//!
//! Not a substitute for bounding recursion: if a *specific* view grows
//! unboundedly deep, fix the view. This only buys headroom for normal
//! nesting depths.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let is_msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") && is_msvc {
        // `-bins` so the flag lands on the app binary and not on every
        // test harness link, which does not need it.
        println!("cargo:rustc-link-arg-bins=/stack:{}", 32 * 1024 * 1024);
    }
}
