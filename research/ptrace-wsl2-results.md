# Descendant ptrace under WSL2

Date: 2026-08-24

## Conclusion

A current WSL2 distribution permits every descendant-ptrace primitive
exercised by the shared environment probe, and the current first-exec
`inject_demo` passes. Both ran as an unprivileged user with zero effective
capabilities. No `SYS_PTRACE`, seccomp override, Windows reboot, or preinstalled
Linux distribution was required.

This was tested directly on a GitHub-hosted Windows Server 2025 runner. The
stock runner had WSL2 itself but no distribution. The job downloaded the
official Ubuntu 24.04.4 WSL image and successfully ran:

```powershell
wsl.exe --import FspyPtraceProbe C:\fspy-ptrace-wsl2 `
  C:\fspy-ptrace-artifacts\ubuntu-24.04.4-wsl-amd64.wsl `
  --version 2
```

The import completed without a reboot. `wsl.exe --list --verbose` reported the
new distribution as version 2, and Linux identified its kernel as
`microsoft-standard-WSL2`. This is direct WSL2 evidence, not a WSL1 or ordinary
Linux substitute.

The scope is specifically an imported Ubuntu distribution running directly
under WSL2 on hosted Windows. It does **not** test Docker Desktop's WSL2 backend,
Docker Desktop distribution integration, or a container nested inside WSL2.
Those layers can add their own seccomp, capability, user-namespace, and Yama
policy and still require a separate run.

