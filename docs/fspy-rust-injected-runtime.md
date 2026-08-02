# Rust injected runtime for fspy

Research date: 2026-08-02

Status: architecture, cross-compiled artifact audit, and native x86-64 execution proof complete. The proof is not yet a production syscall dispatcher.

Primary audience: fspy maintainers implementing the Linux `SIGSYS` handler island and its ptrace injection protocol.

## Decision

Write the injected runtime in freestanding Rust, with small assembly fragments for the raw syscall gateway and `rt_sigreturn` restorer.

Use two mappings:

1. An RX mapping containing a relocation-free Rust code blob, read-only constants, the restorer, and one pointer-sized slot patched before injection.
2. A separate RW mapping containing versioned runtime state, fixed event and scratch storage, and an optional fixed-capacity allocator arena.

Do not allocate in the `SIGSYS` fast path. Use stack records, a preallocated event ring, and an atomic scratch-slot pool there. For initialization and other bounded slow paths, use the in-house monotonic `AtomicUsize` bump allocator in the [proof source](../research/rust-injected-runtime/src/lib.rs). It is lock-free, syscall-free, signal-reentrant, and backed by 64 KiB of fixed memory in the RW state mapping.

The cross-build currently produces these complete probe blobs:

| Target  | Raw blob size | Contents                                                                   |
| ------- | ------------: | -------------------------------------------------------------------------- |
| x86-64  |     240 bytes | handler, restorer, raw six-argument syscall, allocator, state-pointer slot |
| AArch64 |     304 bytes | handler, restorer, raw six-argument syscall, allocator, state-pointer slot |

Both outputs have no runtime relocations, undefined symbols, writable sections, GOT, PLT, TLS, or dynamic-loader dependency. The [linker script](../research/rust-injected-runtime/blob.ld) and [artifact verifier](../research/rust-injected-runtime/verify.sh) make those properties build failures.

