# Descendant ptrace in rootless and user-namespace containers

Tested directly on 2026-08-24. Both rootless Podman and Docker with
`userns-remap` allowed the descendant-only ptrace design without
`CAP_SYS_PTRACE`, `--privileged`, `seccomp=unconfined`, or an AppArmor override.
The then-current `inject_demo` also completed in every tested identity.

## Test host

- Colima VM, ARM64, Ubuntu 24.04.4 LTS
- Linux `6.8.0-100-generic`
- Yama `ptrace_scope=1`
- VM user: UID 501, `CapEff=0`, `Seccomp=0`
- cgroup v2

The tests used:

- [`ptrace-environment-probe.c`](ptrace-environment-probe.c), statically linked
  for ARM64
- the ARM64 `inject_demo` source at commit
  `66ae8432311a4a25138a8e9077dec8a57d5403c4`, whose
  `crates/inject_demo/src/main.rs` SHA-256 was
  `0df86ffbd8a8b8107889be05796ee28ad9fc6ab04ba865cdea5914e8a1014de2`

That snapshot used `PTRACE_SINGLESTEP` for remote `mmap`; this experiment
predates the later `PTRACE_CONT` plus `svc; brk` change. It directly validates
the rootless/user-namespace permission and injection path, but the explicit-
trap implementation has not been rerun in these two topologies.

The test did not record the built injector binary's hash, so the source
identity above is the strongest retained injector identity. The probe was an
earlier untracked revision; neither its source nor binary hash was captured,
and the repository copy has since gained stricter exit and cleanup checks.
Consequently this evidence can reproduce the injector source and container
topology, but not every byte of the original probe. The primary runs used the
runtimes' default security profiles. `--network=none` only removed an unrelated
source of variability.

| Runtime               | Container identity | `CapEff`           | Seccomp | Required probe operations | `inject_demo` |
| --------------------- | ------------------ | ------------------ | ------- | ------------------------- | ------------- |
| Rootless Podman       | root               | `00000000800405fb` | 2       | Pass                      | Pass          |
| Rootless Podman       | UID 65534          | `0`                | 2       | Pass                      | Pass          |
| Docker `userns-remap` | root               | `00000000a80425fb` | 2       | Pass                      | Pass          |
| Docker `userns-remap` | UID 65534          | `0`                | 2       | Pass                      | Pass          |

“Required probe operations” excludes deliberately negative policy checks such
as same-UID sibling attach without opt-in and attach to a non-dumpable child.

## Rootless Podman

Podman 4.9.3 ran as the unprivileged VM user, using runc 1.3.4 and the stock
`/usr/share/containers/seccomp.json`. There was no systemd user session, so
Podman fell back from the systemd cgroup manager to `cgroupfs`.

Its user namespace was:

```text
uid_map
         0        501          1
         1     524288 1073741824
gid_map
         0       1000          1
         1     524288 1073741824
```

The default container-root identity reported:

```text
uid=0 euid=0
CapEff:      00000000800405fb
NoNewPrivs:  0
Seccomp:     2
LSM:         podman (unconfined)
```

That capability set does not contain `CAP_SYS_PTRACE`. A second run used
`--user 65534:65534` and reported `CapEff=0`, with the same seccomp mode and
Yama setting.

Both identities passed:

- `PTRACE_TRACEME`, exec-stop, `PTRACE_GETREGSET`, and `PTRACE_SETREGSET`
- `PTRACE_ATTACH` to a direct child
- `PTRACE_SEIZE` and `PTRACE_INTERRUPT` on a direct child
- `process_vm_readv` and `process_vm_writev` on a direct child
- seizing a live descendant that was not the direct child
- sibling opt-in through `PR_SET_PTRACER`
- the complete tested `inject_demo` snapshot, including injected `mmap`, payload
  writing, detach, in-process SIGSYS setup, and trapped `openat`

The expected restrictions remained intact. A same-UID sibling without
`PR_SET_PTRACER` and a direct child with `PR_SET_DUMPABLE=0` both failed with
`EPERM`.

## Docker `userns-remap`

This was an isolated Docker 29.2.1 daemon, not the VM's normal Docker daemon.
It used separate data, exec, PID, and socket paths; `vfs`; no bridge, iptables,
IP forwarding, or container networking; and `--userns-remap=wangchi`. The
daemon was rootful, but every container process was mapped into the subordinate
ID range:

```text
uid_map
         0     524288 1073741824
gid_map
         0     524288 1073741824
```

The daemon reported the built-in seccomp profile, `userns`, cgroup namespaces,
and AppArmor. Its container-root process reported:

```text
uid=0 euid=0
CapEff:      00000000a80425fb
NoNewPrivs:  0
Seccomp:     2
LSM:         docker-default (enforce)
```

That capability set also omits `CAP_SYS_PTRACE`. The UID 65534 run had no
effective capabilities. Both identities produced the same positive and
negative probe results as rootless Podman, and both completed `inject_demo`.

This proves Docker's default seccomp and AppArmor policies do not prevent the
required operations merely because the tracee and tracer live in a remapped
user namespace. It does not prove compatibility with a deployment's custom
seccomp, AppArmor, or SELinux policy.

## PID 1 changes the orphan test

Without `--init`, the probe itself was PID 1 in the container PID namespace.
When its middle process exited, the kernel reparented the target back to the
probe. `seize-orphan-no-subreaper` therefore passed: despite its historical
label, the probe was once again the target's ancestor.

