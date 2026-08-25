# Descendant ptrace under Kubernetes

Directly tested and rerun with the hardened probe on 2026-08-24. This note
covers a local Kubernetes cluster on the normal Linux container runtime. It is
not evidence for gVisor, Kata, or a managed cluster's additional admission and
node policies.

## Result

Kubernetes itself did not prevent descendant ptrace. The primitive probe and
the current `inject_demo` both worked with:

- no seccomp profile requested;
- `seccompProfile.type: RuntimeDefault`;
- an enforced Restricted Pod Security namespace, running as UID/GID 65534,
  with all capabilities dropped and privilege escalation disabled.

The Restricted case had `CapEff=0`, `NoNewPrivs=1`, `Seccomp=2`, and
`kernel.yama.ptrace_scope=1`. It still allowed `PTRACE_TRACEME`,
`PTRACE_ATTACH`, `PTRACE_SEIZE`, `PTRACE_GETREGSET`, `PTRACE_SETREGSET`,
`PTRACE_INTERRUPT`, `PTRACE_PEEKDATA`, `PTRACE_POKEDATA`, `PTRACE_DETACH`, and
`process_vm_{readv,writev}` for the tested descendant relationships.

A kubelet `Localhost` profile that explicitly returned `EPERM` for `ptrace`,
`process_vm_readv`, and `process_vm_writev` blocked every such operation. The
real demo then failed while spawning `/bin/cat` with `os error 1`.

`shareProcessNamespace: true` made processes in two containers visible to one
another, but did not make the tracer an ancestor of the target. With Yama scope
1 and no capabilities:

- same UID without target opt-in: `PTRACE_SEIZE` failed with `EPERM`;
- same UID after target `PR_SET_PTRACER(tracer_pid)`: seize, interrupt, wait,
  and detach succeeded;
- different UIDs after `PR_SET_PTRACER`: seize still failed with `EPERM`.

This sidecar result does not imply that a cross-container supervisor is a good
deployment design. It isolates PID visibility, ancestry, and credentials. The
normal in-container supervisor/descendant topology does not need a shared PID
namespace.

## Test environment

| Component         | Observed value                            |
| ----------------- | ----------------------------------------- |
| Host VM           | Colima 0.10.3, arm64, Docker runtime      |
| Linux guest       | Ubuntu 24.04.4, Linux `6.8.0-100-generic` |
| Docker            | client 29.7.1, server 29.2.1              |
| Cluster           | kind 0.32.0, `kindest/node:v1.36.1`       |
| Kubernetes        | client 1.36.4, server 1.36.1              |
| Node userspace    | Debian 13 (`trixie`)                      |
| Pod runtime       | containerd 2.3.1, runc 1.4.2              |
| Node Yama         | `kernel.yama.ptrace_scope=1`              |
| Node architecture | AArch64                                   |
| Node AppArmor     | enabled                                   |

The kubelet did not have `seccompDefault` enabled. Consequently, the pod that
omitted `seccompProfile` reported `Seccomp: 0`. This is an observation about
this cluster, not a general promise about omitted Kubernetes profiles.

The root pods' `CapEff=00000000a80425fb` did not contain `CAP_SYS_PTRACE`. The
Restricted pods had no effective capability at all.

The rerun used repository HEAD `66ae8432311a4a25138a8e9077dec8a57d5403c4`
plus the current working-tree explicit-trap change in `inject_demo`. These
checksums identify the exact probe and injector binary that entered the kind
node:

| Input                             | SHA-256 / image ID                                                        |
| --------------------------------- | ------------------------------------------------------------------------- |
| `ptrace-environment-probe.c`      | `abbb06bc7fafd0dcaaceabf1dfd23d300613e2fbf03cf49c4ba7ab4d1ec814c7`        |
| AArch64 `inject_demo` binary      | `55f577baf840b288996edac5ab794801cb7fee513d37c505a5b6bc3ca74a7d33`        |
| `fspy-ptrace-probe:k8s-current`   | `sha256:d286fb988206dc68afc12f4764c8b3c927c4963d91e5cf9752490b034fe08759` |
| `fspy-inject-demo:k8s-current`    | `sha256:4730cb026a582da4dda55a2f0c7dd06acda28733a59fb38b515a13e7e89e67e5` |
| `fspy-ptrace-sidecar:k8s-current` | `sha256:84ec426e6eb1b2fb44c53191cad817f715a25b640e3f9827def93c40fa61aa3a` |

