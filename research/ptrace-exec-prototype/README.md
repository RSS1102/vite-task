# Post-exec `SIGSYS` handler injection prototype

This proves the kernel ordering needed by a hybrid fspy design:

1. A child installs a seccomp filter that returns `SECCOMP_RET_TRAP` for
   `getpid` and then performs a real `execve`.
2. Its parent catches `PTRACE_EVENT_EXEC`, after the new image exists but before
   its first user-space instruction.
3. The parent advances once to the pending `execve` syscall-exit stop.
4. The parent executes remote `mmap`, `rt_sigaction`, and `mprotect` syscalls at
   the stopped entry PC, copies in a freestanding handler, and detaches.
5. The target verifies `TracerPid: 0` and calls `getpid`. The injected in-process
   handler changes the saved return register to `0x51515151`.

The source has native AArch64 and x86-64 register/trampoline implementations.
It uses only libc/kernel headers and is suitable for a native Linux CI job.
The syscall-exit rendezvous is required because the exec event occurs before
the original syscall finishes returning. In particular, the x86-64 return path
would otherwise overwrite the first injected syscall number in `rax`.

## Run

```sh
make check
```

Expected output includes:

```text
injector: caught PTRACE_EVENT_EXEC before target entry
injector: mapped handler at ..., handler=..., blob=... bytes
injector: detached; target's trapped syscall now has no tracer
target: TracerPid=0 before trapped getpid
target: trapped getpid returned 0x51515151 (expected 0x51515151)
PASS: post-exec handler ran entirely in-process after detach
injector: target exit status 0
```

## Prototype boundaries

- This initial launcher uses `PTRACE_TRACEME`. A production chain can use the
  trapped exec handler to coordinate a short-lived `PTRACE_SEIZE` with the
  supervisor, then detach at the same post-exec stop.
- The filter traps only `getpid`; therefore the three remote injection syscalls
  do not need the planned sixth-argument gateway marker.
- The injected handler only writes a constant return register. It does not yet
  validate `siginfo_t`, reissue a syscall, log an event, virtualize signal state,
  use an alternate stack, or protect its mapping from the target.
- The remote-syscall stub temporarily overwrites one machine word at the new
  entry PC and restores both that word and the complete register set after each
  call.
- The injected page is anonymous RX after installation. SELinux/AppArmor
  policies that deny anonymous executable memory require a file-backed handler
  mapping instead.
- Ptrace remains subject to one-tracer exclusivity, Yama, seccomp, and LSM
  policy. The important performance property is that the tracer is detached
  before any `SECCOMP_RET_TRAP` signal is delivered.
