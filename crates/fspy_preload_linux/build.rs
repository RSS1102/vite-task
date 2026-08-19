//! Enforce that the freestanding payload is built position-independent.
//!
//! The injector maps the payload at an address the kernel chooses, so it must
//! be a PIE (`ET_DYN`). `x86_64-unknown-none` is position-independent by
//! default, but other `-none` targets (e.g. `AArch64`) default to a static
//! relocation model and need `-C relocation-model=pic`. That codegen flag is
//! set in this workspace's `.cargo/config.toml`, which does NOT apply when fspy
//! is built from another workspace. A build script cannot set a codegen flag
//! itself, so verify it is configured and fail with instructions if not — then
//! supply the matching `-pie` linker flag (which a build script *can* set), so
//! a consumer only has to configure the one codegen flag.

use std::env;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");

    // Only the freestanding payload (built for a `-none` target) has the PIE
    // requirement. On any host target this crate is an empty stub.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("none") {
        return;
    }

    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target = env::var("TARGET").unwrap_or_else(|_| arch.clone());
    let rustflags = env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    let pic =
        rustflags.contains("relocation-model=pic") || rustflags.contains("relocation-model=pie");
    let forced_static = rustflags.contains("relocation-model=static");

    // x86_64-unknown-none is PIE by default; every other `-none` target must be
    // told to emit position-independent code.
    let position_independent = !forced_static && (arch == "x86_64" || pic);
    assert!(
        position_independent,
        "\n\nfspy_preload_linux must be built as a position-independent executable so the \
         injector can map it at any address, but `{target}` is not configured for PIC.\n\
         Add this to the consuming workspace's .cargo/config.toml:\n\n    \
         [target.{target}]\n    rustflags = [\"-C\", \"relocation-model=pic\"]\n\n"
    );

    if arch != "x86_64" {
        // rust-lld needs `-pie` to emit an ET_DYN; supply it here so consumers
        // only configure the codegen flag above.
        println!("cargo::rustc-link-arg-bins=-pie");
    }
}