The namespace used for the hardened cases was admitted with:

```text
pod-security.kubernetes.io/enforce=restricted
pod-security.kubernetes.io/enforce-version=latest
```

## Primitive matrix

The probe ran below a shell that remained PID 1. This prevents the probe from
adopting its own orphan and keeps the orphan/subreaper comparison meaningful.

| Operation                                 | Profile omitted | RuntimeDefault | Restricted + RuntimeDefault | Restricted + Localhost deny                |
| ----------------------------------------- | --------------- | -------------- | --------------------------- | ------------------------------------------ |
| `TRACEME` + exec + GET/SETREGSET          | Pass            | Pass           | Pass                        | `EPERM` at `TRACEME`                       |
| attach direct child                       | Pass            | Pass           | Pass                        | `EPERM`                                    |
| seize direct child                        | Pass            | Pass           | Pass                        | `EPERM`                                    |
| `process_vm_writev` / `readv`             | Pass / pass     | Pass / pass    | Pass / pass                 | `EPERM` / `EPERM`                          |
| `PTRACE_PEEKDATA` / `POKEDATA`            | Pass            | Pass           | Pass                        | Not reached because seize returned `EPERM` |
| seize live grandchild                     | Pass            | Pass           | Pass                        | `EPERM`                                    |
| seize same-UID sibling                    | `EPERM`         | `EPERM`        | `EPERM`                     | `EPERM`                                    |
| sibling after `PR_SET_PTRACER`            | Pass            | Pass           | Pass                        | `EPERM`                                    |
| seize non-dumpable child                  | `EPERM`         | `EPERM`        | `EPERM`                     | `EPERM`                                    |
| seize orphan reparented away              | `EPERM`         | `EPERM`        | `EPERM`                     | `EPERM`                                    |
| seize orphan with supervisor as subreaper | Pass            | Pass           | Pass                        | `EPERM`                                    |

The negative sibling, non-dumpable, and orphan cases are expected Yama/core
ptrace access-control results. They show that `Seccomp: 2` is not itself proof
that seccomp blocked ptrace.

The Restricted process reported:

```text
pid=13 uid=65534 euid=65534
CapEff: 0000000000000000
NoNewPrivs: 1
Seccomp: 2
YamaScope: 1
traceme+exec+regset          result=PASS errno=0 (none) SIGTRAP exec-stop
seize-direct-child           result=PASS errno=0 (none)
process-vm-write             result=PASS errno=0 (none) direct child
process-vm-read              result=PASS errno=0 (none) direct child
ptrace-word-io               result=PASS errno=0 (none) stopped direct child
seize-live-grandchild        result=PASS errno=0 (none) ancestor, not direct parent
seize-sibling                result=FAIL errno=1 (Operation not permitted) same UID
seize-sibling-pr-set-ptracer result=PASS errno=0 (none) target opted in
seize-orphan-no-subreaper    result=FAIL errno=1 (Operation not permitted) reparented away
seize-orphan-subreaper       result=PASS errno=0 (none) reparented to supervisor
required-summary             result=PASS errno=0 (none)
```

Under the Localhost deny profile, every ptrace and process-VM attempt returned
errno 1. `PR_SET_PTRACER` could not bypass the seccomp rule. The hardened probe
reported `required-summary result=FAIL`, exited 1, and Kubernetes recorded the
pod as `Failed` with reason `Error`. That terminal failure is expected and is
the strict negative control, not an infrastructure failure.

## Real injection demo

The current AArch64 `fspy-inject-demo:k8s-current` image completed with exit
code 0 under all three profiles that did not explicitly deny ptrace. This build
uses `PTRACE_CONT` with an injected `svc #0; brk #0` pair for remote `mmap`;
the Kubernetes rerun therefore covers the post-gVisor explicit-trap design, not
the earlier `PTRACE_SINGLESTEP` implementation.

| Pod                      | Identity and policy                                              | Result                              |
| ------------------------ | ---------------------------------------------------------------- | ----------------------------------- |
| `inject-omitted`         | root, profile omitted                                            | `Succeeded`, exit 0                 |
| `inject-runtime-default` | root, RuntimeDefault                                             | `Succeeded`, exit 0                 |
| `inject-restricted`      | UID/GID 65534, drop ALL, no privilege escalation, RuntimeDefault | `Succeeded`, exit 0                 |
| `inject-localhost-deny`  | same Restricted settings, Localhost deny                         | `Failed` / `Error`, exit 1, `EPERM` |

