# Linux `SECCOMP_RET_TRAP` prototype results

Research date: 2026-08-02

This is an isolated feasibility harness. It does not modify the existing fspy
implementation and is not production-ready.

## Result in one paragraph

The same-process `SECCOMP_RET_TRAP` mechanism is viable on the tested Linux
kernel, including direct assembly syscalls, filesystem argument rewriting,
trusted syscall reissue, nested traps, and concurrent calls from four threads.
A static-musl build passed. The median minimal trap cost was about 0.566 us for
register emulation and 0.714 us for a trusted syscall reissue. A comparable
cross-process seccomp user-notification round trip was about 13.9 us, roughly
20-25 times slower than the in-process paths. Exec and signal semantics carry
most of the implementation risk. In particular, fspy must keep
kernel `SIGSYS` unblocked and non-ignored while virtualizing the target's view,
must decide how to coexist with per-thread alternate stacks, and must replace
kernel exec with a substantially hardened ELF loader. A corrected `AT_RANDOM`
implementation was enough to run and bundle with the current static Go esbuild
0.28.1 under the prototype filter.

## Environment

- Host: macOS 27.0 ARM64, Lima 2.2.0 with the Virtualization.framework driver.
- Guest: Ubuntu 24.04.4 LTS ARM64, 4 vCPUs, 6 GiB memory.
- Kernel: `6.8.0-134-generic #134-Ubuntu SMP PREEMPT_DYNAMIC`.
- Toolchain: GCC 13.3.0, glibc 2.39, musl 1.2.4, Python 3.12.3.
- Seccomp actions: `kill_process kill_thread trap errno user_notif trace log allow`.
- Benchmark affinity: single-thread trap and preload samples used CPU 0;
  user notification used CPU 0 for the tracee and CPU 1 for the supervisor.
- This is a VM microbenchmark. Absolute results should be rerun on native
  x86-64 and ARM64 CI machines; the relative process-boundary cost is clear.

## Artifacts

- `trap_bench.c`: direct syscall, filesystem rewrite, signal virtualization,
  nested trap, multithreading, alternate-stack, and timing probe.
- `reexec_bootstrap.c`: real kernel exec into a static-musl second stage, then
  trusted raw handler reinstallation under the inherited filter.
- `unotify_bench.c`: forked seccomp user-notification emulation and `CONTINUE`
  timing probe.
- `preload_open_bench.c` and `preload_open_interposer.c`: minimal LD_PRELOAD
  dispatch lower bound.
- `trap_preload.c`: retained native handler DSO used to stress a pure userland
  handoff into esbuild. It traps the filesystem syscall family used by current
  fspy plus signal-mask/action changes and reports counts at `exit_group`.
- `libreflect_runner.c`: general argv-preserving runner for libreflect's pure
  `reflect_execve` path.
- `ulexecve_at_random_fix.py`: narrow experimental correction for the reference
  Anvil loader's invalid `AT_RANDOM` pointer.
- `esbuild_input.js` and `esbuild_value.js`: two-file bundle fixture.

## Reproduction commands

Run these inside the Linux guest from this directory (or copy the files to a
guest-local temporary directory first):

```bash
gcc -O2 -Wall -Wextra -Werror -pthread trap_bench.c -o trap_bench
taskset -c 0 ./trap_bench

gcc -O2 -Wall -Wextra -Werror -pthread -static trap_bench.c \
  -o trap_bench_glibc_static
taskset -c 0 ./trap_bench_glibc_static

musl-gcc -O2 -Wall -Wextra -Werror -pthread -static \
  -idirafter /usr/include -idirafter /usr/include/aarch64-linux-gnu \
  trap_bench.c -o trap_bench_musl_static
taskset -c 0 ./trap_bench_musl_static

musl-gcc -O2 -Wall -Wextra -Werror -static \
  -idirafter /usr/include -idirafter /usr/include/aarch64-linux-gnu \
  reexec_bootstrap.c -o reexec_bootstrap_musl
./reexec_bootstrap_musl

gcc -O2 -Wall -Wextra -Werror unotify_bench.c -o unotify_bench
./unotify_bench

gcc -O2 -Wall -Wextra -Werror -fno-builtin \
  preload_open_bench.c -o preload_open_bench
gcc -O2 -Wall -Wextra -Werror -shared -fPIC \
  preload_open_interposer.c -ldl -o libopen_interposer.so
taskset -c 0 ./preload_open_bench
taskset -c 0 env LD_PRELOAD="$PWD/libopen_interposer.so" \
  ./preload_open_bench

gcc -O2 -Wall -Wextra -Werror -shared -fPIC \
  trap_preload.c -o libtrap_preload.so

gcc -O2 -Wall -Wextra -Werror libreflect_runner.c \
  -I/path/to/libreflect/include -L/path/to/libreflect/lib \
  -Wl,-rpath,/path/to/libreflect/lib -lreflect -o libreflect_runner
```

