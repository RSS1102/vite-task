# Ptrace-at-exec compatibility in constrained Linux environments

Date: 2026-08-24

## Conclusion

Tracing only processes owned by fspy is substantially more deployable than the
usual advice to add `SYS_PTRACE` suggests.

On the tested modern Docker default, an unprivileged process can use all the
operations needed by fspy against its descendants:

- `PTRACE_TRACEME` and the post-`execve` `SIGTRAP` stop;
- `PTRACE_ATTACH` and `PTRACE_SEIZE`;
- `PTRACE_GETREGSET`, `PTRACE_SETREGSET`, and ptrace detach/resume;
- `process_vm_readv` and `process_vm_writev`.

This passed without `--cap-add SYS_PTRACE`, without
`--security-opt seccomp=unconfined`, as both container root and UID 65534. The
real `inject_demo` also completed under both identities with Docker's default
profile.

The result was reproduced directly on GitHub Actions `ubuntu-latest`: the
unprivileged host runner, Docker containers launched by that runner, and a real
Actions `container:` job all passed the descendant operations. The host runner
had no effective capabilities; both container forms had seccomp mode 2 and no
`SYS_PTRACE`. The current `inject_demo` completed as root and non-root in all
three placements. A fresh strict rerun also required ptrace word I/O, the
passing summary, and the current explicit-trap injector's zero exit. See
[run 32653068642](https://github.com/voidzero-dev/vite-task/actions/runs/32653068642)
and the [hardened rerun note](ptrace-strict-rerun-results.md).

This is not universal. Any one of these independently blocks the design:

1. a seccomp profile denying the required `ptrace` requests; denying only
   `process_vm_writev` disables the bulk-transfer fast path, not the ptrace
   word-I/O fallback;
2. Yama `ptrace_scope=2` without `CAP_SYS_PTRACE`, or `ptrace_scope=3`;
3. a target that is not dumpable;
4. a target whose credentials no longer match the supervisor;
5. another LSM denying the relationship;
6. a runtime with an incomplete ptrace implementation.

For Yama's common `ptrace_scope=1`, being a descendant is sufficient; being the
direct child is not necessary. Reparenting the target away from the supervisor
loses that permission. `PR_SET_PTRACER` in the target or making the supervisor a
subreaper restores it in the experiment. Calling `PR_SET_PTRACER` from every
exec-handshake handler is the narrower design.

The production conclusion should therefore be **probe and use ptrace**, not
**require `SYS_PTRACE`**, and retain an unsupported/fallback path for hardened
environments.

## What was tested

The reusable probe is
[`ptrace-environment-probe.c`](ptrace-environment-probe.c). The explicit deny
profile is [`ptrace-deny-seccomp.json`](ptrace-deny-seccomp.json).

The current probe exits nonzero when a required positive descendant operation
fails. Deliberately negative relationship checks do not affect its exit status,
and process-VM failure is reported without failing the summary when ptrace word
I/O succeeds. A fresh hosted/WSL2 rerun enforces that strict summary and exact
injector exit status. Older provider captures remain useful where no fresh run
was possible, and their individual result lines were inspected directly.

Local platform:

| Component               | Value                                            |
| ----------------------- | ------------------------------------------------ |
| Host                    | macOS arm64 with Colima                          |
| Linux guest             | Ubuntu 24.04.4, Linux `6.8.0-100-generic`, arm64 |
| Docker client/server    | 29.7.1 / 29.2.1                                  |
| Runtime                 | runc                                             |
| Docker security options | AppArmor, builtin seccomp, cgroup namespace      |
| Initial Yama setting    | `kernel.yama.ptrace_scope=1`                     |

GitHub-hosted platform in the successful run:

| Component               | Value                                               |
| ----------------------- | --------------------------------------------------- |
| Runner image            | `ubuntu-24.04`, image `20260816.277.1`              |
| Linux kernel            | Azure `6.17.0-1022-azure`, x86-64                   |
| Host identity           | UID 1001, `CapEff=0`, seccomp mode 0                |
| Docker                  | Engine 28.0.4, containerd 2.3.3, runc 1.4.3         |
| Docker security options | AppArmor, builtin seccomp, cgroup namespace         |
| Actions job container   | `ubuntu:24.04`, seccomp mode 2, no `SYS_PTRACE`     |
| Yama                    | `kernel.yama.ptrace_scope=1` in host and containers |

The probe deliberately reports exact `errno` and covers relationships that a
simple `strace true` test misses.

### Reproduce the probe image

From the repository root:

```bash
docker build -t fspy-ptrace-probe -f- research <<'DOCKERFILE'
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc libc6-dev \
 && rm -rf /var/lib/apt/lists/*
COPY ptrace-environment-probe.c /tmp/probe.c
RUN gcc -O2 -Wall -Wextra -Werror -o /probe-normal /tmp/probe.c \
 && cp /probe-normal /probe-suid \
 && chown root:root /probe-suid \
 && chmod 4755 /probe-suid
ENTRYPOINT ["/probe-normal"]
DOCKERFILE
```

`--init` is intentional: it prevents the probe itself from becoming PID 1 and
adopting every orphan, so the no-subreaper case is meaningful.

```bash
docker run --rm --init fspy-ptrace-probe
docker run --rm --init --user 65534:65534 fspy-ptrace-probe
```

## Direct experimental results

### Modern Docker default

Both UID 0 and UID 65534 produced the same result. UID 65534 had an empty
effective capability set. `Seccomp: 2` confirmed that Docker's filter was
active.

| Probe                                       | Result  | Meaning for fspy                                              |
| ------------------------------------------- | ------- | ------------------------------------------------------------- |
| `TRACEME` + exec + `GETREGSET`/`SETREGSET`  | Pass    | Initial exec-stop injection works.                            |
| `ATTACH` direct child                       | Pass    | Capability is not required for an owned child.                |
| `SEIZE` direct child                        | Pass    | Preferred non-stopping attach works.                          |
| `process_vm_readv`/`writev` direct child    | Pass    | Bulk payload transfer works.                                  |
| `SEIZE` live grandchild                     | Pass    | Yama scope 1 accepts an ancestor, not only the direct parent. |
| `SEIZE` same-UID sibling                    | `EPERM` | Same UID alone is insufficient under Yama scope 1.            |
| sibling after target `PR_SET_PTRACER`       | Pass    | Explicit target opt-in satisfies Yama scope 1.                |
| direct child with `PR_SET_DUMPABLE=0`       | `EPERM` | Ancestry does not override the core dumpability check.        |
| orphan reparented away                      | `EPERM` | The ancestry permission is lost after reparenting.            |
| orphan reparented to a subreaper supervisor | Pass    | Maintaining ancestry satisfies Yama.                          |

Adding `--security-opt seccomp=unconfined` did not change the relationship
results. Adding `--security-opt no-new-privileges=true` also did not change
them. `no_new_privs` is therefore not itself a ptrace restriction.

Adding `--cap-add SYS_PTRACE` made sibling, non-dumpable, and reparented cases
pass. It is a broad bypass, not a requirement for the ordinary descendant case.

### Why Docker's default allows this

Docker's documentation describes `ptrace` and `process_vm_*` among calls that
can be blocked by the default policy, which is easy to read as an unconditional
denial. The actual maintained
[`moby/profiles` default](https://github.com/moby/profiles/blob/main/seccomp/default.json)
allows `ptrace`, `process_vm_readv`, and `process_vm_writev` on kernels 4.8 and
newer. It also allows them when `CAP_SYS_PTRACE` is present. Docker explains that
the pre-4.8 denial existed to prevent a seccomp bypass; modern kernels fixed
that issue. See the official
[`seccomp` documentation](https://docs.docker.com/engine/security/seccomp/).

Containerd's maintained
[`DefaultProfile`](https://github.com/containerd/containerd/blob/main/contrib/seccomp/seccomp_default.go)
has the same kernel-4.8 conditional. The containers/common profile used by
Podman also lists all three calls in its ordinary allowlist.

Consequently, `Seccomp: 2` does **not** mean ptrace is unavailable. The actual
filter rules matter.

### Explicit seccomp denial

```bash
docker run --rm --init \
  --security-opt "seccomp=$PWD/research/ptrace-deny-seccomp.json" \
  fspy-ptrace-probe
```

Every ptrace operation and both `process_vm_*` operations failed with `EPERM`,
including `PTRACE_TRACEME`, direct-child `PTRACE_SEIZE`, and the
`PR_SET_PTRACER` case. Repeating the run with `--cap-add SYS_PTRACE` produced
the same failures: a capability cannot override a seccomp rule that directly
returns `EPERM`.

The real `inject_demo` failed in its child `pre_exec` hook with `EPERM` under
this profile, before `/bin/cat` could exec.

### Yama policy

The Colima VM's global Yama value was changed temporarily and restored to 1.
The relevant commands were:

```bash
colima ssh -- sudo sysctl -w kernel.yama.ptrace_scope=0
docker run --rm --init fspy-ptrace-probe

colima ssh -- sudo sysctl -w kernel.yama.ptrace_scope=2
docker run --rm --init fspy-ptrace-probe
docker run --rm --init --cap-add SYS_PTRACE fspy-ptrace-probe

colima ssh -- sudo sysctl -w kernel.yama.ptrace_scope=1
```

These commands change a VM-wide security policy. Do not set scope 3 on a
shared or long-lived kernel: Linux does not permit lowering it again before a
reboot.

| `ptrace_scope` | No `SYS_PTRACE`                                      | With `SYS_PTRACE`             | Experimental result                                                                          |
| -------------: | ---------------------------------------------------- | ----------------------------- | -------------------------------------------------------------------------------------------- |
|              0 | Same-credential, dumpable tasks                      | Bypasses normal access checks | Child, grandchild, sibling, and reparented target passed; dumpable-zero failed.              |
|              1 | Descendants or `PR_SET_PTRACER`, plus classic checks | Bypasses Yama/classic checks  | Results in the Docker-default table above.                                                   |
|              2 | No ptrace, including `TRACEME`                       | Allowed                       | Every no-capability case failed; every capability case passed.                               |
|              3 | No ptrace                                            | No ptrace                     | Not set locally because the kernel intentionally makes this value irreversible until reboot. |

The scope-3 result is documented by the kernel rather than inferred. See
[Yama's `ptrace_scope` documentation](https://docs.kernel.org/admin-guide/LSM/Yama.html#ptrace-scope).

The core ptrace check additionally requires matching real/effective/saved IDs,
a dumpable target, and approval from all active LSMs, unless the caller has
`CAP_SYS_PTRACE` in the **target's user namespace**. The full access algorithm
is documented in [`ptrace(2)`](https://man7.org/linux/man-pages/man2/ptrace.2.html).
`process_vm_readv` and `process_vm_writev` use a ptrace attach access check too,
so they fail under the same credential, dumpability, Yama, and LSM boundaries.

### Full fspy injection proof

The current AArch64 demo was cross-built and run in the default container:

```bash
cargo zigbuild -p inject_demo --target aarch64-unknown-linux-gnu

target_dir="$(cargo metadata --format-version 1 --no-deps \
  | jq -r .target_directory)/aarch64-unknown-linux-gnu/debug"
docker build -t fspy-inject-demo -f- "$target_dir" <<'DOCKERFILE'
FROM debian:bookworm-slim
COPY inject_demo /inject_demo
ENTRYPOINT ["/inject_demo"]
DOCKERFILE

docker run --rm fspy-inject-demo
docker run --rm --user 65534:65534 fspy-inject-demo
```

The resulting image was tested as root and UID 65534. Both runs:

1. received the post-exec ptrace stop;
2. remotely mapped and wrote the Rust payload;
3. detached;
4. installed the in-process `SIGSYS` handler;
5. reported libc and `test_path` `openat` calls;
6. printed `SIGSYS works` and exited zero.

No capability or seccomp override was supplied. This validates more than syscall
availability: it exercises the actual register access, remote syscall,
word-at-a-time payload write, detach, and resumed target.

This demo uses `PTRACE_TRACEME` before the first exec. It does **not** yet
validate the proposed detached descendant-exec handshake based on
`PTRACE_SEIZE`; the primitive probe validates that attach permission, but the
end-to-end handshake remains implementation work.

### Privilege-changing exec

The probe image contains a root-owned setuid helper and a non-setuid launcher.
Running the launcher as UID 65534 produced:

| Case                                 | Effective UID after exec |
| ------------------------------------ | -----------------------: |
| Untraced setuid exec                 |                        0 |
| `PTRACE_TRACEME` then setuid exec    |                    65534 |
| `no_new_privs`, untraced setuid exec |                    65534 |

Tracing suppresses setuid/setgid and file-capability elevation by Linux design;
fspy's required `no_new_privs` already does the same. Ptrace support must not be
described as preserving privilege-changing exec semantics.

### User and PID namespaces

Docker already places the probe in a separate PID namespace, so the passing
default tests validate a supervisor and descendants in one container PID
namespace.

A nested user namespace was also created with `unshare -Ur` (the outer Docker
filter had to be disabled because Docker blocks the namespace-creation syscall).
Inside that namespace the probe again passed. This shows that a user namespace
does not inherently prevent tracing tasks in the same namespace.

```bash
docker run --rm --init --security-opt seccomp=unconfined \
  --entrypoint unshare fspy-ptrace-probe -Ur /probe-normal
```

This was **not** a rootless Docker daemon test. Docker documents that rootless
mode places both daemon and containers in a user namespace. Based on the kernel
rules and nested-namespace result, supervisor and target in the same rootless
container should retain the descendant path, subject to its seccomp and Yama
configuration. That is an inference requiring a real rootless-Docker CI job.

If a descendant changes to an unmapped UID or moves under a user namespace in a
way that makes the supervisor fail the target-namespace credential check,
ptrace can still fail. Browser sandboxes need explicit end-to-end coverage for
this reason.

## Environment matrix

“Likely” means the platform documentation establishes the relevant container or
VM mechanism, and kernel/runtime sources establish the expected result, but the
probe was not run on that provider.

| Environment                                                   | Descendant ptrace expectation                                                | Evidence and caveats                                                                                                                                                                                                                                                                                                                                                                     |
| ------------------------------------------------------------- | ---------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Modern Docker/runc default, kernel >= 4.8                     | **Yes, tested**                                                              | Full primitive probe and `inject_demo`; no capability or profile override.                                                                                                                                                                                                                                                                                                               |
| Docker as non-root user                                       | **Yes, tested**                                                              | UID 65534, zero effective capabilities.                                                                                                                                                                                                                                                                                                                                                  |
| Docker with `no_new_privs`                                    | **Yes, tested**                                                              | It affects privilege elevation, not ptrace permission.                                                                                                                                                                                                                                                                                                                                   |
| Docker custom profile denying ptrace                          | **No, tested**                                                               | `EPERM` for all requests.                                                                                                                                                                                                                                                                                                                                                                |
| Host Yama scope 0 or 1                                        | **Yes for a dumpable same-credential descendant, tested in VM**              | Scope 1 requires maintained ancestry or per-target `PR_SET_PTRACER`.                                                                                                                                                                                                                                                                                                                     |
| Host Yama scope 2                                             | **No without capability, tested**                                            | `SYS_PTRACE` restored access.                                                                                                                                                                                                                                                                                                                                                            |
| Host Yama scope 3                                             | **No, documented**                                                           | Capability does not override; reboot is needed after setting it.                                                                                                                                                                                                                                                                                                                         |
| Docker with `userns-remap`                                    | **Yes, tested**                                                              | Full probe and `inject_demo` passed as container root and capability-free UID 65534 under Docker's stock seccomp and AppArmor profiles.                                                                                                                                                                                                                                                  |
| Rootless Docker                                               | **Not directly tested**                                                      | Rootless Podman passed, and Docker `userns-remap` passed separately, but neither is identical to a rootless Docker daemon.                                                                                                                                                                                                                                                               |
| Podman default                                                | **Yes, tested rootless**                                                     | Rootless Podman/runc passed the full probe and `inject_demo` without `SYS_PTRACE` or a profile override.                                                                                                                                                                                                                                                                                 |
| Kubernetes with seccomp omitted and `seccompDefault` disabled | **Yes, tested on kind/containerd/runc**                                      | The pod reported `Seccomp: 0`; the primitive probe and real `inject_demo` passed. Node Yama/LSMs remain deployment-specific.                                                                                                                                                                                                                                                             |
| Kubernetes `RuntimeDefault` on modern containerd              | **Yes, tested on kind/containerd/runc**                                      | The primitive probe and real `inject_demo` passed with `Seccomp: 2`. Runtime profiles still vary by runtime and release.                                                                                                                                                                                                                                                                 |
| Kubernetes Restricted Pod Security                            | **Yes on the tested RuntimeDefault, with no capability fallback**            | UID 65534, zero capabilities, `NoNewPrivs=1`, and drop-ALL passed. A `Localhost` profile explicitly denying ptrace returned `EPERM`; Restricted policy cannot add `SYS_PTRACE` as an escape hatch.                                                                                                                                                                                       |
| Kubernetes supervisor and target in one container             | **Yes on the tested profiles**                                               | This is fspy's intended placement and preserves the descendant relationship.                                                                                                                                                                                                                                                                                                             |
| Kubernetes sidecar supervisor                                 | **Not equivalent, tested**                                                   | With `shareProcessNamespace: true`, a same-UID sibling failed under Yama 1 until the target used `PR_SET_PTRACER`; a different-UID tracer still failed. PID visibility does not create ancestry or relax credentials.                                                                                                                                                                    |
| gVisor/runsc sandbox                                          | **Descendant ptrace passes; end-to-end passes on x86-64 but fails on ARM64** | The strict probe, exec event, register/code mutation, explicit-trap remote `mmap`, process-VM access, and detach passed as root and UID 65534 on both architectures. The current injector passes on x86-64. ARM64 runsc fails to roll argument zero back before a `SECCOMP_RET_TRAP` signal frame; relative `openat` is consequently reissued with `dirfd=SYS_openat` and fails `EBADF`. |
| Kata/Firecracker-style VM sandbox                             | **Likely standard guest-kernel behavior**                                    | Depends on the guest kernel's Yama, LSM, seccomp, and runtime profile rather than host ptrace capability. Not directly tested.                                                                                                                                                                                                                                                           |
| WSL2 distribution                                             | **Yes, tested directly**                                                     | An imported Ubuntu 24.04.4 distribution on WSL 2.7.11/kernel 6.18.33.2 passed the full unprivileged probe and current `inject_demo`. This does not cover WSL1 or Docker Desktop's additional container layer.                                                                                                                                                                            |

Kubernetes sources: [seccomp tutorial](https://kubernetes.io/docs/tutorials/security/seccomp/),
[Pod Security Standards](https://kubernetes.io/docs/concepts/security/pod-security-standards/),
and [shared process namespaces](https://kubernetes.io/docs/tasks/configure-pod-container/share-process-namespace/).
WSL source: [Comparing WSL versions](https://learn.microsoft.com/en-us/windows/wsl/compare-versions).
gVisor sources: [Linux syscall compatibility](https://gvisor.dev/docs/user_guide/compatibility/linux/amd64/)
and its [ptrace implementation](https://github.com/google/gvisor/blob/master/pkg/sentry/kernel/ptrace.go).

### Direct Kubernetes results

The complete Kubernetes matrix, exact sidecar manifest, core pod settings,
environment, and reproduction steps are in
[`ptrace-kubernetes-results.md`](ptrace-kubernetes-results.md). The directly
tested cluster was kind 0.32.0 with Kubernetes 1.36.1, containerd 2.3.1, runc
1.4.2, Linux 6.8, and Yama scope 1 on AArch64.

The important deployment distinction is topology, not the word “Kubernetes”:

- an in-container supervisor remained the ancestor of its target and needed no
  capability under the tested default and Restricted policies;
- a separate container in a shared process namespace could see the target but
  was its sibling, so Yama scope 1 denied it unless the same-UID target opted in
  with `PR_SET_PTRACER`;
- an explicit kubelet `Localhost` seccomp denial blocked both ptrace and
  `process_vm_*`, and target opt-in could not override it.

### Direct gVisor results

The complete runsc experiment and focused exec/injection probe are in
[`gvisor-ptrace-results.md`](gvisor-ptrace-results.md). Current gVisor passed
the needed descendant permission checks and ptrace primitives. The current
injector's `svc #0; brk #0` plus `PTRACE_CONT` remote syscall passed under both
runsc and runc without adding a supervisor round trip, resolving runsc's
missing ARM64 single-step support.

The exact injector then exposed a separate runsc signal-emulation drift.
Native Linux calls `syscall_rollback()` before delivering a seccomp-trap
SIGSYS; ARM64 runsc instead puts the syscall number in `x0`, losing original
argument zero in the handler's `ucontext`. The independent
[`gvisor-seccomp-trap-arg0-probe.c`](gvisor-seccomp-trap-arg0-probe.c) passed
under runc and observed `SYS_close` instead of `0x1234` under runsc. This blocks
generic in-process syscall emulation on that runtime even though ptrace itself
works.

The x86-64 control directly passed the strict probe and current injector as
root and UID 65534 on the same runsc release. The complete evidence, binary
hashes, and reproduction workflow are in
[`ptrace-strict-rerun-results.md`](ptrace-strict-rerun-results.md). The latest
[run 32654193527](https://github.com/voidzero-dev/vite-task/actions/runs/32654193527)
used the fully audited current probe `da027737…34277a` and asserted both runsc
injector exits and success markers. This narrows the signal-frame
incompatibility to ARM64 in the tested release.

### Direct rootless and user-namespace results

The complete matrix and reproduction details are in
[`rootless-container-ptrace-results.md`](rootless-container-ptrace-results.md).
Rootless Podman 4.9.3/runc 1.3.4 and an isolated Docker 29.2.1 daemon using
`userns-remap` both passed the full descendant probe and the then-current
`inject_demo`
as container root and UID 65534. The UID 65534 cases had no effective
capabilities; all cases used the runtime's stock seccomp profile without
`SYS_PTRACE`, privileged mode, or an unconfined policy.

Those runs predate the explicit-trap remote-syscall change and used
`PTRACE_SINGLESTEP`. They validate permission, mapping, payload injection,
detach, and SIGSYS handling in both namespace topologies; the current
`PTRACE_CONT` plus `svc; brk` implementation has not been rerun there.
The captured injector source is pinned to commit `66ae8432` and its source hash
is recorded in the detailed note. No hash was retained for the earlier
untracked probe revision, so the topology is reproducible but the complete
original artifact set is not byte-for-byte reproducible.

The negative Yama checks remained effective. A same-UID sibling without
`PR_SET_PTRACER` and a non-dumpable direct child returned `EPERM`. Running the
probe below a container init also preserved the intended orphan distinction:
an orphan reparented away from the supervisor failed, while the supervisor-as-
subreaper case passed.

## CI categories

GitHub Actions was tested directly. The other provider rows remain documented
expectations: this repository had no Azure Pipelines, GitLab, CircleCI, or
Buildkite configuration or available provider credentials, so launching those
jobs would have required new external accounts or secrets.

| CI category                                      | Expectation                                                | Operational guidance                                                                                                                                                                                            |
| ------------------------------------------------ | ---------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| GitHub Actions Ubuntu hosted job, directly on VM | **Yes, tested**                                            | UID 1001 with zero effective capabilities passed all descendant operations; the current `inject_demo` completed as that user and under `sudo`.                                                                  |
| GitHub Actions Docker and `container:` jobs      | **Yes, tested**                                            | Default Docker and an actual `ubuntu:24.04` job container passed as root and UID 65534 with seccomp mode 2 and no `SYS_PTRACE`. No container options were added.                                                |
| Azure Pipelines hosted Linux job, directly on VM | **Likely yes**                                             | Microsoft documents a fresh VM and passwordless sudo.                                                                                                                                                           |
| Azure Pipelines container job                    | **Likely on modern Docker**                                | The `container.options` field accepts startup options; exact hosted policy should be probed.                                                                                                                    |
| GitLab shell executor                            | **Host-dependent**                                         | Ordinary kernel/Yama rules; the runner operator controls the host.                                                                                                                                              |
| GitLab Docker executor                           | **Likely with modern Docker defaults, operator-dependent** | GitLab Runner exposes `cap_add`, `security_opt`, `userns_mode`, and `privileged` in runner configuration, generally controlled by the runner administrator rather than the project.                             |
| CircleCI machine executor                        | **Likely yes**                                             | CircleCI documents full OS control and `sysctl` access.                                                                                                                                                         |
| CircleCI Docker executor                         | **Unknown**                                                | CircleCI documents that privileged containers are unavailable in this executor. The descendant path may still work, as it does in default Docker, but a hardened denial may have no project-level escape hatch. |
| Self-hosted CI                                   | **Configuration-dependent**                                | Run the exact probe during runner qualification and after runtime/kernel upgrades.                                                                                                                              |

### Direct GitHub Actions results

The strict rerun is
[32653068642](https://github.com/voidzero-dev/vite-task/actions/runs/32653068642).
It repeated every placement below with the exit-enforcing probe and current
`syscall; int3` injector. It also proved that the full deny profile makes the
probe and injector exit 1 as root and UID 65534, while a targeted
`PR_SET_PTRACER` denial is reported without aborting unrelated required tests.
Exact hashes and output are in
[`ptrace-strict-rerun-results.md`](ptrace-strict-rerun-results.md).

The [successful run](https://github.com/voidzero-dev/vite-task/actions/runs/32651507168)
used commit `dff344416134bb3525ee5f6c0b3b375ba416871a` on the isolated
`research/ptrace-hosted-ci-20260824` branch. It tested three Linux placements:

1. the GitHub-hosted Ubuntu VM as UID 1001 and as root;
2. ordinary Docker containers started on that VM as root and UID 65534;
3. a GitHub Actions `container: ubuntu:24.04` job as root and UID 65534.

For the unprivileged VM user, Docker root, Docker UID 65534, job-container root,
and job-container UID 65534, the exact relationship pattern was identical:

| Probe                                      | Result  |
| ------------------------------------------ | ------- |
| `TRACEME` + exec + `GETREGSET`/`SETREGSET` | Pass    |
| `ATTACH` direct child                      | Pass    |
| `SEIZE` direct child                       | Pass    |
| `process_vm_readv` and `process_vm_writev` | Pass    |
| `SEIZE` live grandchild                    | Pass    |
| same-UID sibling                           | `EPERM` |
| sibling after `PR_SET_PTRACER`             | Pass    |
| non-dumpable direct child                  | `EPERM` |
| orphan reparented away                     | `EPERM` |
| orphan reparented to subreaper             | Pass    |

The VM root run additionally passed sibling, non-dumpable, and reparented-away
cases because it had a broad effective capability set. That privileged result
is not used to justify the ordinary fspy path.

The current x86-64 `inject_demo` then completed in all six identity/placement
combinations. Each run injected the 40,960-byte freestanding payload, installed
the in-process SIGSYS handler, reported `/bin/cat`'s `openat` calls including
`test_path`, printed `SIGSYS works`, and exited zero. Neither Docker path used
`--cap-add SYS_PTRACE` or `--security-opt seccomp=unconfined`; the Actions job
container supplied no custom options at all.

The setuid check also reproduced Linux's tracing semantics. An untraced helper
changed UID 1001 or 65534 to effective UID 0, while a `PTRACE_TRACEME` exec kept
the original effective UID.

### Direct WSL2 results on GitHub-hosted Windows

The first hosted run established that `windows-latest` had WSL 2.7.11, Linux
kernel package 6.18.33.2-2, default version 2, and no installed distribution.
A dedicated follow-up downloaded the pinned official Ubuntu 24.04.4 WSL image
and imported it with `wsl.exe --import ... --version 2`, without installing a
Windows feature or rebooting.

The [successful run](https://github.com/voidzero-dev/vite-task/actions/runs/32652094818)
then ran the full shared probe and current `inject_demo` inside the imported
distribution as UID 1001 with no effective capabilities. All operations
exercised by that probe passed; the expected Yama scope-1 sibling,
non-dumpable, and reparented-away cases returned `EPERM`. The injector
completed as both root and UID 1001. The final on-demand `SEIZE` plus
`PTRACE_EVENT_EXEC` handshake remains a separate validation item.

The later strict
[run 32653068642](https://github.com/voidzero-dev/vite-task/actions/runs/32653068642)
reimported the pinned distribution and repeated both identities with the
exit-enforcing probe, ptrace word I/O, and the current explicit-trap injector.
It passed and unregistered the distribution during cleanup.

The complete environment, output, workflow, cleanup, and three preliminary
harness failures are recorded in
[`ptrace-wsl2-results.md`](ptrace-wsl2-results.md). This is direct Linux process
execution under `microsoft-standard-WSL2`. It does not test Docker Desktop's
WSL2 backend or a nested container, which can add another security policy.

Official provider sources:

- [GitHub-hosted runner privileges](https://docs.github.com/en/actions/reference/runners/github-hosted-runners#administrative-privileges)
  and [job container options](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idcontaineroptions).
- [Azure hosted agents](https://learn.microsoft.com/en-us/azure/devops/pipelines/agents/hosted)
  and [container startup options](https://learn.microsoft.com/en-us/azure/devops/pipelines/process/container-phases#startup-options).
- [GitLab Docker executor](https://docs.gitlab.com/runner/executors/docker/)
  and [advanced Docker runner configuration](https://docs.gitlab.com/runner/configuration/advanced-configuration/).
- [CircleCI Docker executor](https://circleci.com/docs/using-docker/)
  and [machine executor](https://circleci.com/docs/using-linuxvm/).

## AI coding sandboxes

Published descriptions generally stop above the syscall-policy layer:

| Environment                  | Published isolation                                | Ptrace conclusion                                                                                                                                                                                                                                                                   |
| ---------------------------- | -------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| OpenAI Codex cloud           | A per-task container using the `universal` image   | **Yes with ptrace word I/O, tested directly with the strict probe and current injector.** UID 1000 with no capabilities passed the required summary and explicit-trap `inject_demo`; both process-VM syscalls returned `ENOSYS`, while peek/poke passed.                            |
| Local Codex on Linux/WSL2    | Bubblewrap/user-namespace based sandboxing         | **Potentially compatible.** A user namespace alone did not block the probe, but official docs do not promise ptrace availability or absence of an additional filter. `danger-full-access` removes Codex's local sandbox restrictions, but host/container policy still applies.      |
| Anthropic Claude Code cloud  | Per-session isolated Anthropic-managed environment | **Yes in the tested privileged posture.** The strict probe and current injector passed directly in the cloud task, but its ordinary task shell was UID 0 with `CAP_SYS_PTRACE`, no seccomp, and no Yama. This does not establish capability-free compatibility.                     |
| GitHub Copilot coding agent  | Ephemeral GitHub Actions development environment   | **Yes, tested directly with the strict probe and current injector.** UID 1001 with no effective capabilities passed ptrace word I/O, both process-VM syscalls, the required summary, and explicit-trap `inject_demo`.                                                               |
| A gVisor-based agent sandbox | Userspace-kernel syscall mediation                 | **Architecture-dependent on the tested runsc release.** Descendant ptrace primitives pass on ARM64 and x86-64. The current end-to-end injector passes on x86-64; on ARM64 it reaches the handler but loses syscall argument zero because runsc omits Linux's seccomp-trap rollback. |

Sources: [OpenAI Codex cloud environments](https://developers.openai.com/codex/cloud/environments),
[OpenAI Codex local sandboxing](https://developers.openai.com/codex/sandboxing),
[Claude Code cloud security and isolation](https://code.claude.com/docs/en/claude-code-on-the-web#security-and-isolation),
and [GitHub Copilot coding agent](https://docs.github.com/en/copilot/concepts/agents/coding-agent/about-coding-agent).

An AI provider saying “container”, “VM”, or “sandbox” is not enough to infer the
answer. fspy should expose a small diagnostic that prints the exact probe result
inside the real job.

### Direct AI sandbox results

The task links, environment captures, exact output excerpts, and reproduction
context are in
[`ai-sandbox-ptrace-results.md`](ai-sandbox-ptrace-results.md). The three direct
tests establish that coding-agent products can expose different privilege and syscall
surfaces even when ordinary descendant ptrace works:

- GitHub Copilot's coding-agent VM passed the strict relationship probe,
  ptrace word I/O, `process_vm_readv`/`process_vm_writev`, and the current
  explicit-trap injector as UID 1001 with no capabilities.
- OpenAI Codex Cloud passed the same strict probe, ptrace word I/O, and current
  injector as UID 1000 with no capabilities, but returned `ENOSYS` for both
  process-VM syscalls.
- Anthropic Claude Code cloud passed the strict probe, both process-VM
  operations, ptrace word I/O, and the current injector. Its task shell was
  root with `CAP_SYS_PTRACE`, however, so the direct result is less
  representative of an ordinary unprivileged fspy process than the Codex and
  Copilot results.

A later
[clean-target Codex confirmation](https://chatgpt.com/codex/tasks/task_e_6a8b2967ff00832fac6ff1dce29a1f5f)
verified the tested probe and supervisor hashes inside the task, used the pinned
Rust 1.99 nightly toolchain, and freshly compiled both payload and supervisor
in a new `/tmp` Cargo target directory. It reproduced Codex's payload entry
`0x36c4` and 142 relocations with payload SHA-256 `eaf2cf2b...80a5bf4`;
Copilot and hosted CI had printed `0x3550` and 141 from the same source commit.
This rules out a stale Codex Cargo target. The remaining cross-provider payload
build difference is unexplained. It does not weaken the ptrace result: the
verified supervisor source uses `PTRACE_CONT` plus a syscall and explicit trap,
and both payload variants completed the end-to-end test.

The Codex result means the injector should not make bulk process-VM transfer a
hard dependency. Its existing ptrace word-read/write mechanism is a proven
fallback in that environment.

## Design recommendations for fspy

### 1. Probe the exact production mechanism

At supervisor startup, use disposable children to test:

1. `PTRACE_SEIZE` with `PTRACE_O_TRACEEXEC | PTRACE_O_EXITKILL`;
2. `PTRACE_INTERRUPT`, wait, and detach;
3. `process_vm_writev` into a known child word, plus ptrace word I/O so the
   fallback is qualified independently;
4. a seized child's successful exec and `PTRACE_EVENT_EXEC`;
5. register read/write using the architecture-specific regset.

Do not use `Seccomp:` or the presence of `SYS_PTRACE` as a proxy. The tested
default had `Seccomp: 2`, no capability, and worked. Seccomp, Yama, common ptrace
access checks, and other LSMs all commonly collapse to `EPERM`.

The startup result is only a baseline. A later target can become non-dumpable,
change credentials, or enter a namespace that changes the access relationship.
The exec handshake must treat attach failure as a normal unsupported path, not
an invariant violation.

### 2. Have each execing target opt in under Yama scope 1

In the SIGSYS exec handler, immediately before notifying the supervisor, call:

```text
prctl(PR_SET_PTRACER, supervisor_pid)
```

This is target-local, narrowly authorizes one debugger PID and that debugger's
descendants, and fixes the observed reparenting failure under scope 1. It must
happen for each execing process; Yama's exception is attached to that tracee,
not a blanket inherited grant to future processes.

This does not bypass:

- Yama scope 2 or 3;
- a seccomp denial;
- `PR_SET_DUMPABLE=0`;
- mismatched credentials or another LSM denial.

Making the supervisor a child subreaper also passed, but changes process
adoption and wait/reaping semantics. Do not take that broader semantic change
only to obtain ptrace permission unless fspy already owns those semantics.

### 3. Keep supervisor and target in one container

The natural parent/descendant topology works with default PID isolation. A
sidecar needs shared PID namespaces, correct UID/user-namespace relationships,
and usually an explicit ptrace policy. It is a different deployment design.

PID namespaces also create an identification problem separate from permission:
the TID returned by `gettid()` in a nested namespace may not be the number the
outer supervisor must pass to ptrace. The descendant exec handshake must carry
or derive a supervisor-visible task identity. This research did not validate
that translation.

### 4. Use bulk transfer, but preserve a diagnostic fallback

Modern Docker and containerd defaults allow `process_vm_writev`, and it passed
locally. Use it for the payload when the startup probe succeeds. Codex Cloud
returned `ENOSYS` for the process-VM syscalls while `PTRACE_POKEDATA` and the
current injector passed, so ptrace word I/O is a required compatibility
fallback. It cannot help when policy denies `ptrace` itself.

### 5. Do not request broad privilege by default

Do not tell all Docker/Kubernetes/CI users to add `SYS_PTRACE` or run
unconfined. The common descendant case worked without either. When the probe
fails, report actionable diagnostics and let the deployment owner choose:

- a narrow seccomp profile allowing `ptrace` and `process_vm_writev`;
- `SYS_PTRACE` for Yama scope 2 or cross-relationship debugging;
- a non-ptrace fspy fallback;
- explicitly unsupported tracing.

Yama scope 3 cannot be repaired with a capability.

### 6. Preserve the detached steady state

Attach only for the exec transaction, inject at `PTRACE_EVENT_EXEC`, and detach
before target code runs. This avoids ptrace stops on ordinary signals and on the
frequent in-process `SIGSYS` path. Availability of ptrace does not require
paying steady-state ptrace overhead.

## Remaining validation

- Implement the actual handler -> `PR_SET_PTRACER` -> supervisor `SEIZE` ->
  reissued exec -> `PTRACE_EVENT_EXEC` -> detach protocol and rerun this matrix.
- Run the probe in Azure hosted VM/container jobs, GitLab.com shared runners,
  and CircleCI's Docker executor.
- Run on a real rootless Docker daemon. Rootless Podman and Docker
  `userns-remap` are now covered directly.
- Repeat the Kubernetes matrix on a managed cluster and on CRI-O. Local
  kind/containerd/runc is covered.
- Test under Kata Containers. For ARM64 runsc, validate an upstream
  syscall-rollback fix or select a different seccomp backend after a runtime
  probe. Current x86-64 runsc is covered directly.
- Run inside Docker Desktop's WSL2 backend or a container nested in WSL2. A
  distribution running directly under WSL2 is now covered.
- Run in additional AI sandboxes. For Claude cloud, repeat as a capability-free
  non-root user if that execution mode becomes available; the current direct
  task was privileged.
- Test Chromium namespace/user transitions specifically. Permission and
  supervisor-visible PID translation are both relevant.
- Add tests for a target that changes UID, calls `PR_SET_DUMPABLE=0`, and execs
  from a non-leader thread.

## Drifts and local side effects

- The prior research statement that the current `inject_demo` works in default
  Docker is confirmed, now as both root and UID 65534.
- Direct GitHub Actions evidence replaced the prior inference for its hosted
  Ubuntu VM, ordinary Docker, and `container:` job placements. The successful
  run is [32651507168](https://github.com/voidzero-dev/vite-task/actions/runs/32651507168).
- Fresh strict Codex Cloud and Copilot coding-agent results supersede the
  earlier partial captures. The current Codex task and closed Copilot
  [PR #697](https://github.com/voidzero-dev/vite-task/pull/697) both ran the
  explicit-trap injector; exact task/session links are in the AI-sandbox note.
  A third Codex task rebuilt in a new target directory and reproduced Codex's
  distinct payload metadata, ruling out stale local Cargo output without
  explaining the remaining cross-provider artifact difference.
  Copilot PRs #696 and #697 were closed and their ephemeral task branches were
  deleted after capture. Historical PR #695 remains clearly labeled as old.
- Claude Code cloud was tested directly at the same source commit. The strict
  probe and current explicit-trap injector exited 0 after installing only the
  missing `x86_64-unknown-none` Rust target. Unlike Codex and Copilot, Claude's
  task shell was UID 0 with `CAP_SYS_PTRACE`, so the result is recorded as a
  privileged-environment pass rather than capability-free evidence.
- Rootless Podman and Docker `userns-remap` direct tests replaced the prior
  user-namespace inference. Rootless Docker itself remains untested.
- Starting the isolated rootful `userns-remap` daemon in the Colima host
  network namespace removed the normal daemon's `docker0` link despite
  `--bridge=none`. After confirming no containers were running, Docker was
  restarted; the default bridge was recreated and an ordinary networked
  container passed. The restart and regenerated bridge ID are material local
  drift. The reproducer now requires a disposable VM or dedicated host network
  namespace for a second rootful daemon.
- The current demo still validates only the first `TRACEME` exec, not every
  descendant exec through on-demand `SEIZE`.
- The experiment changed `inject_demo` from `PTRACE_SINGLESTEP` to an explicit
  syscall-plus-trap patch resumed with `PTRACE_CONT`. Research probes, profiles,
  manifests, and reports were added under `research/`.
- The C probe now checks two pipe writes whose ignored results were rejected by
  the hosted Ubuntu compiler's fortified headers under `-Werror`. This does not
  change the tested behavior.
- The C probe now reports a failed `PR_SET_PTRACER` subtest instead of
  aborting. Codex Cloud exposed this because its kernel has no Yama control file
  and rejected that Yama-specific opt-in, while ordinary descendant ptrace and
  the real injector still worked.
- The C probe now exercises `PTRACE_PEEKDATA`/`PTRACE_POKEDATA` and exits
  nonzero when a required positive descendant operation fails. Fresh hosted,
  WSL2, Kubernetes, Codex Cloud, and Copilot runs used this strict probe and
  the current explicit-trap injector. Older captures remain labeled as
  historical evidence only.
- The broad hosted, WSL2, Kubernetes, and AI strict captures used probe SHA-256
  `abbb06bc…14c7`. A post-capture review hardened all wait-status checks, makes
  successful `PTRACE_SEIZE` tests interrupt and stop their tracees before detach, and
  keeps cleanup reaping through intermediate ptrace stops. The repository copy
  is now `da027737…34277a`. It compiled for Linux with `-Werror` and locally
  preserved default root/non-root exit 0, full-deny exit 1, and
  targeted-`PR_SET_PTRACER`-deny exit 0, including the setuid suite as UID 65534. The x86-64 gVisor matrix was freshly rerun with this exact current
  source; the other external environments were not.
- Existing uncommitted `Cargo.lock`, `crates/fspy_loader/`, and other research
  work were left intact.
- The existing Colima default profile was started and left running. Its Yama
  setting was restored to 1 after the tests.
- Local Docker images for the environment probe, exec-primitives probe,
  seccomp-trap reproduction, and injector were created and retained.
- The disposable kind cluster, runsc runtimes, Podman installation, experiment
  containers, and their temporary images/configuration were removed. Ignored
  mise installs for kind 0.32.0 and kubectl 1.36.4 remain, as do refreshed VM
  APT indexes and Cargo target-cache outputs.
- Installing gVisor prerequisites upgraded `ca-certificates`, `curl`, and
  related `libcurl` packages in the Colima VM; those security updates were not
  downgraded. The runsc package and repository were removed.
- The remote experimental branch `research/ptrace-hosted-ci-20260824` remains
  available for reproduction. It created three GitHub Actions runs: the first
  [failed at setup](https://github.com/voidzero-dev/vite-task/actions/runs/32651440711)
  because the temporary workflow initially used unpinned actions; the second
  [reached compilation](https://github.com/voidzero-dev/vite-task/actions/runs/32651474520)
  and exposed the unchecked-write warnings; the third passed. Its uploaded
  binaries have a one-day retention, and all hosted VMs and containers were
  ephemeral.
- The strict hosted/WSL2 and x86-64 runsc reruns remain on remote experimental
  branches `research/ptrace-strict-hosted-wsl-20260824` and
  `research/ptrace-gvisor-x86-20260824`. Their successful runs are
  [32653068642](https://github.com/voidzero-dev/vite-task/actions/runs/32653068642)
  and [32654193527](https://github.com/voidzero-dev/vite-task/actions/runs/32654193527).
  The hosted VMs were ephemeral; the WSL distro was unregistered, and the
  gVisor workflow verified the runsc binary, shim, and Docker registration were
  removed.
- The Windows hosted runner was inspected without installation or reboot. WSL2
  components were present but no distribution was preinstalled. A later
  dedicated workflow imported a pinned Ubuntu image without reboot, directly
  validated WSL2, and unregistered it during cleanup.
- The WSL2 reproducer remains on remote experimental branch
  `research/ptrace-wsl2-20260824`. Four runs were created: three exposed
  PowerShell/checksum/output-encoding harness issues, and the fourth passed.
  Their exact links and causes are in the WSL2 note; uploaded artifacts expire
  after one day.
- Azure, GitLab, CircleCI, Buildkite, Docker Desktop in WSL2, and managed
  Kubernetes remained untested because they lacked an existing direct
  execution path or required new provider access.
- The Codex Cloud CLI generated a repository-root `error.log` while retrieving
  task results; it was removed. No new provider account, key, or paid resource
  was created.