A successful run mapped the Rust payload, detached, installed the in-process
SIGSYS handler, reported all three expected `openat` paths, and exited zero:

```text
payload: 163840 bytes, entry +0x13bfc, 50 relocations
mapped 164160 bytes into the target
detached — payload will restore the exec context in-process
fspy_preload_linux: installed SIGSYS handler
openat: /etc/ld.so.cache
openat: /lib/aarch64-linux-gnu/libc.so.6
openat: test_path
SIGSYS works
/bin/cat exited with code 0
```

The deny run failed before exec:

```text
Error: spawn /bin/cat

Caused by:
    Operation not permitted (os error 1)
```

As in the Docker experiment, this demo uses `PTRACE_TRACEME` for the first
exec. It does not yet prove the proposed detached `SEIZE`/exec handshake. The
primitive probe proves that the required descendant seize permission exists
under the tested pod policies.

## Shared-process-namespace experiment

The exact pod manifest is
[`ptrace-kubernetes-sidecar.yaml`](ptrace-kubernetes-sidecar.yaml). Its small
probe and image recipe are
[`ptrace-kubernetes-sidecar-probe.c`](ptrace-kubernetes-sidecar-probe.c) and
[`ptrace-kubernetes-sidecar.Dockerfile`](ptrace-kubernetes-sidecar.Dockerfile).
All three pods ran in the enforced Restricted namespace with RuntimeDefault,
no capabilities, and no privilege escalation.

Same UID, no opt-in:

```text
target pid=7 ppid=0 uid=65534 euid=65534 tracer=20 opt_in=no pr_set_ptracer=not-called rc=0 errno=0 (none)
tracer pid=20 ppid=0 uid=65534 euid=65534 target=7 target_uid=65534 tracer_is_ancestor=no seize_rc=-1 errno=1 (Operation not permitted) completed=no expected=EPERM
```

Same UID, target opt-in:

```text
target pid=7 ppid=0 uid=65534 euid=65534 tracer=20 opt_in=yes pr_set_ptracer=success rc=0 errno=0 (none)
tracer pid=20 ppid=0 uid=65534 euid=65534 target=7 target_uid=65534 tracer_is_ancestor=no seize_rc=0 errno=0 (none) completed=yes expected=success
```

Different UIDs, target opt-in:

```text
target pid=7 ppid=0 uid=65534 euid=65534 tracer=20 opt_in=yes pr_set_ptracer=success rc=0 errno=0 (none)
tracer pid=20 ppid=0 uid=65533 euid=65533 target=7 target_uid=65534 tracer_is_ancestor=no seize_rc=-1 errno=1 (Operation not permitted) completed=no expected=EPERM
```

`PPid: 0` here is the observed top-level-container-process representation in
the shared pod PID namespace. The explicit ancestry walk also found that the
tracer was not an ancestor of the target.

## Reproduction

The full probe/injector matrix is retained in
[`ptrace-kubernetes-pods.yaml`](ptrace-kubernetes-pods.yaml). It includes both
namespaces, all eight pods, their exact commands and security contexts, and the
expected terminal phase and exit code for each strict-deny case. The separate
[`ptrace-kubernetes-sidecar.yaml`](ptrace-kubernetes-sidecar.yaml) contains all
three shared-process-namespace cases.

Build the current images. The complete probe Dockerfile is also documented in
[`ptrace-environment-compatibility.md`](ptrace-environment-compatibility.md).

```bash
docker build -t fspy-ptrace-probe:k8s-current -f- research <<'DOCKERFILE'
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

cargo zigbuild -p inject_demo --target aarch64-unknown-linux-gnu
target_dir="$(cargo metadata --format-version 1 --no-deps \
  | jq -r .target_directory)/aarch64-unknown-linux-gnu/debug"
docker build -t fspy-inject-demo:k8s-current -f- "$target_dir" <<'DOCKERFILE'
FROM debian:bookworm-slim
COPY inject_demo /inject_demo
ENTRYPOINT ["/inject_demo"]
DOCKERFILE

docker build -t fspy-ptrace-sidecar:k8s-current \
  -f research/ptrace-kubernetes-sidecar.Dockerfile research
```

