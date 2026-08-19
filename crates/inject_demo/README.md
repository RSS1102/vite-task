# inject_demo — ptrace code-injection proof of concept

Demonstrates the "get our code registered between `execve` and the target's
first instruction" half of the SIGSYS design, in isolation and with **no seccomp
filter**. The injected code prints a string the supervisor hands it, then hands
control back.

## Pieces

| Crate                   | Role                                                                                                                                                                                                                                                                                                                           |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `fspy_nostd::io::write` | Linux-only raw-`write(2)` wrapper (no libc).                                                                                                                                                                                                                                                                                   |
| `fspy_injection_abi`    | Shared `no_std` register-context layout used by the supervisor and freestanding payload.                                                                                                                                                                                                                                       |
| `fspy_preload_linux`    | Freestanding `#![no_std]` payload, built for `*-unknown-none` (no libc, no C runtime, no loader). Its entry receives a string and resume context, writes the string to stderr, then restores that context and enters the target. It has no baked-in message and reads no environment. Stand-in for "install a SIGSYS handler". |
| `fspy_blob`             | Flattens the trusted static-PIE (`ET_DYN`) artifact into a `Blob` and records its base-relative relocations. It knows nothing about the payload.                                                                                                                                                                               |
| `inject_demo`           | Spawns `/bin/echo hello`, stops it at the exec-stop, injects the payload, string, and resume context, then detaches. The payload resumes the target in-process, so there is no completion signal or second ptrace turn. It drives ptrace through the safe `nix` wrappers.                                                      |

## How the injection works

1. Spawn `/bin/echo` via `std::process::Command`, calling `PTRACE_TRACEME` in a
   `pre_exec` hook so the child traces itself and stops at the exec.
2. The child stops at the **exec-stop** — the new program is mapped but has not
   run an instruction. The parent saves its registers here.
3. The parent runs `mmap` _inside_ the target by borrowing its program counter to
   execute one `syscall`/`svc` instruction. It restores both the instruction and
   registers on every path.
4. It writes the relocated payload at the mapped base, followed by the string
   and an aligned copy of the exec-stop register context.
5. It points the program counter at the payload's entry and, in the argument
   registers, passes the string (address, length) and context address.
6. The parent detaches, which starts the payload. After initialization, the
   payload restores the saved context and transfers directly to `/bin/echo`'s
   entry point. No completion signal or additional ptrace turn is required.

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
fspy_preload_linux: printing data handed to it by the supervisor   # <- payload prints the supervisor's string
detached — payload will restore the exec context in-process
hello                                          # <- /bin/echo itself, after register restoration
/bin/echo exited with code 0
```

## Notes / limits

- x86-64 and aarch64 only.
- `mmap`s the payload region `RWX`; a kernel that forbids `W^X` mappings
  (hardened SELinux/PaX) would need a two-step `PROT_WRITE` → `mprotect PROT_EXEC`.
- The Linux `inject_demo` test validates the real embedded payload. The
  end-to-end run needs a Linux host.
