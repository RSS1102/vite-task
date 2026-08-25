# Hardened ptrace rerun on hosted CI, WSL2, and x86-64 gVisor

Date: 2026-08-24

## Conclusion

The exit-enforcing descendant-ptrace probe and the current explicit-trap
`inject_demo` pass without `SYS_PTRACE`, `seccomp=unconfined`, or a privileged
container in all of these tested placements:

- the GitHub-hosted Ubuntu VM as UID 1001;
- Docker's default runc profile as root and UID 65534;
- a GitHub Actions `container: ubuntu:24.04` job as root and UID 65534;
- an official Ubuntu 24.04.4 image imported as a genuine WSL2 distribution,
  as root and UID 1001;
- x86-64 gVisor/runsc as root and UID 65534.

The evidence has two exact probe revisions. The broad hosted Ubuntu, Docker,
job-container, and WSL2 capture used source SHA-256 `abbb06bc…14c7`. A later
audit hardened every wait/stop/detach path; x86-64 gVisor was freshly rerun with
the resulting current source `da027737…34277a`. Do not attribute the current
probe revision to the broader placements without another rerun.

The full explicit seccomp-deny control returned exit code 1 for both the probe
and injector, under both tested Docker identities. The targeted
`PR_SET_PTRACER`-deny control did not abort or hang: it printed the failed
optional subtest, continued, passed ptrace word I/O, printed a passing required
summary, and exited zero.

The x86-64 gVisor result is important. The same release's ARM64 sandbox has a
separate `SECCOMP_RET_TRAP` argument-register rollback incompatibility, but the
x86-64 injector completed its SIGSYS `openat` emulation. The incompatibility is
therefore architecture-specific in `runsc release-20260817.0`, not a general
gVisor limitation.

Successful runs:

