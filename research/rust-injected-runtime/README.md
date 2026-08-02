# Freestanding Rust handler blob

This research artifact compiles a `#![no_std]` Rust `SIGSYS` handler, raw syscall gateway, `rt_sigreturn` restorer, and fixed-capacity lock-free allocator into relocation-free x86-64 and AArch64 blobs.

Run:

```sh
make check
```

The check rejects runtime relocations, undefined symbols, writable data, dynamic linking, GOT, PLT, TLS, and initialization sections. On native Linux x86-64 it also maps the blob RX, installs the Rust handler and restorer with `rt_sigaction`, triggers a seccomp `SIGSYS`, and exercises the allocator and raw syscall gateway.

Generated files stay under `target/`. The source artifacts are:

- [`src/lib.rs`](src/lib.rs): state ABI, handler probe, allocator, syscall wrapper, and restorer
- [`blob.ld`](blob.ld): extraction layout and forbidden-section assertions
- [`verify.sh`](verify.sh): post-link artifact audit
- [`smoke_x86_64.c`](smoke_x86_64.c): native execution harness

See the [full injected-runtime design](../../docs/fspy-rust-injected-runtime.md) for the production architecture and remaining work.
