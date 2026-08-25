# inject_demo — ptrace plus in-process SIGSYS proof of concept

Installs a seccomp filter before `execve`, injects a freestanding Rust payload at
the exec-stop, and lets that payload handle trapped `openat` calls inside the
target process.

## Pieces

| Crate                | Role                                                                                                                    |
| -------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `fspy_nostd`         | Direct Linux syscall and borrowed-descriptor APIs used by the payload.                                                  |
| `fspy_preload_linux` | Freestanding `#![no_std]` payload, SIGSYS handler, and shared resume ABI.                                               |
| `fspy_blob`          | Flattens the static-PIE artifact and applies its base-relative relocations.                                             |
| `inject_demo`        | Creates `test_path`, installs the filter, injects at the `/bin/cat test_path` exec-stop, and detaches from the process. |

## How the injection works

1. Spawn `/bin/cat test_path` via `std::process::Command`. Its `pre_exec` hook
   calls `PTRACE_TRACEME`, enables `NO_NEW_PRIVS`, and installs a classic-BPF
   seccomp filter.
2. The filter returns `SECCOMP_RET_TRAP` for `openat`, except when the unused
   sixth syscall-argument slot contains `OPENAT_COOKIE`.
3. The child stops at the **exec-stop** — the new program is mapped but has not
   run an instruction. The parent saves its registers here.
4. The parent runs `mmap` _inside_ the target by borrowing its program counter
   to execute `syscall; int3` on x86-64 or `svc; brk` on AArch64. It resumes with
   `PTRACE_CONT`, stops at the explicit trap, then restores both the patched
   instructions and registers on every path.
5. It writes the relocated payload at the mapped base, followed by the string
   and an aligned copy of the exec-stop register context.
6. It points the program counter at the payload's entry and, in the argument
   registers, passes the string (address, length) and context address.
7. The parent detaches. The payload installs the SIGSYS handler, restores the
   saved context, and transfers directly to `/bin/cat`'s entry point.
8. For each trapped `openat`, the handler prints the pathname, repeats the raw
   syscall with the cookie, and places its raw return value in the interrupted
   context. The cookie makes the repeated syscall pass the same filter without
   recursion.

The x86-64 trampoline restores every general-purpose register and `rflags`; its
final `ret` reads the target address from the stack. AArch64 has no equivalent
memory-indirect branch, so its final `br x16` leaves only `x16` changed: it holds
the original entry address instead of its saved exec-stop value. AAPCS64 defines
`x16` as the IP0 scratch register, so normal startup code must not rely on its
incoming value.

## The payload targets `*-unknown-none`

The payload is embedded as an artifact dependency built for `x86_64-unknown-none`
or `aarch64-unknown-none-softfloat` (matching the injector's arch). These
bare-metal targets have **no libc, no C runtime, and no dynamic loader**, and are
linked by `rust-lld` directly — so the payload builds the same way everywhere
(no external linker, no crt to collide with, `panic = abort` by default) and is a
self-contained, position-independent (`ET_DYN`) blob mapped anywhere. AArch64's
`-none` target defaults to non-PIC, so it needs `-C relocation-model=pic` (set in
`.cargo/config.toml`); the payload's `build.rs` supplies the matching `-pie` and
fails the build with instructions if that codegen flag is missing — e.g. when the
crate is consumed from another workspace, whose config would not apply. x86-64 is
PIE out of the box.

Because `*-unknown-none` reports `target_os = "none"`, `fspy_nostd` treats it as
freestanding code running on Linux. The payload therefore uses the same
`BorrowedFd` and direct-syscall `io::write` API intended for the eventual
injected runtime, without libc or rustix.

Add the target's `core` once:

```bash
rustup target add "$(uname -m)-unknown-none"              # x86_64
rustup target add aarch64-unknown-none-softfloat          # aarch64
```

### Native (on the Linux box)

```bash
cargo build -p inject_demo
./target/debug/inject_demo
```

### Cross from macOS, then copy to the box

```bash
cargo zigbuild -p inject_demo --target aarch64-unknown-linux-gnu   # or x86_64-…
```

The payload links via `rust-lld` regardless of host, so only `inject_demo` itself
needs the cross linker.

Expected output (stderr/stdout may interleave):

```
payload: <N> bytes, entry +0x…, <k> relocations
mapped <N> bytes into the target at 0x…
fspy_preload_linux: installed SIGSYS handler
detached — payload will restore the exec context in-process
openat: /etc/ld.so.cache
openat: /lib/<architecture>/libc.so.6
openat: test_path
SIGSYS works
/bin/cat exited with code 0
```

## Notes / limits

- x86-64 and aarch64 only.
- The handler currently assumes a valid NUL-terminated path pointer. Emulating
  `EFAULT` without faulting the handler remains deliberately deferred.
- `mmap`s the payload region `RWX`; a kernel that forbids `W^X` mappings
  (hardened SELinux/PaX) would need a two-step `PROT_WRITE` → `mprotect PROT_EXEC`.
- The Linux `inject_demo` test validates the real embedded payload. The
  end-to-end run needs a Linux host.
