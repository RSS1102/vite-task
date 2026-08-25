# Descendant ptrace under gVisor/runsc

Date: 2026-08-24

## Conclusion

Current gVisor permits and implements the descendant-ptrace operations needed
by the injector. This was tested directly under `runsc` as container root and
as UID 65534 with no capabilities. Both identities passed:

- `PTRACE_TRACEME` and the post-exec stop;
- `PTRACE_ATTACH`;
- `PTRACE_SEIZE`, `PTRACE_INTERRUPT`, and `PTRACE_DETACH`;
- `PTRACE_O_TRACEEXEC`, the actual `PTRACE_EVENT_EXEC` stop, and
  `PTRACE_GETEVENTMSG`;
- `PTRACE_GETREGSET` and `PTRACE_SETREGSET` for `NT_PRSTATUS`;
- `PTRACE_PEEKTEXT` and `PTRACE_POKETEXT`;
- `process_vm_readv` and `process_vm_writev`;
- a remote `mmap`, target restoration, detach, and normal target execution.

No `SYS_PTRACE`, `seccomp=unconfined`, or privileged container was required.
The Yama and dumpability behavior matched the ordinary Linux results: an
ancestor can trace a same-credential dumpable descendant under scope 1, a
sibling cannot, `PR_SET_PTRACER` permits the nominated sibling, and a
non-dumpable child cannot be attached. `SYS_PTRACE` bypassed those checks.

The injector now uses `svc #0; brk #0` with `PTRACE_CONT`, rather than
`PTRACE_SINGLESTEP`, for its remote syscall. That exact injector mapped and
relocated the payload, restored execution in-process, detached, installed the
SIGSYS handler, and intercepted `openat` under runsc.

The current end-to-end demo still fails on **ARM64 gVisor**, for a separate
reason: runsc does not restore the original first syscall argument before it
constructs the `SECCOMP_RET_TRAP` signal frame. The handler therefore sees the
syscall number in `x0` instead of argument zero. For the demo's relative
`openat`, runsc shows `dirfd=56` (`SYS_openat`) and returns `EBADF`. Native
Linux restores `x0` and the same binary exits successfully.

The conclusion is therefore split:

- descendant ptrace permission and the injection primitives are compatible;
- the current in-process SIGSYS syscall emulation is not compatible with the
  tested ARM64 runsc release until gVisor matches Linux's syscall rollback, or
  fspy selects another interception backend there.