The esbuild handoff used the reference Anvil `ulexecve.py` named by the prior
research, and the current `@esbuild/linux-arm64` package (0.28.1):

```bash
export ULEXECVE_PATH=/path/to/reference/ulexecve.py
export ESBUILD=/path/to/@esbuild/linux-arm64/bin/esbuild

python3 ./ulexecve_at_random_fix.py "$ESBUILD" --version
LD_PRELOAD="$PWD/libtrap_preload.so" \
  python3 ./ulexecve_at_random_fix.py "$ESBUILD" --version
LD_PRELOAD="$PWD/libtrap_preload.so" \
  python3 ./ulexecve_at_random_fix.py "$ESBUILD" \
  esbuild_input.js --bundle --minify
```

## Measured results

Five pinned runs of the minimal trap probe produced these median values:

| Path                                                     | Median ns/op |    Relative to its baseline |
| -------------------------------------------------------- | -----------: | --------------------------: |
| Direct `getpid` syscall, no filter                       |        115.6 |                       1.00x |
| `RET_TRAP`, set return register only                     |        565.8 |                       4.93x |
| `RET_TRAP`, trusted in-process syscall reissue           |        713.7 |                       6.17x |
| `openat("/dev/null") + close`, no filter                 |        531.7 |                       1.00x |
| Trap + safe path copy + trusted `openat` reissue + close |       1451.7 |                       2.72x |
| User notification, supervisor emulates result            |      13922.7 | about 122x syscall baseline |
| User notification with `CONTINUE`                        |      13931.4 | about 123x syscall baseline |

The in-process emulation and reissue paths intentionally disabled the
alternate-stack diagnostic, which performs an extra syscall. The filesystem
path includes a self `process_vm_readv` and the real `openat`, so it is a more
representative lower bound for fspy than the register-only number. It still
does not include path normalization or recording.

The four-thread probe completed 200,000 trapped direct syscalls with zero bad
returns. Its first trap on each new thread ran on the target stack; the handler
installed a private alternate stack and edited `ucontext.uc_stack`, after which
all subsequent traps used it. One sample reported an aggregate wall time of
403 ns per call across four vCPUs. This proves concurrency and the lazy-stack
mechanic, not that fspy should claim the application's alternate stack.

The minimal LD_PRELOAD wrapper only called the next `openat`. Five samples had
median `openat+close` times of 529.4 ns without preload and 527.6 ns with it,
which is indistinguishable from noise. This is a dispatch lower bound, not the
cost of current fspy logging. Its key limitation remains that inline/direct
syscalls bypass it entirely.

## Mechanics validated

### Trusted syscall reissue

The filter checks a random-looking 64-bit magic value in
`seccomp_data.args[5]`. Every syscall currently intercepted by fspy has at most
five real arguments, so the native handler can put the magic in the unused
sixth argument and issue the original syscall without recursively trapping.
This avoids depending on the handler's instruction address, which is useful
when a userland loader preserves a small survivor mapping.

This is an accidental-bypass guard. A malicious target can supply
the magic and bypass tracing. A per-process random value reduces accidental
collision and casual spoofing, but not an adversary that can inspect the
process. An instruction-pointer allowlist is the harder alternative.

### Safe argument access

The handler uses raw self `process_vm_readv`/`process_vm_writev` calls rather
than directly dereferencing target pointers. The invalid-action-pointer test
returned `EFAULT` instead of recursively faulting inside `SIGSYS`. Production
code needs bounded string-copy loops and architecture-specific ABI tests.

### Signal-state virtualization

The host action uses `SA_SIGINFO | SA_NODEFER`. A deliberate direct syscall
from inside the handler nested successfully; without `SA_NODEFER`, automatic
blocking of `SIGSYS` makes a foreign nested trap fatal.

The filter traps target `rt_sigaction(SIGSYS, ...)` calls. The target can
install/query a virtual handler or `SIG_IGN`, while the kernel retains the host
handler. It also traps `rt_sigprocmask`, strips `SIGSYS` from the real mask,
and maintains a virtual target-visible bit. A target that observed `SIGSYS` as
blocked still completed the next trapped syscall. This is required because a
seccomp-generated `SIGSYS` whose real disposition is blocked or ignored is
forced to the default disposition.

The prototype mask state is global for simplicity. Production state must be
per-thread and async-signal-safe. It must also consider masks restored by
`rt_sigreturn`, target-installed seccomp filters, and preexisting uses of
`SIGSYS`.

### Handler bootstrap after the host exec

`reexec_bootstrap.c` installed the filter, issued a trusted real `execve` of
its static-musl image, and entered a fresh second stage. The kernel reset the
caught handler and preserved the filter. The second stage used a raw
magic-bypassed `rt_sigaction` before its first trapped syscall; a direct
`getpid` then trapped and returned the emulated value:

```text
reexec-bootstrap: handler reinstalled before trapped syscall PASS
status=0
```

This validates the central target-exec-to-host-exec cycle on the tested ARM64
musl startup. It does not prove every CRT/toolchain is quiet before `main`;
production should use a small audited custom entry routine rather than depend
on that property.