Successful run:
[GitHub Actions 32652094818](https://github.com/voidzero-dev/vite-task/actions/runs/32652094818)

Reproducer artifact:
[`ptrace-wsl2-experiment.yml`](ptrace-wsl2-experiment.yml)

## Tested environment

| Component                 | Observed value                              |
| ------------------------- | ------------------------------------------- |
| Hosted runner             | GitHub Actions `windows-latest`             |
| Windows                   | Windows Server 2025 Datacenter, build 26100 |
| Runner image              | `windows-2025-vs2026`, `20260818.207.1`     |
| Hypervisor                | Present                                     |
| WSL                       | 2.7.11.0; default version 2                 |
| WSL kernel package        | 6.18.33.2-2                                 |
| Imported distribution     | Official Ubuntu 24.04.4 LTS AMD64 WSL image |
| Guest kernel              | `6.18.33.2-microsoft-standard-WSL2`         |
| Guest init                | `/sbin/init`                                |
| Guest Yama                | `kernel.yama.ptrace_scope=1`                |
| Unprivileged identity     | UID/GID 1001                                |
| Unprivileged capabilities | `CapEff=0`                                  |
| Guest seccomp             | `Seccomp=0`                                 |

The Ubuntu image was pinned to:

```text
https://releases.ubuntu.com/24.04/ubuntu-24.04.4-wsl-amd64.wsl
SHA-256 9b2f7730dc68227dd04a9f3e5eab86ad85caf556b8606ad94f1f29ff5c4fd3f5
```

## Exact unprivileged probe result

The job ran [`ptrace-environment-probe.c`](ptrace-environment-probe.c) as UID
1001, not just as WSL root:

```text
pid=558 uid=1001 euid=1001
CapEff:                          0000000000000000
NoNewPrivs:                     0
Seccomp:                        0
YamaScope:                      1
traceme+exec+regset            result=PASS errno=0 (none) SIGTRAP exec-stop
attach-direct-child            result=PASS errno=0 (none)
seize-direct-child             result=PASS errno=0 (none)
process-vm-write               result=PASS errno=0 (none) direct child
process-vm-read                result=PASS errno=0 (none) direct child
seize-live-grandchild          result=PASS errno=0 (none) ancestor, not direct parent
seize-sibling                  result=FAIL errno=1 (Operation not permitted) same UID
seize-sibling-pr-set-ptracer   result=PASS errno=0 (none) target opted in
seize-dumpable-zero-child      result=FAIL errno=1 (Operation not permitted)
seize-orphan-no-subreaper      result=FAIL errno=1 (Operation not permitted) reparented away
seize-orphan-subreaper         result=PASS errno=0 (none) reparented to supervisor
```

The sibling, non-dumpable, and reparented-away failures are the expected Yama
scope-1 and core ptrace access checks. The relationship pattern matches the
ordinary Linux, Docker, GitHub Actions, Kubernetes, and gVisor experiments.

The setuid subtest also matched native Linux semantics:

```text
suid-target ruid=1001 euid=0
setuid-exec-untraced           result=PASS errno=0 (none) expected euid root
suid-target ruid=1001 euid=1001
setuid-exec-traced             result=PASS errno=0 (none) expected euid unchanged
```

Tracing therefore suppresses a privilege-changing exec in WSL2 just as it does
on ordinary Linux.

## Current injection demo

The workflow built the current x86-64 `inject_demo` on the hosted Ubuntu
runner, transferred it as a workflow artifact, and executed the identical
binary inside WSL2 as both root and UID 1001. Both runs completed with exit
code zero.

Representative unprivileged output:

```text
payload: 40960 bytes, entry +0x3550, 141 relocations
mapped 41152 bytes into the target
fspy_preload_linux: installed SIGSYS handler
detached — payload will restore the exec context in-process
openat: /etc/ld.so.cache
openat: /lib/x86_64-linux-gnu/libc.so.6
openat: test_path
SIGSYS works
/bin/cat exited with code 0
```

This validates the current complete first-exec proof under WSL2:

1. the post-exec ptrace stop;
2. register and instruction manipulation for a remote mapping;
3. injected Rust payload transfer and relocation;
4. detach and in-process execution;
5. installation of the SIGSYS handler;
6. interception and reissue of `/bin/cat`'s `openat` calls.

As in the other environment results, the current demo still uses
`PTRACE_TRACEME` for its first exec. It does not implement the final detached
handler-to-supervisor protocol for every descendant exec, and this WSL2 run did
not combine `PTRACE_SEIZE`, `PTRACE_O_TRACEEXEC`, and `PTRACE_EVENT_EXEC` into
that final handshake.

## Reproduction and cleanup

The exact workflow is retained on remote branch
`research/ptrace-wsl2-20260824` at commit `70040669`. It:

1. builds `ptrace-environment-probe` and `inject_demo` on `ubuntu-latest`;
2. downloads and verifies the pinned Ubuntu WSL image;
3. imports it explicitly with `--version 2`;
4. verifies the WSL2 version and kernel identity;
5. runs the probe as UID 1001 and as root;
6. runs `inject_demo` as UID 1001 and as root;
7. terminates and unregisters the distribution in an `always()` cleanup step.

The successful job unregistered `FspyPtraceProbe` and removed its Windows-side
install and artifact directories. Its uploaded binary artifact has one-day
retention. The remote experimental branch remains for reproduction.

Three earlier runs failed due only to experiment-harness issues:

- [32651787911](https://github.com/voidzero-dev/vite-task/actions/runs/32651787911): PowerShell did not parse the downloaded checksum list as text.
- [32651904326](https://github.com/voidzero-dev/vite-task/actions/runs/32651904326): `wsl --import` succeeded and reported version 2, but the assertion did not strip UTF-16 NULs from `wsl.exe` output.
- [32651996398](https://github.com/voidzero-dev/vite-task/actions/runs/32651996398): import and version validation succeeded, but a CRLF PowerShell here-string reached Bash as `pipefail\r`.

Each run's `always()` cleanup removed any distribution that had been imported.
None of these failures indicates a ptrace or WSL2 limitation.

The successful capture used the probe's earlier print-only exit behavior. The
individual result lines and both injector exit codes were inspected directly.
The locally retained probe now adds ptrace word I/O and returns nonzero when a
required positive descendant operation fails, so future reproductions do not
depend on log inspection alone.