The native [CI execution proof](https://github.com/voidzero-dev/vite-task/actions/runs/30737941238) mapped the x86-64 blob RX, installed its handler and restorer through raw `rt_sigaction`, triggered a seccomp `SIGSYS`, returned through the Rust restorer, updated the RW state, and exercised the raw syscall gateway and allocator.

## How Rust code is injected

The supervisor does not inject a Rust ELF executable or start a Rust runtime. Rust is only the source language for a raw machine-code artifact.

At `PTRACE_EVENT_EXEC`, before the target executes its entry instruction, the supervisor performs this sequence:

1. Remote-`mmap` a private RW state region and initialize its versioned ABI header, rings, scratch slots, allocator cursor, and fixed arena.
2. Read the architecture-specific raw blob produced by `llvm-objcopy`.
3. Patch the `FSPY_STATE_PTR` slot in that local byte buffer with the remote state address.
4. Remote-`mmap` a code region as RW and copy the blob with `PTRACE_POKEDATA` or `process_vm_writev`.
5. Remote-`mprotect` the code region to RX. Never leave the production handler RWX.
6. Install its physical `SIGSYS` action with the relocated handler and restorer addresses.
7. Restore target registers and overwritten instruction bytes, then detach.

All code and constant references inside the artifact are PC-relative. The only process-specific value is the state pointer slot. The slot is part of the raw blob and is patched before the remote mapping becomes executable.

An initialized Rust `static` is the wrong representation for this slot. LLVM can constant-fold a private static, while an externally visible PIC static can introduce a GOT lookup. The linker script instead defines the bytes directly:

```ld
. = ALIGN(8);
HIDDEN(FSPY_STATE_PTR = .);
QUAD(0);
```

Rust finds the slot using `lea [rip + FSPY_STATE_PTR]` on x86-64 or `adr FSPY_STATE_PTR` on AArch64. The build asserts that the AArch64 blob remains within the `adr` range. If the artifact grows beyond that bound, change it to `adrp` plus `add` and retain the relocation audit.

## Freestanding Rust contract

The crate uses `#![no_std]`, `panic=abort`, and no dependencies. It links with `rust-lld -nostdlib` rather than musl, despite using a `*-unknown-linux-musl` target for the Linux ABI. The runtime must not depend on:

- libc, `errno`, pthreads, TLS, dynamic linking, or target-owned callbacks;
- formatting, unwinding, or a panic path that returns;
- compiler-generated stack probes or large stack allocations;
- target CPU features newer than the deployment baseline;
- hidden `memcpy` or `memset` imports.

The few operations that require a stable raw ABI stay explicit:

- The syscall gateway is inline assembly. On x86-64 it marks `rcx` and `r11` clobbered and uses `r10`, `r8`, and `r9` for arguments four through six. It deliberately omits the `nomem` option because the kernel can access pointed-to memory.
- The `rt_sigreturn` restorer is a standalone assembly symbol. Its syscall number is 15 on x86-64 and 139 on AArch64.
- The `SIGSYS` handler accepts the kernel's three `SA_SIGINFO` arguments and mutates the architecture return register in the supplied `ucontext`.

Hard-coded `siginfo` and `ucontext` offsets are part of the Linux UAPI contract, not the Rust or libc ABI. Keep native C `offsetof` assertions for glibc and musl in CI, then compare them with Rust constants. The proof currently uses:

| Field                            | x86-64 offset | AArch64 offset |
| -------------------------------- | ------------: | -------------: |
| `siginfo.si_code`                |             8 |              8 |
| `siginfo.si_syscall`             |            24 |             24 |
| `ucontext` syscall return result |           144 |            184 |

These values follow the Linux [x86-64 signal context](https://github.com/torvalds/linux/blob/master/arch/x86/include/uapi/asm/sigcontext.h), [AArch64 signal context](https://github.com/torvalds/linux/blob/master/arch/arm64/include/uapi/asm/sigcontext.h), and [AArch64 ucontext](https://github.com/torvalds/linux/blob/master/arch/arm64/include/uapi/asm/ucontext.h). Do not infer additional offsets from a Rust `libc` struct without the native checks.

## Fixed-capacity global allocator

No existing crate is a better fit than the small in-house allocator.

| Candidate                                                                                   | Assessment                                                                                                                                                                                     |
| ------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Rust's [`GlobalAlloc` example](https://doc.rust-lang.org/core/alloc/trait.GlobalAlloc.html) | Uses exactly the required fixed arena plus atomic monotonic cursor. This is the basis of the proof.                                                                                            |
| [`lock_free_buddy_allocator`](https://github.com/pskrgag/lock_free_buddy_allocator)         | Lock-free, but page-granular, requires a backend allocator for internal metadata, requires a CPU-ID provider, and implements the nightly `Allocator` API rather than a ready global allocator. |
| [`slaballoc`](https://github.com/DrChat/slaballoc)                                          | Lockless and fixed-memory, but allocates only one `Sized` type, is not `GlobalAlloc`, uses nightly features, and retains an alignment FIXME.                                                   |
| [`atomic-pool`](https://github.com/embassy-rs/atomic-pool)                                  | Good model for typed scratch slots, but intentionally exposes a pool-specific `Box`, not arbitrary `Layout` allocation through `GlobalAlloc`.                                                  |

The implemented allocator reserves an aligned interval with a CAS loop:

```rust
let mut current = state.arena_next.load(Relaxed);
loop {
    let aligned = current.checked_add(align - 1)? & !(align - 1);
    let end = aligned.checked_add(size)?;
    if end > ARENA_LEN {
        return null_mut();
    }
    match state.arena_next.compare_exchange_weak(current, end, Relaxed, Relaxed) {
        Ok(_) => return state.arena_base().add(aligned),
        Err(observed) => current = observed,
    }
}
```

The actual implementation returns null rather than using `?`, validates `Layout`, rejects alignments above 4096, and uses checked arithmetic. `dealloc` is a no-op.

Rust guarantees that available atomics in `core::sync::atomic` are [lock-free but not necessarily wait-free](https://doc.rust-lang.org/core/sync/atomic/index.html#portability). For the supported x86-64 and AArch64 targets, the audited output is a native compare-and-swap loop. AArch64 builds with `-C target-feature=-outline-atomics`; otherwise LLVM can emit an external outline-atomic helper, breaking the self-contained blob contract.

`Relaxed` ordering is sufficient to reserve disjoint byte ranges. It does not publish initialized objects to another thread. Any cross-thread object handoff needs its own release and acquire operation.

This allocator has deliberate limitations:

- It is lock-free, not wait-free. A contending caller can retry.
- It never reclaims memory. Logical exec replaces the whole state mapping.
- Exhaustion is deterministic. Infallible `Box::new` or `Vec::push` can still abort on a null result, so runtime code must use fallible allocation APIs.
- Being signal-reentrant does not make allocation desirable in a handler. A nested signal cannot deadlock the allocator, but it can consume the remaining arena or starve an interrupted CAS loop.

## Keep the SIGSYS path allocation-free

Use three bounded structures instead:

1. Build the immediate syscall description in fixed stack storage.
2. Reserve an event-ring record with a monotonically increasing atomic sequence. Publish the completed record with a separate release store so the supervisor never reads a partial record.
3. For bounded path-copy or nested-handler scratch space, claim a fixed slot with an atomic bitmap. Release the bitmap bit only after the slot is no longer referenced.

Nested `SIGSYS` delivery under `SA_NODEFER` must claim a different scratch slot. When the pool is exhausted, block or use a documented supervisor slow path. Do not reuse an in-flight slot and do not drop a cache-relevant event.

Represent virtualized target `SIGSYS` actions as immutable snapshots. Publish a new snapshot with an atomic pointer swap and do not reclaim old snapshots until logical exec. That avoids use-after-free if a nested handler still observes the previous action.

## Build and audit

Run the proof from its directory:

```sh
make check
```

The build performs these steps for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`:

1. Compile one `staticlib` with PIC, no red zone, no unwind tables, and aborting panics.
2. Link only selected sections with `rust-lld -nostdlib --gc-sections --no-undefined`.
3. Reject `.data`, `.bss`, GOT, PLT, TLS, dynamic, and initialization sections in the linker script.
4. Extract `.fspy_blob` with `llvm-objcopy`.
5. Assert that the final ELF has no relocations or undefined symbols and report the state-pointer patch offset.
6. On native Linux x86-64, map the blob RX, install its handler and restorer, trigger a seccomp trap, and exercise its allocator and raw syscall gateway.

Production CI should additionally disassemble and allowlist every syscall instruction, enforce a maximum blob and stack-frame size, run the C/Rust layout assertions, inject at randomized addresses, stress nested and concurrent signals, and execute on native AArch64. Cross-compilation verifies the AArch64 artifact shape, but only native execution can validate instruction-cache coherency after remote writes.

## Remaining work

The Rust language and artifact format are no longer feasibility risks. The production risks are in runtime semantics:

- implement the complete syscall dispatcher and fault-safe target-memory copying;
- build the lossless shared event ring and bounded scratch pool;
- complete per-thread signal-mask and logical `SIGSYS` action virtualization;
- protect the code and state mappings from target `mmap`, `mprotect`, `mremap`, and `munmap` operations;
- test post-injection AArch64 instruction-cache synchronization;
- integrate the same runtime with both the ptrace bridge and the static-host userland loader.

The ptrace bridge remains unsuitable for Chromium's namespace-sandbox zygote exec. Rust changes the handler implementation, not that exec-handshake boundary. Route that exec through the static-host loader before attempting ptrace injection.
