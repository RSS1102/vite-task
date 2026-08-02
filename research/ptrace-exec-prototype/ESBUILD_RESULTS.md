# Latest-esbuild compatibility experiment

Validation date: 2026-08-02

The npm registry and upstream GitHub release API both reported esbuild 0.28.1
as latest. The GitHub release was published on 2026-06-11:

- <https://registry.npmjs.org/esbuild/latest>
- <https://github.com/evanw/esbuild/releases/tag/v0.28.1>
- <https://registry.npmjs.org/@esbuild/linux-arm64/-/linux-arm64-0.28.1.tgz>

## Environment

```text
Ubuntu 24.04.4 LTS
Linux 6.8.0-134-generic aarch64
GCC 13.3.0
Lima/VZ, 4 CPUs, 6 GiB RAM
esbuild 0.28.1, statically linked AArch64 ELF
```

## Interception configuration

Before exec, `esbuild_injector.c` installs `SECCOMP_RET_TRAP` rules for:

- `getpid`
- `openat`
- `rt_sigaction`
- `rt_sigprocmask`

At `PTRACE_EVENT_EXEC`, it injects a 360-byte freestanding PIC handler. The
handler:

- reissues `getpid` and `openat` with the sixth argument set to
  `0xf5f05ec0dec0de55`;
- compares both 32-bit halves of that marker in classic BPF before allowing a
  gateway call;
- shadows the target's logical `SIGSYS` action so Go can query and replace it
  without removing the physical fspy handler;
- strips the physical SIGSYS bit from every action mask passed to the kernel;
- strips SIGSYS from every `rt_sigprocmask` input before reissuing it;
- returns raw kernel results through saved AArch64 `x0`.

The tracer detaches before target entry. It does not remain attached for any
seccomp trap.

## Results

Version startup passed:

```text
esbuild-injector: handler=0xf0ea72d07000 blob=360 bytes (RWX experiment)
esbuild-injector: exec-stop to detach 86.125 us
0.28.1
esbuild-injector: target exit=0
```

A real bundle, which requires `openat` to read `input.js`, also passed:

```text
esbuild-injector: handler=0xe6ec97cc4000 blob=360 bytes (RWX experiment)
esbuild-injector: exec-stop to detach 110.125 us

  out.js  1.0kb

⚡ Done in 1ms
esbuild-injector: target exit=0
```

Executing the generated bundle printed `answer=42`.

The version experiment also passed with the injector compiled under
AddressSanitizer and UndefinedBehaviorSanitizer.

Thirty independent `esbuild --version` injections measured time directly from
receipt of `PTRACE_EVENT_EXEC` through successful `PTRACE_DETACH`:

```text
runs=30 min=66.208 us p50=78.708 us p95=101.792 us max=217.375 us mean=84.669 us
```

This excludes launcher startup and esbuild runtime. It includes remote `mmap`,
45 word-sized `PTRACE_POKEDATA` writes, remote `rt_sigaction`, register and entry
instruction restoration, and detach.

Run the recorded experiment with:

```sh
./run_esbuild_experiment.sh
```

## Exact boundary of this proof

This is compatibility evidence, not a production handler:

- The handler page is RWX so its four-word SIGSYS shadow can share the PIC
  mapping. Production should split RX code from RW state or use a file-backed
  sealed mapping.
- `rt_sigaction` and `rt_sigprocmask` inputs are modified in place around the
  gateway syscall. A concurrent reader can observe the temporary sanitized
  value, and an invalid/read-only pointer can fault in the handler.
- SIGSYS action state is shadowed, but logical per-thread SIGSYS mask state is
  not. Callers always observe the physical unblocked state in returned masks.
- A non-seccomp SIGSYS is not forwarded to the target's shadow handler.
- `SA_RESETHAND`, logical `SA_NODEFER`, `SA_ONSTACK`, synchronous signal waits,
  `signalfd`, and target-edited `ucontext` masks are not emulated.
- Only native AArch64 and the four named syscall numbers are implemented.
- The gateway marker is an accidental-bypass guard, not a security boundary.

Despite those limits, latest static Go/esbuild completed runtime signal setup,
created threads, opened its input, and produced a valid bundle after ptrace had
detached.

## Non-leader exec observation

`nonleader_exec.c` separately traced a pthread worker executing `/bin/true`.
The event was reported under the process leader's TID, while
`PTRACE_GETEVENTMSG` returned the worker's former TID:

```sh
cc -O2 -g -Wall -Wextra -Werror -std=gnu11 -pthread \
  -o nonleader-exec nonleader_exec.c
./nonleader-exec /bin/true
```

```text
nonleader: clone event leader=66112 worker=66113
nonleader: exec stop reported as tid=66112; former tid=66113
PASS: PTRACE_GETEVENTMSG preserved the non-leader's former TID
```

This confirms that a production supervisor must re-key per-TID exec state at a
non-leader exec event.
