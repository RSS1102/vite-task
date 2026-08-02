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

## Recursive `PTRACE_SEIZE` and Vitest browser proof

`recursive_injector.c` implements the production-shaped recursive success path
on x86-64:

1. The in-process handler traps `execve` and `execveat`, sends its TID and a
   release-word address to the ancestor supervisor with a queued real-time
   signal, and waits in a futex. It does not depend on an inherited control fd.
2. The supervisor attaches only to that thread with `PTRACE_SEIZE`, releases
   the handler, and follows it to `PTRACE_EVENT_EXEC`.
3. The supervisor injects a freestanding handler into the new image, installs
   the physical `SIGSYS` action, and detaches before target code runs.
4. Steady-state file syscalls are handled in process with no ptrace stop. The
   prototype reissues `openat`, `openat2`, `newfstatat`, `statx`, `getdents64`,
   `faccessat`, and `faccessat2` through the sixth-argument gateway.
5. `rt_sigaction(SIGSYS, ...)` and `rt_sigprocmask` are minimally virtualized so
   shell, Node, and Chromium signal initialization cannot remove or block the
   physical handler.

Run a command with:

```sh
make recursive_injector
./recursive_injector /bin/true
```

The repository CI runs its real Vitest browser fixture through this launcher.
The [native x86-64 result](https://github.com/voidzero-dev/vite-task/actions/runs/30735989574)
passed Node 22.19.0, Vitest 4.1.10, Playwright 1.61.1, and Chrome Headless Shell
149.0.7827.55. The bridge injected nine images, including four Chromium images,
reported zero failed execs, and produced a passing JSON test report. The same
bridge also passed inside Docker's default seccomp and AppArmor profiles with
`no_new_privs=1`.

One non-obvious requirement came directly from this test. Linux automatically
blocks `SIGSYS` while the handler runs. A successful exec from that handler
never reaches `rt_sigreturn`, so the new image otherwise inherits `SIGSYS`
blocked and dies on its first trapped syscall. The handler must explicitly
unblock physical `SIGSYS` immediately before issuing the gateway exec.

The Vitest fixture uses Playwright's default unsandboxed Chromium launch. A
future test must enable `chromiumSandbox: true` to validate interaction with
Chromium's own seccomp policy and logical `SIGSYS` handler.

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
- The recursive prototype uses direct pointer loads while virtualizing signal
  APIs and only reissues file syscalls; it does not yet perform fault-safe path
  capture or write fspy events to shared memory.
- Its logical `SIGSYS` model is intentionally incomplete. Per-thread virtual
  masks, delivery to a target-installed logical handler, and coexistence with
  another seccomp `TRAP` producer remain production work.