- [hosted Ubuntu, Docker, job container, and imported WSL2 run
  32653068642](https://github.com/voidzero-dev/vite-task/actions/runs/32653068642),
  commit `83c9036182a8c95bed3264c20d902318fee67043`;
- [x86-64 gVisor run
  32654193527](https://github.com/voidzero-dev/vite-task/actions/runs/32654193527),
  commit `5a4120e9061b142c1cb5c8c917fe0de7190b9874`.

## Exact tested artifacts

Both workflows built the same injector snapshot, but different exact probe
revisions:

| Coverage                                   | Probe source SHA-256                                               | Probe binary SHA-256                                               | `inject_demo` binary SHA-256                                       |
| ------------------------------------------ | ------------------------------------------------------------------ | ------------------------------------------------------------------ | ------------------------------------------------------------------ |
| hosted Ubuntu, Docker, job container, WSL2 | `abbb06bc7fafd0dcaaceabf1dfd23d300613e2fbf03cf49c4ba7ab4d1ec814c7` | `be4aa8aae2fd516762912423425128c78bcb932e723705e3c9ce4bbfcea02fb5` | `88e01482356cd076db64680da00f86534e7fa8bd98ea8fd62f377b915fa00d84` |
| x86-64 gVisor/runsc                        | `da027737ccb9cc41329ba20cc43d9d01b5371c8bfa0bd67b48ffc0452534277a` | `cce552cb641a7d7a0148c3e0d1ca0c7bb49ad057c0a7127695181859a4650081` | `88e01482356cd076db64680da00f86534e7fa8bd98ea8fd62f377b915fa00d84` |

The probe is the current
[`ptrace-environment-probe.c`](ptrace-environment-probe.c). Compared with the
older hosted captures, it now:

- requires essential positive descendant operations to pass;
- requires a stopped-child `PTRACE_PEEKDATA`/`PTRACE_POKEDATA` round trip;
- prints `required-summary`;
- exits nonzero if a required operation fails;
- reports a failed `PR_SET_PTRACER` call and continues instead of aborting.

`process_vm_readv` and `process_vm_writev` remain diagnostic rather than
required because ptrace word I/O is the supported fallback. The expected
scope-1 failures for a sibling without opt-in, a non-dumpable child, and an
orphan reparented away from the supervisor also do not fail the required
summary.

The injector snapshot uses `syscall; int3` followed by `PTRACE_CONT` on x86-64,
not `PTRACE_SINGLESTEP`. The experimental branches copied that already-current
working-tree implementation so hosted builders could compile the exact code;
this experiment did not author another product-code change.

## Exit-code matrix

Every zero in this table was asserted by the workflow. Probe success also
required both `ptrace-word-io result=PASS` and
`required-summary result=PASS`. Injector success required both
`openat: test_path` and `/bin/cat exited with code 0`.

| Placement                     | Identity  | Probe source      | Probe | Setuid subtest | Injector |
| ----------------------------- | --------- | ----------------- | ----: | -------------: | -------: |
| hosted Ubuntu VM              | UID 1001  | `abbb06bc…14c7`   |     0 |              0 |        0 |
| hosted Ubuntu VM              | root      | `abbb06bc…14c7`   |     0 |        not run |        0 |
| Docker default/runc           | root      | `abbb06bc…14c7`   |     0 |        not run |        0 |
| Docker default/runc           | UID 65534 | `abbb06bc…14c7`   |     0 |              0 |        0 |
| Actions default job container | root      | `abbb06bc…14c7`   |     0 |        not run |        0 |
| Actions default job container | UID 65534 | `abbb06bc…14c7`   |     0 |              0 |        0 |
| imported Ubuntu WSL2          | root      | `abbb06bc…14c7`   |     0 |        not run |        0 |
| imported Ubuntu WSL2          | UID 1001  | `abbb06bc…14c7`   |     0 |              0 |        0 |
| x86-64 gVisor/runsc           | root      | `da027737…34277a` |     0 |        not run |        0 |
| x86-64 gVisor/runsc           | UID 65534 | `da027737…34277a` |     0 |        not run |        0 |

The setuid tests verified the expected Linux behavior: an untraced exec gained
effective UID 0, while a traced exec retained the calling unprivileged UID.

## Hosted Ubuntu and default-container environment

| Component                    | Observed value                                 |
| ---------------------------- | ---------------------------------------------- |
| Actions image                | `ubuntu-24.04`, image version `20260816.277.1` |
| Host OS                      | Ubuntu 24.04.4 LTS                             |
| Host kernel                  | `6.17.0-1022-azure`, x86-64                    |
| Runner identity              | UID/GID 1001                                   |
| Runner `CapEff`              | `0000000000000000`                             |
| Runner seccomp               | `Seccomp: 0`                                   |
| Host Yama                    | `kernel.yama.ptrace_scope=1`                   |
| Docker Engine                | 28.0.4                                         |
| containerd                   | 2.3.3                                          |
| runc                         | 1.4.3                                          |
| Docker security options      | AppArmor, built-in seccomp, cgroup namespace   |
| Docker/job-container seccomp | `Seccomp: 2`                                   |
| UID 65534 `CapEff`           | `0000000000000000`                             |

The Actions job container used the same Azure kernel and Yama setting as its
host. PID 1 and the test process both reported seccomp filtering. Neither the
ordinary Docker commands nor the job-container declaration added a ptrace
capability or security override.

## Hardened negative controls

The full deny profile returns `EPERM` for `ptrace`, `process_vm_readv`, and
`process_vm_writev`. It produced:

| Docker identity | Probe exit | Injector exit |
| --------------- | ---------: | ------------: |
| root            |          1 |             1 |
| UID 65534       |          1 |             1 |

Both probe runs printed:

```text
ptrace-word-io                 result=FAIL errno=1 (Operation not permitted) seize failed before word I/O
required-summary               result=FAIL errno=0 (none) one or more required operations failed
```

The separate
[`pr-set-ptracer-deny-seccomp.json`](pr-set-ptracer-deny-seccomp.json) denies
only `prctl(PR_SET_PTRACER, ...)`, by matching argument zero against decimal
`1499557217`. That run printed:

```text
ptrace-word-io                 result=PASS errno=0 (none) stopped direct child
seize-sibling-pr-set-ptracer   result=FAIL errno=1 (Operation not permitted) PR_SET_PTRACER failed
required-summary               result=PASS errno=0 (none)
DOCKER_PR_SET_PTRACER_DENY_PROBE_EXIT=0
```

This proves graceful probe behavior. It does not mean a production protocol
that requires target-side `PR_SET_PTRACER` could ignore that denial.

## Genuine WSL2 environment

The Windows job downloaded the pinned official Ubuntu WSL image, verified its
SHA-256, imported it explicitly with `--version 2`, and verified that
`wsl.exe --list --verbose` reported version 2 before running Linux code.

| Component          | Observed value                                 |
| ------------------ | ---------------------------------------------- |
| Actions image      | `win25-vs2026`, image version `20260818.207.1` |
| Windows            | Server 2025 Datacenter, build 26100            |
| Hypervisor         | present                                        |
| WSL                | 2.7.11.0; default version 2                    |
| WSL kernel package | 6.18.33.2-2                                    |
| Imported image     | Ubuntu 24.04.4 LTS AMD64 WSL image             |
| Guest kernel       | `6.18.33.2-microsoft-standard-WSL2`            |
| Guest PID 1        | `/sbin/init`                                   |
| Guest Yama         | `kernel.yama.ptrace_scope=1`                   |
| UID 1001 `CapEff`  | `0000000000000000`                             |
| Guest seccomp      | `Seccomp: 0`                                   |

The image URL and digest were:

```text
https://releases.ubuntu.com/24.04/ubuntu-24.04.4-wsl-amd64.wsl
9b2f7730dc68227dd04a9f3e5eab86ad85caf556b8606ad94f1f29ff5c4fd3f5
```

Both identities completed the complete current demonstration: exec-stop,
register access, remote mapping through the explicit trap, ptrace word
transfer, payload relocation, detach, SIGSYS installation, `openat`
interception and reissue, and normal `/bin/cat` exit.

This remains a first-exec `PTRACE_TRACEME` proof. It does not by itself validate
the future detached SIGSYS-handler to supervisor `PR_SET_PTRACER`/`SEIZE`
handshake for every descendant exec.

## x86-64 gVisor result

The final [gVisor workflow run](https://github.com/voidzero-dev/vite-task/actions/runs/32654193527)
used commit `5a4120e9061b142c1cb5c8c917fe0de7190b9874` and current probe source
`da027737ccb9cc41329ba20cc43d9d01b5371c8bfa0bd67b48ffc0452534277a`.
It installed the official pinned tarball, verified its published SHA-512 file,
registered runsc with Docker, and ran with `--init` so the orphan/subreaper
topology matched the other container tests.

| Component         | Observed value                                              |
| ----------------- | ----------------------------------------------------------- |
| Host              | the same Ubuntu Actions image and x86-64 Azure kernel above |
| runsc             | `release-20260817.0`                                        |
| OCI spec          | 1.2.1                                                       |
| platform          | systrap                                                     |
| OCI seccomp       | disabled by runsc default; guest `Seccomp: 0`               |
| guest Yama        | `kernel.yama.ptrace_scope=1`                                |
| non-root identity | UID 65534, `CapEff=0`                                       |

Exact outcomes:

| Identity  | Hardened probe | Current injector |
| --------- | -------------: | ---------------: |
| root      |              0 |                0 |
| UID 65534 |              0 |                0 |

Both probe runs exited zero, passed ptrace word I/O and the required summary,
and were checked by the workflow. With the separate init process, the same-UID
sibling, non-dumpable child, and orphan reparented away from the supervisor
failed with `EPERM` as expected under Yama scope 1. The opted-in sibling and
subreaper case passed.

Both injector runs exited zero. The workflow required the exit status and both
of its externally observable success markers; the logs also printed the full
sequence:

```text
fspy_preload_linux: installed SIGSYS handler
openat: test_path
SIGSYS works
/bin/cat exited with code 0
```

This proves that x86-64 runsc both accepts the `syscall; int3` remote-syscall
mechanism and supplies a SIGSYS context compatible with the current `openat`
handler. It narrows the separately observed ARM64 failure to the ARM64 signal
context/syscall-rollback implementation in this release.

## Probe revision scope

The broad hosted Ubuntu, default-container, and WSL2 results above used probe
SHA-256
`abbb06bc7fafd0dcaaceabf1dfd23d300613e2fbf03cf49c4ba7ab4d1ec814c7`.
The Kubernetes and AI-sandbox notes likewise identify that earlier source
where applicable. Those captures remain evidence for exactly that revision.

Review then found unchecked wait-status paths and successful `PTRACE_SEIZE`
tests that attempted `PTRACE_DETACH` before stopping the tracee. The current
probe checks every wait result, interrupt-stops seized tracees before detach,
and reaps through intermediate ptrace stops. Its SHA-256 is
`da027737ccb9cc41329ba20cc43d9d01b5371c8bfa0bd67b48ffc0452534277a`.

The x86-64 gVisor run was repeated after that audit, so its result directly
covers the current revision. The same revision was compiled locally with Linux
`-Werror`; Docker default root and UID 65534 exited 0, the full deny profile
exited 1, the targeted `PR_SET_PTRACER` denial exited 0 after reporting that
optional failure, and the UID 65534 setuid suite exited 0. The other broad
external placements have not yet been rerun with this exact probe hash.

## Reproduction, cleanup, and drift

Exact workflow copies are retained as:

- [`ptrace-strict-hosted-wsl-experiment.yml`](ptrace-strict-hosted-wsl-experiment.yml);
- [`ptrace-gvisor-x86-experiment.yml`](ptrace-gvisor-x86-experiment.yml).

The corresponding remote experiment branches remain:

- `research/ptrace-strict-hosted-wsl-20260824` at `83c90361`;
- `research/ptrace-gvisor-x86-20260824` at `5a4120e9`.

The successful WSL job terminated and unregistered `FspyStrictPtrace`, then
removed the Windows-side distro and artifact directories. The successful
gVisor job ran `runsc uninstall`, reloaded Docker, and removed the downloaded
release and installed runsc files. A final assertion verified that the runsc
binary and shim were absent and Docker no longer advertised the runtime.
GitHub-hosted VMs are disposable. The strict hosted/WSL run's uploaded binaries
have one-day retention.

One earlier fresh run,
[32652959308](https://github.com/voidzero-dev/vite-task/actions/runs/32652959308)
at `c081522c`, passed the hosted VM, Docker, default job-container, targeted
denial, and explicit-denial jobs. Its WSL job imported and booted Ubuntu but a
PowerShell-to-`bash -lc` transport expanded Bash positional variables before
the probe ran. Its `always()` cleanup successfully unregistered the distro.
Commit `83c90361` changed only that script transport to base64-decode the exact
script inside WSL2.

The first x86-64 gVisor run,
[32653154421](https://github.com/voidzero-dev/vite-task/actions/runs/32653154421),
also passed. Its probe was PID 1, however, so ordinary orphan reparenting made
the probe the new parent even without subreaper mode. Run
[32653253454](https://github.com/voidzero-dev/vite-task/actions/runs/32653253454)
added Docker's init process and passed with the earlier `abbb06bc…14c7` probe,
but its workflow captured rather than asserted the two runsc injector results.
The current run supersedes it with the fully audited probe plus explicit exit
and marker assertions.

Two intermediate reruns were cancelled when further probe audit fixes landed:
[32654096201](https://github.com/voidzero-dev/vite-task/actions/runs/32654096201)
and [32654149729](https://github.com/voidzero-dev/vite-task/actions/runs/32654149729).
Neither reached the probe or injector steps. Both cancellation paths executed
and passed the runtime removal and verification steps.

Local preflight built a temporary ARM64 Docker image and confirmed default
probe exits 0, full-deny exits 1, and targeted `PR_SET_PTRACER` denial exits 0.
That image and its temporary logs were removed after validation. Two temporary
git worktrees and their ignored `mise.local.toml` files were also removed. No
changes were made to the user's existing `Cargo.lock`, `fspy_loader`, or other
dirty working-tree state. The remote experiment branches and run logs are the
only intentional external drift.