### Alternate signal stacks

Alternate stacks are per-thread and new threads do not inherit one. The lazy
experiment works, but silently replacing the application's one alternate stack
breaks its `SA_ONSTACK` semantics and leaks mappings when threads churn. The
least intrusive production default is likely to let the native handler run on
the current target stack. Full isolation requires virtualizing `sigaltstack`
and multiplexing host/application state per thread; that is substantially more
work and the first trap on a new thread still arrives on its current stack.

## Userland handoff and esbuild

The earlier libreflect survival probe was rerun on this VM:

```text
pure loader: handler=1 altstack=1 trapped_getpid=424242 (PASS)
real execve: Bad system call, status 159
```

The exec control demonstrates the expected reset of the caught signal
disposition and alternate stack while the seccomp filter survives.

Libreflect also loaded current esbuild 0.28.1 directly. With the full probe DSO
retained across the handoff, both version output and the two-file bundle passed:

```text
0.28.1
fspy-sigsys-probe: fs=5 getpid=0 sigaction=2 sigprocmask=14
fspy-sigsys-probe: fs=72 getpid=0 sigaction=2 sigprocmask=14
(()=>{console.log(12**2);})();
```

These are cleaner target counts than the Anvil experiment below because
libreflect's native runner needs very little filesystem setup. Esbuild did not
call `getpid`, but its Go runtime performed two `SIGSYS` action operations and
14 mask operations, so the compatibility test directly exercised signal
virtualization as well as static Go's filesystem syscalls and worker threads.

An unmodified Anvil loader ran esbuild 0.17.19, 0.19.12, and 0.21.5, but
segfaulted for 0.24.2, 0.25.12, 0.27.3, and current 0.28.1. The dividing line
correlated with Go 1.20 versus Go 1.23+. A GDB hardware watchpoint found the
cause: the loader's `AT_RANDOM` value points to
`stack_base + auxv_word_index`, treating a word index as bytes and landing in
`argv`. Go 1.23+ `runtime.randinit` overwrites the consumed 16-byte seed, which
corrupts `argv[1]` and its null terminator.

The wrapper in this directory allocates 16 dedicated random bytes and patches
the auxiliary vector before handoff. With that correction, current esbuild
0.28.1 (static ARM64, Go 1.26.4) passed both `--version` and a two-file bundle
under the filter:

```text
0.28.1
fspy-sigsys-probe: fs=422 getpid=2 sigaction=6 sigprocmask=23
fspy-sigsys-probe: fs=489 getpid=2 sigaction=6 sigprocmask=23
(()=>{console.log(12**2);})();
```

The counts include Python/loader setup before the handoff, but the successful
bundle necessarily exercised static Go's direct syscalls, worker threads, and
signal initialization after the handoff. Go attempted six `SIGSYS` action
operations and 23 mask operations, demonstrating why signal virtualization is
not optional.

## Feasibility boundary

A purpose-built fspy loader can fix the defects seen in reference loaders:

- validate ELF headers and segment bounds;
- map `PT_LOAD` ranges with correct final W^X permissions and collision checks;
- copy argv, environment, platform, exec filename, and 16 random bytes into an
  owned initial-stack mapping;
- construct correct native auxv entries and interpreter state;
- retain a small position-independent survivor island containing the handler,
  restorer, raw syscall gate, and state;
- fail cleanly when fixed-address target segments collide with that island.

The proposed exec rewrite avoids the largest multithreading gap. On every
logical target exec, the handler can issue a real kernel exec of the static
`fspy_host`, carrying the intended target and argv as host arguments. That
kernel transition kills sibling threads, releases a vfork parent, closes
CLOEXEC descriptors, and performs the normal kernel exec resets. The new host
is single-threaded when it performs the pure userland target handoff. Residual
threads only affect direct in-process-loader experiments
or if `fspy_host` itself starts threads before handoff.

The remaining differences come from target-specific exec behavior.
`/proc/self/exe` and kernel process identity name the
host; target setuid/file-capability and LSM transitions are not applied;
shebang and `binfmt_misc` dispatch, noexec-mount policy, executable-file
accounting, dumpability, and other target-specific decisions need explicit
handling or remain observably different. There is also an atomicity problem:
after kernel exec successfully commits to `fspy_host`, a later parse, mapping,
interpreter, or startup failure cannot return the target's original exec errno
to the old image. Preflight checks reduce but cannot eliminate this
failure-after-host-commit window.

Because the inherited seccomp filter survives the kernel exec while caught
signal actions do not, the static host must install its native handler before
performing any intercepted syscall. Its earliest entry code must use the
trusted bypass for `rt_sigaction`; a dynamic loader or ordinary CRT startup is
too early to trust unless audited syscall by syscall.

The prototype supports continuing this design. The next milestone should be a
hardened, custom-entry static host that validates and loads the target without
starting threads, plus explicit policy for unsupported target-specific exec
semantics and post-commit failures.