The Kubernetes tools were installed through the ignored local mise config.
Create the cluster and load all three images:

```bash
mise install kind@0.32.0 kubectl@1.36.4
kind create cluster \
  --name fspy-ptrace-strict \
  --kubeconfig /tmp/fspy-ptrace-strict-kubeconfig \
  --image kindest/node:v1.36.1 \
  --wait 5m
kind load docker-image --name fspy-ptrace-strict \
  fspy-ptrace-probe:k8s-current \
  fspy-inject-demo:k8s-current \
  fspy-ptrace-sidecar:k8s-current
export KUBECONFIG=/tmp/fspy-ptrace-strict-kubeconfig
```

Install the deny profile in the kind node's kubelet seccomp directory, then
apply the retained manifests:

```bash
docker exec fspy-ptrace-strict-control-plane \
  mkdir -p /var/lib/kubelet/seccomp/profiles
docker cp research/ptrace-deny-seccomp.json \
  fspy-ptrace-strict-control-plane:/var/lib/kubelet/seccomp/profiles/ptrace-deny.json
kubectl apply -f research/ptrace-kubernetes-pods.yaml
kubectl apply -f research/ptrace-kubernetes-sidecar.yaml
```

The manifest deliberately runs `/probe-normal` below a shell that remains PID

1. If the image entrypoint were used directly, the probe would become PID 1
   and adopt its own orphan, invalidating the no-subreaper negative case.

Wait for the declared terminal phases. The two Localhost-deny pods must reach
`Failed`; waiting for `Succeeded` would incorrectly treat the negative control
as broken infrastructure.

```bash
for pod in ptrace-probe-omitted ptrace-probe-runtime-default \
  inject-omitted inject-runtime-default; do
  kubectl wait -n fspy-ptrace \
    --for=jsonpath='{.status.phase}'=Succeeded "pod/$pod" --timeout=120s
done

for pod in ptrace-probe-restricted inject-restricted \
  ptrace-sidecar-no-opt-in ptrace-sidecar-opt-in \
  ptrace-sidecar-different-uid; do
  kubectl wait -n fspy-ptrace-restricted \
    --for=jsonpath='{.status.phase}'=Succeeded "pod/$pod" --timeout=120s
done

for pod in ptrace-probe-localhost-deny inject-localhost-deny; do
  kubectl wait -n fspy-ptrace-restricted \
    --for=jsonpath='{.status.phase}'=Failed "pod/$pod" --timeout=120s
done
```

In the captured rerun, every positive container exited 0 with reason
`Completed`. `ptrace-probe-localhost-deny` and `inject-localhost-deny` each
exited 1 with reason `Error`, exactly as annotated in the manifest. Inspect
both logs: phase and exit status prove process behavior, while the individual
log lines identify whether an expected core/Yama failure or seccomp caused it.

## Limits

- This is one local kind/containerd/runc cluster and one RuntimeDefault
  implementation. A managed provider can add another seccomp profile, LSM
  policy, Yama setting, user namespace, or admission rule.
- The node is a Docker container inside a Colima VM. The workload results are
  direct kernel/runtime observations, but do not exercise a cloud-provider
  host boundary.
- No `SYS_PTRACE` fallback was tested or needed. Restricted Pod Security would
  reject adding that capability.
- No cross-pod, hostPID, privileged, gVisor, Kata, or setuid workload is covered
  here.
- `shareProcessNamespace` only establishes visibility. It does not relax core,
  Yama, seccomp, LSM, dumpability, or credential checks.

## Cleanup and residual state

The `fspy-ptrace-strict` kind cluster was deleted after the results were
captured. Its pods, node container, kubelet Localhost profile, temporary
kubeconfig, `kindest/node:v1.36.1` image, and the three experiment-created
`k8s-current` image tags were removed. Colima was already running before the
experiment and remains running.

The ignored `mise.local.toml` now selects `kind = "0.32.0"` and
`kubectl = "1.36.4"`; those two tool installations remain in mise's local
cache. Cross-building refreshed Cargo artifacts under the shared target cache,
and pulling/building refreshed Docker's Debian base and build layers. The
pre-existing `fspy-ptrace-probe:latest` and `fspy-inject-demo:latest` images
were left unchanged.