This signal-frame drift is AArch64-specific in the evidence here. On x86-64,
argument zero and the syscall return value use different registers. A fresh
GitHub-hosted run of the same runsc release passed both the strict probe and
the current injector as root and UID 65534. It used the fully audited probe
source `da027737…34277a` and asserted both injector exit zero and the two success
markers. See the
[hardened rerun](ptrace-strict-rerun-results.md) and
[Actions run 32654193527](https://github.com/voidzero-dev/vite-task/actions/runs/32654193527).

## Tested environment

| Component            | Value                                 |
| -------------------- | ------------------------------------- |
| Host                 | macOS ARM64                           |
| VM                   | Colima using Virtualization.framework |
| Guest                | Ubuntu 24.04.4 ARM64                  |
| Guest kernel         | `6.8.0-100-generic`                   |
| Docker client/server | 29.7.1 / 29.2.1                       |
| gVisor binary        | `runsc` 20260817.0                    |
| runsc version        | `release-20260817.0`, OCI spec 1.2.1  |
| runsc platform       | `systrap` (default)                   |
| gVisor Yama scope    | 1                                     |

gVisor officially supports ARM64 and Linux 5.6 or newer. The first pass used
the project's official APT repository, following the
[gVisor installation guide](https://gvisor.dev/docs/user_guide/install/). The
strict rerun downloaded the signed release binary directly and verified its
SHA-512 checksum. `docker run --runtime=runsc hello-world` passed, and `uname`
inside runsc reported Linux `4.19.0-gvisor`.

## Existing environment probe

The hardened
[`ptrace-environment-probe.c`](ptrace-environment-probe.c) was run without any
runtime override beyond `--runtime=runsc`:

```bash
docker run --rm --runtime=runsc --init fspy-ptrace-probe
docker run --rm --runtime=runsc --init --user 65534:65534 fspy-ptrace-probe
```

Root and UID 65534 produced the same ptrace results. The latter reported a zero
effective capability set.

| Probe                                            | Result  |
| ------------------------------------------------ | ------- |
| `TRACEME` + exec + `GETREGSET`/`SETREGSET`       | Pass    |
| `ATTACH` direct child                            | Pass    |
| `SEIZE` direct child                             | Pass    |
| `process_vm_writev` direct child                 | Pass    |
| `process_vm_readv` direct child                  | Pass    |
| `PTRACE_PEEKDATA`/`PTRACE_POKEDATA` direct child | Pass    |
| `SEIZE` live grandchild                          | Pass    |
| `SEIZE` same-UID sibling                         | `EPERM` |
| sibling after `PR_SET_PTRACER`                   | Pass    |
| `SEIZE` child with dumpability zero              | `EPERM` |
| orphan reparented away from supervisor           | `EPERM` |
| orphan reparented to subreaper supervisor        | Pass    |

The successful `SEIZE` case also performed `PTRACE_INTERRUPT` and detached; a
failure in either operation would have aborted the probe. Both the ordinary
runsc runtime (`Seccomp: 0`) and a runtime configured with
`--oci-seccomp=true` (`Seccomp: 2`) printed `required-summary result=PASS` and
exited zero as root and UID 65534.

Repeating the root probe with `--cap-add SYS_PTRACE` made the sibling,
non-dumpable child, and reparented-away orphan pass. This confirms that gVisor
applies its emulated capability and ptrace access checks in the expected way.

## Exec-boundary and injection-primitives probe

The focused
[`ptrace-exec-injection-primitives-probe.c`](ptrace-exec-injection-primitives-probe.c)
fills the gaps in the general environment probe. It:

1. seizes a paused child with `PTRACE_O_TRACEEXEC | PTRACE_O_EXITKILL`;
2. lets the child exec;
3. verifies `SIGTRAP | PTRACE_EVENT_EXEC` and reads the event message;
4. gets and sets the complete general-register set;
5. reads and replaces instructions at the new image's entry point;
6. executes remote `mmap` followed by an explicit trap;
7. writes and reads the new mapping with `process_vm_*`;
8. restores code and registers, detaches, and verifies normal target exit.

Build and run it with:

```bash
docker build -t fspy-ptrace-exec-probe -f- research <<'DOCKERFILE'
FROM fspy-ptrace-probe
COPY ptrace-exec-injection-primitives-probe.c /tmp/ptrace-exec-probe.c
RUN gcc -O2 -Wall -Wextra -Werror \
    -o /ptrace-exec-probe /tmp/ptrace-exec-probe.c
ENTRYPOINT ["/ptrace-exec-probe"]
DOCKERFILE

docker run --rm --runtime=runsc fspy-ptrace-exec-probe
docker run --rm --runtime=runsc --user 65534:65534 fspy-ptrace-exec-probe
```

Representative runsc output:

```text
pid=1 uid=65534 euid=65534
traceexec-event               result=PASS former_tid=2
get-set-regset                result=PASS bytes=272
remote-mmap-syscall-trap      result=PASS address=0xf0cedc0d3000
process-vm-injected-mapping   result=PASS
detach-resume-target          result=PASS
single-step                   result=UNSUPPORTED status=0
```

The runc control produced the same passing injection results and reported
`single-step result=PASS status=0x57f`.

The explicit trap is not an additional supervisor round trip relative to the
current demo: both `PTRACE_SINGLESTEP` and `PTRACE_CONT` resume once and require
one wait for the tracee to stop. The difference is that the stop is encoded in
the injected instruction stream rather than delegated to architecture-specific
single-step support.

## Current explicit-trap injector

The strict rerun built the current AArch64 `inject_demo`, including its
`PTRACE_CONT` plus `svc; brk` remote syscall, and ran it through Docker using
both ordinary runsc and runsc with OCI seccomp enabled. It was tested as root
and as UID 65534. No run added `SYS_PTRACE`, disabled seccomp, or used a
privileged container.

The ptrace and injection portion completed in every run:

```text
payload: 163840 bytes, entry +0x13bfc, 50 relocations
mapped 164160 bytes into the target at 0xf8bafeb93000
detached — payload will restore the exec context in-process
fspy_preload_linux: installed SIGSYS handler
openat: /etc/ld.so.cache
openat: /lib/aarch64-linux-gnu/libc.so.6
openat: test_path
/bin/cat: test_path: Bad file descriptor
Error: /bin/cat exited with code 1
```

The absolute-path opens succeed because Linux ignores `dirfd` for an absolute
pathname. The relative `test_path` exposes the corrupted first argument.

The x86-64 control used the same source snapshot and runsc
`release-20260817.0`. Both identities exited zero and printed `openat:
test_path`, `SIGSYS works`, and `/bin/cat exited with code 0`. This establishes
an architecture-specific compatibility split, not a universal gVisor failure.

## `SECCOMP_RET_TRAP` argument-zero reproduction

[`gvisor-seccomp-trap-arg0-probe.c`](gvisor-seccomp-trap-arg0-probe.c) removes
ptrace and filesystem state from the failure. It traps `close(0x1234)`, reads
argument zero from the `ucontext`, and returns `EBADF` from the handler.

The same ARM64 image under runc reports:

```text
expected-arg0=0x1234 observed-arg0=0x1234 result=-1 errno=9
seccomp-trap-arg0 result=PASS
```

Under runsc, as root and UID 65534, it reports:

```text
expected-arg0=0x1234 observed-arg0=0x39 result=-1 errno=9
seccomp-trap-arg0 result=FAIL
```

`0x39` is AArch64 `SYS_close`. The implementation explains the observation:

1. Linux handles `SECCOMP_RET_TRAP` by calling `syscall_rollback()` before
   sending SIGSYS. On AArch64, rollback copies `orig_x0` back to `x0`.
2. gVisor saves the original value in `OrigR0`, but its seccomp trap path sends
   SIGSYS and then calls `SetReturn(sysno)`. Signal setup copies that clobbered
   register into the user signal frame.

See Linux's [`kernel/seccomp.c`](https://github.com/torvalds/linux/blob/master/kernel/seccomp.c),
Linux's [AArch64 syscall helpers](https://github.com/torvalds/linux/blob/master/arch/arm64/include/asm/syscall.h),
and gVisor's [`seccomp.go`](https://github.com/google/gvisor/blob/release-20260817.0/pkg/sentry/kernel/seccomp.go),
[`syscalls_arm64.go`](https://github.com/google/gvisor/blob/release-20260817.0/pkg/sentry/arch/syscalls_arm64.go),
and [`signal_arm64.go`](https://github.com/google/gvisor/blob/release-20260817.0/pkg/sentry/arch/signal_arm64.go).

The robust resolution is an upstream gVisor rollback fix. A production fspy
probe should detect the incompatible behavior and select a non-TRAP backend.
Seccomp user notification is one possible cross-process fallback where the
runtime supports it. `SECCOMP_RET_TRACE` is not equivalent to the detached
design: without an attached tracer configured for seccomp events it makes the
syscall fail with `ENOSYS`; with one, every intercepted syscall incurs a ptrace
stop. It is therefore a slower, permanently attached fallback, not a repair for
the in-process handler. Per-syscall reconstruction is not a general solution:
`SECCOMP_RET_DATA` exposes only 16 bits, while argument zero can be a full-width
pointer, descriptor, offset, or flag word.

## Why ARM64 single-step fails

In the tested release,
[`pkg/sentry/arch/arch_aarch64.go`](https://github.com/google/gvisor/blob/release-20260817.0/pkg/sentry/arch/arch_aarch64.go)
implements `SingleStep()` as always false and leaves both `SetSingleStep()` and
`ClearSingleStep()` empty with a reference to gVisor issue 1239. The generic
ptrace layer accepts `PTRACE_SINGLESTEP`, so the request itself returns success;
the missing architecture implementation means execution is not stopped after
one instruction.

This result is architecture-specific. It directly proves the behavior of the
tested ARM64 release; it does not establish that gVisor on x86-64 lacks
single-step.

## OCI seccomp behavior

The default runsc runtime advertises `--oci-seccomp=false`. Consequently,
`/proc/self/status` reported `Seccomp: 0`, and even an explicit Docker profile
that denied ptrace and `process_vm_*` was not applied inside the sandbox. This
is runsc's default configuration, not a guarantee that gVisor deployments
cannot use OCI seccomp.

To test that distinction, a temporary second runtime was installed:

```bash
sudo runsc install --runtime runsc-oci-seccomp -- --oci-seccomp=true
sudo systemctl reload docker
```

With this runtime, Docker's default profile reported `Seccomp: 2` and every
required operation still passed, including ptrace word I/O. With
[`ptrace-deny-seccomp.json`](ptrace-deny-seccomp.json), all ptrace and
`process_vm_*` cases failed with `EPERM`, including `TRACEME` and a direct-child
`SEIZE`. Therefore the same deployment rule applies to gVisor when OCI seccomp
is enabled: the maintained Docker default is compatible, while an explicit
deny cannot be bypassed with ancestry or `PR_SET_PTRACER`.

## VM changes and cleanup

The experiment made only disposable-VM and research-file changes. It installed
the official `runsc` package and its APT source/key, added `runsc` and the
temporary `runsc-oci-seccomp` entries to `/etc/docker/daemon.json`, and reloaded
Docker. Installing prerequisites also installed `apt-transport-https` and
upgraded `ca-certificates`, `curl`, `libcurl4t64`, and
`libcurl3t64-gnutls` to the then-current Ubuntu updates. APT package indexes
were refreshed.

The runtime entries, runsc package, APT source/key, and first-pass
experiment-created Docker images were removed after testing. The strict rerun
temporarily registered two runtimes backed by a checksum-verified binary in
`/tmp`, then removed both runtime entries and the binary. Docker networking and
an ordinary runc container were verified afterward. Docker and containerd
remained running, the original daemon configuration was restored, and Yama
remained at its original value of 1. The Ubuntu package upgrades were not
downgraded. The strict rerun retained local Docker build images for the probe,
minimal reproduction, and injector.