The test was repeated with `--init` under both runtimes. The probe then ran
below the container init process, and the intended distinction appeared:

```text
seize-orphan-no-subreaper  result=FAIL errno=1 (Operation not permitted)
seize-orphan-subreaper     result=PASS errno=0
```

This is normal PID-namespace reparenting, not a relaxation of Yama. The fspy
supervisor should remain the tracee's ancestor or become a child subreaper when
its process topology can orphan tracees.

## Reproduction

Build the captured single-step demo from a detached worktree, not from the
current branch, which now contains the explicit-trap implementation. Compiling
the current probe below reruns a stricter superset; it does not recreate the
uncaptured probe revision byte for byte. The probe is static so it can run in
either test image:

```bash
staging=/tmp/fspy-ptrace-test
mkdir -p "$staging"
gcc -O2 -Wall -Wextra -Werror -static \
  research/ptrace-environment-probe.c \
  -o "$staging/ptrace-environment-probe"
git worktree add --detach /tmp/fspy-ptrace-single-step \
  66ae8432311a4a25138a8e9077dec8a57d5403c4
CARGO_TARGET_DIR=/tmp/fspy-ptrace-cargo cargo zigbuild \
  --manifest-path /tmp/fspy-ptrace-single-step/Cargo.toml \
  --locked -p inject_demo --target aarch64-unknown-linux-gnu
cp /tmp/fspy-ptrace-cargo/aarch64-unknown-linux-gnu/debug/inject_demo \
  "$staging/inject_demo"
git worktree remove /tmp/fspy-ptrace-single-step
```

Representative rootless Podman commands:

```bash
podman run --rm --network=none \
  -v "$staging:/work:ro" debian:bookworm-slim \
  /work/ptrace-environment-probe

podman run --rm --network=none --user 65534:65534 --workdir /tmp \
  -v "$staging:/work:ro" debian:bookworm-slim \
  /work/inject_demo

podman run --rm --init --network=none --user 65534:65534 \
  -v "$staging:/work:ro" debian:bookworm-slim \
  /work/ptrace-environment-probe
```

For the isolated Docker daemon, use
[`ptrace-docker-userns-daemon.json`](ptrace-docker-userns-daemon.json) to turn
off the containerd snapshotter, which Docker rejects in combination with user
namespace remapping:

```bash
sudo dockerd \
  --config-file "$PWD/research/ptrace-docker-userns-daemon.json" \
  --host unix:///tmp/fspy-docker-userns/docker.sock \
  --data-root /tmp/fspy-docker-userns/data \
  --exec-root /tmp/fspy-docker-userns/exec \
  --pidfile /tmp/fspy-docker-userns/docker.pid \
  --userns-remap="$USER" \
  --bridge=none --iptables=false --ip6tables=false --ip-forward=false \
  --storage-driver=vfs
```

Point a second shell at that socket and use the same run commands:

```bash
export DOCKER_HOST=unix:///tmp/fspy-docker-userns/docker.sock
docker run --rm --network=none \
  -v "$staging:/work:ro" debian:bookworm-slim \
  /work/ptrace-environment-probe
docker run --rm --network=none --user 65534:65534 --workdir /tmp \
  -v "$staging:/work:ro" debian:bookworm-slim \
  /work/inject_demo
```

Run the second daemon only in a disposable VM or a dedicated host network
namespace. In this experiment, starting it alongside the normal daemon removed
the normal daemon's `docker0` link even though the second daemon used
`--bridge=none`. Separate data and exec roots are not sufficient isolation.

## Limits and environment drift

- Rootless Docker itself was not tested. Rootless Podman covers the rootless
  runtime case; Docker `userns-remap` separately covers Docker's remapped user
  namespace and default AppArmor behavior.
- Setuid execution and cross-container/sibling attachment are outside the
  descendant-only design and were not treated as required success cases.
- Podman and its 16 dependency packages were temporarily installed in the VM,
  then purged. Its images, containers, user store, configuration, runtime data,
  and downloaded package archives were removed.
- The isolated Docker daemon and its automatically selected containerd
  namespace (`moby-524288.524288`) were removed. Its temporary data, exec,
  socket, and staging paths were deleted. Its containers never used a host
  bridge, but this rootful daemon did share the VM's network namespace.
- A later cleanup audit found that the normal daemon's network database still
  referred to `docker0`, while the link itself was absent. The normal daemon
  last attached a container successfully at 00:27. `systemd-networkd` then
  recorded `docker0: Link DOWN` at 00:33:28, the exact second that the isolated
  rootful daemon started. This strongly attributes the deletion to the second
  daemon despite its `--bridge=none` setting. Rootless Podman lacked
  `CAP_NET_ADMIN` in the host user namespace and could not have deleted the
  host link.
- No containers were running, so Docker was restarted at 00:39:33. This
  recreated `docker0` at `172.17.0.1/16` and replaced the default bridge
  network ID. The four pre-existing exited containers remained. An ordinary
  default-network Alpine container then received `172.17.0.2`, installed its
  default route, and successfully pinged the bridge gateway. This service
  restart and regenerated bridge identity are material environment drift.
- `apt-get update` refreshed package-index metadata in the VM. That cache was
  not rolled back.
- Cross-building refreshed files in Cargo's shared target cache; no product
  source was changed.
- One preliminary command accidentally started a local macOS `sudo dockerd`
  process waiting for authentication. It was killed before authentication and
  made no privileged or filesystem change.
