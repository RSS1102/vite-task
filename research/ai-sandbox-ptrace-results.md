# Descendant ptrace in AI coding sandboxes

Date: 2026-08-24

Audience: fspy maintainers verifying whether the ptrace injector can run in
hosted AI coding sandboxes.

## Result

The tested OpenAI Codex Cloud and GitHub Copilot coding-agent environments both
let an unprivileged, capability-free process trace and inject into its own
descendant. Anthropic Claude Code cloud also passed, but its task shell was
root and had `CAP_SYS_PTRACE`, so that result establishes availability only in
Claude's current privileged task posture. The hardened environment probe and
the current explicit-trap `inject_demo` exited zero in all three environments.

| Environment          | Identity                        | Seccomp  | Yama        | Required probe | Memory transfer                                 | Current injector |
| -------------------- | ------------------------------- | -------- | ----------- | -------------- | ----------------------------------------------- | ---------------- |
| Codex Cloud          | UID 1000, `CapEff=0`            | disabled | unavailable | pass, exit 0   | `process_vm_*`: `ENOSYS`; ptrace word I/O: pass | pass, exit 0     |
| Claude Code cloud    | UID 0, `CAP_SYS_PTRACE` present | disabled | unavailable | pass, exit 0   | `process_vm_*`: pass; ptrace word I/O: pass     | pass, exit 0     |
| Copilot coding agent | UID 1001, `CapEff=0`            | disabled | scope 1     | pass, exit 0   | `process_vm_*`: pass; ptrace word I/O: pass     | pass, exit 0     |

This is direct execution evidence from three provider environments. It is not a
provider guarantee, and hosted-agent policy can change.

## Source revision

The strict reruns used remote branch
`research/ptrace-strict-hosted-wsl-20260824` at commit
`83c9036182a8c95bed3264c20d902318fee67043`.

- [`ptrace-environment-probe.c` at the tested commit](https://github.com/voidzero-dev/vite-task/blob/83c9036182a8c95bed3264c20d902318fee67043/research/ptrace-environment-probe.c), SHA-256
  `abbb06bc7fafd0dcaaceabf1dfd23d300613e2fbf03cf49c4ba7ab4d1ec814c7`
- [`inject_demo` at the tested commit](https://github.com/voidzero-dev/vite-task/blob/83c9036182a8c95bed3264c20d902318fee67043/crates/inject_demo/src/main.rs), SHA-256
  `6dbc5c7b2668f047c21c9395cb1d62db3e25af560d6ced671a349655e65ed4fb`

Those hashes were first verified independently from the retained remote commit.
The later clean-target Codex task also calculated them inside its task
environment and produced the same values.

The probe includes a strict `required-summary`, returns nonzero when a required
operation fails, and separately checks both `process_vm_*` and
`PTRACE_PEEKDATA`/`PTRACE_POKEDATA`. The injector uses `ptrace::cont` after
patching a syscall plus an explicit trap. It does not use the older
`PTRACE_SINGLESTEP` flow.

## OpenAI Codex Cloud

The authoritative rerun is
[Codex task `task_e_6a8b27181900832f9e969906cfb85721`](https://chatgpt.com/codex/tasks/task_e_6a8b27181900832f9e969906cfb85721).
The task started with a root shell, then used `setpriv` to run the probe and
injector as the ordinary `ubuntu` user.

### Environment

```text
Linux 465b35beafe8 6.18.35 #1 SMP Mon Jul 27 18:07:50 UTC 2026 x86_64 GNU/Linux
Ubuntu 24.04.4 LTS
uid=1000(ubuntu) gid=1000(ubuntu) groups=1000(ubuntu)
CapEff:          0000000000000000
NoNewPrivs:      0
Seccomp:         0
Seccomp_filters: 0
Yama ptrace_scope: unavailable (No such file or directory)
/proc/self/cgroup: 0::/
/proc/1/cgroup:    0::/
/.dockerenv: present
systemd-detect-virt -c: docker (exit 0)
```

### Hardened probe

The probe compiled with exit 0 and ran once under UID 1000:

```text
pid=5190 uid=1000 euid=1000
traceme+exec+regset            result=PASS errno=0 (none) SIGTRAP exec-stop
attach-direct-child            result=PASS errno=0 (none)
seize-direct-child             result=PASS errno=0 (none)
process-vm-write               result=FAIL errno=38 (Function not implemented) direct child
process-vm-read                result=FAIL errno=38 (Function not implemented) direct child
ptrace-word-io                 result=PASS errno=0 (none) stopped direct child
seize-live-grandchild          result=PASS errno=0 (none) ancestor, not direct parent
seize-sibling                  result=PASS errno=0 (none) same UID
seize-sibling-pr-set-ptracer   result=FAIL errno=22 (Invalid argument) PR_SET_PTRACER failed
seize-dumpable-zero-child      result=FAIL errno=1 (Operation not permitted)
seize-orphan-no-subreaper      result=PASS errno=0 (none) reparented away
seize-orphan-subreaper         result=PASS errno=0 (none) reparented to supervisor
required-summary               result=PASS errno=0 (none)
ptrace-probe-exit:0
```

The `process_vm_*` failures are optional transfer-path results, not ptrace
attach failures. Codex Cloud returned `ENOSYS`, while the required ptrace word
fallback passed. The `PR_SET_PTRACER` result is also optional because this
kernel does not expose Yama and rejected that Yama-specific request.

### Current explicit-trap injector

`x86_64-unknown-none` was initially absent. The only permitted setup command,
`rustup target add x86_64-unknown-none`, exited 0. `cargo build --locked -p
inject_demo` then exited 0. The exact built binary was copied to a path readable
by UID 1000 and executed with `CapEff=0`:

```text
payload: 40960 bytes, entry +0x36c4, 142 relocations
mapped 41152 bytes into the target
detached — payload will restore the exec context in-process
fspy_preload_linux: installed SIGSYS handler
openat: /etc/ld.so.cache
openat: /lib/x86_64-linux-gnu/libc.so.6
openat: test_path
SIGSYS works
/bin/cat exited with code 0
inject-demo-exit:0
```

### Clean-target confirmation

[`task_e_6a8b2967ff00832fac6ff1dce29a1f5f`](https://chatgpt.com/codex/tasks/task_e_6a8b2967ff00832fac6ff1dce29a1f5f)
repeated the test from a brand-new Cargo target directory under `/tmp`. This
ruled out a stale repository target directory. The task ran in:

```text
Linux f0d885472fcc 6.18.35 #1 SMP Mon Jul 27 18:07:50 UTC 2026 x86_64 GNU/Linux
probe identity: uid=1000, euid=1000, CapEff=0000000000000000
Seccomp=0, NoNewPrivs=0, Yama unavailable
```

It calculated these hashes inside Codex Cloud:

```text
aa290b418e11a819faddc7be36c33da9940a905fe672d868d448aa2ed4cde0bb  Cargo.lock
896ef4f2b1b2a42ca1d39caad1fc45f124cd0ba1a62e5ef170cb687dc88107f4  rust-toolchain.toml
0a2af36eadd8157e1ce1d329a992ac76ca8e5cb18d0ed635c8b4218a4e11d8ea  .cargo/config.toml
b5b779d64ff7eb7e43d3022f4fab61e2fcda3f6f5607fd7bea49d8f2df628c20  crates/fspy_preload_linux/Cargo.toml
e22a8c7eb963453a9eb29cd2c057888cef672eb69a7df701fd907bf971dda3e6  crates/fspy_preload_linux/build.rs
2434d0211c2c652645f9170344c021cbd8456906b9ca191e2bb84342d2c446c1  crates/fspy_preload_linux/src/lib.rs
f407c6428ed190c666c74040570a1d7d5a2903424c8900cf732fe41e5b469606  crates/fspy_preload_linux/src/main.rs
6da16bd6cae98f16f06bd9ac2bb632b8527af180253ced9779b325310b0cf5c4  crates/fspy_preload_linux/src/sigsys.rs
```

The build used:

```text
rustc 1.99.0-nightly (73dc9167f 2026-08-01)
LLVM 22.1.8
cargo 1.99.0-nightly (7c83d4cc0 2026-07-29)
active toolchain: nightly-2026-08-02-x86_64-unknown-linux-gnu
```

None of `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, `CARGO_TARGET_DIR`,
`CARGO_INCREMENTAL`, `RUSTC_WRAPPER`, `RUSTC_WORKSPACE_WRAPPER`, or
`CARGO_PROFILE_*` was set before the task supplied its new target directory.
The clean build freshly compiled both `fspy_preload_linux` and `inject_demo`.
Its artifacts were:

| Artifact             |            Size | SHA-256                                                            | ELF metadata                                                                 |
| -------------------- | --------------: | ------------------------------------------------------------------ | ---------------------------------------------------------------------------- |
| `fspy_preload_linux` |    56,976 bytes | `eaf2cf2b4de0e6d20204478637ef728dea8958b81a369ea0e3594471080a5bf4` | entry `0x36c4`, 142 `.rela.dyn` relocations                                  |
| `inject_demo`        | 1,688,272 bytes | `2f2f1e1d8513619466accd5422eb677994fa7e988749e4f8318f1fea7b6f0dbd` | x86-64 PIE, entry `0x5d450`, 3,308 `.rela.dyn` and 4 `.rela.plt` relocations |

The copied clean injector again ran as UID 1000 with `CapEff=0`, printed
`entry +0x36c4, 142 relocations`, intercepted `openat: test_path`, and exited 0.
The final repository status remained empty.

An earlier strict attempt,
[`task_e_6a8b26451354832f8e9ce158ea2115ec`](https://chatgpt.com/codex/tasks/task_e_6a8b26451354832f8e9ce158ea2115ec),
ran the probe only as root and could not build the injector because
`x86_64-unknown-none` was missing. Its build exit was 101 with Rust error
`E0463`. That was a build-environment dependency, not a ptrace denial. The
unprivileged rerun above supersedes it.

## GitHub Copilot coding agent

The authoritative rerun is
[agent session `5f848b4f-cc0c-4678-865a-2ca5b0e37639`](https://github.com/voidzero-dev/vite-task/pull/697/agent-sessions/5f848b4f-cc0c-4678-865a-2ca5b0e37639)
on closed [PR #697](https://github.com/voidzero-dev/vite-task/pull/697).

### Environment

```text
Linux runnervm76f27 6.17.0-1022-azure #22-Ubuntu SMP Mon Jul 27 17:24:03 UTC 2026 x86_64 GNU/Linux
Ubuntu 24.04.4 LTS
uid=1001(runner) gid=1001(runner)
CapEff:          0000000000000000
NoNewPrivs:      0
Seccomp:         0
Seccomp_filters: 0
YamaScope:       1
/proc/self/cgroup: 0::/user.slice/user-0.slice/session-c1.scope/ebpf-cgroup-firewall
/.dockerenv: absent
systemd-detect-virt: microsoft (exit 0)
```

This was an Azure VM rather than a container. The running process had no
effective Linux capabilities.

### Hardened probe

The probe compiled with exit 0 and ran under UID 1001:

```text
pid=3983 uid=1001 euid=1001
traceme+exec+regset            result=PASS errno=0 (none) SIGTRAP exec-stop
attach-direct-child            result=PASS errno=0 (none)
seize-direct-child             result=PASS errno=0 (none)
process-vm-write               result=PASS errno=0 (none) direct child
process-vm-read                result=PASS errno=0 (none) direct child
ptrace-word-io                 result=PASS errno=0 (none) stopped direct child
seize-live-grandchild          result=PASS errno=0 (none) ancestor, not direct parent
seize-sibling                  result=FAIL errno=1 (Operation not permitted) same UID
seize-sibling-pr-set-ptracer   result=PASS errno=0 (none) target opted in
seize-dumpable-zero-child      result=FAIL errno=1 (Operation not permitted)
seize-orphan-no-subreaper      result=FAIL errno=1 (Operation not permitted) reparented away
seize-orphan-subreaper         result=PASS errno=0 (none) reparented to supervisor
required-summary               result=PASS errno=0 (none)
probe_exit:0
```

The sibling, non-dumpable, and reparented-orphan failures are expected under
Yama scope 1. Direct descendants, an opted-in sibling, and an orphan retained
through subreaper ancestry passed.

### Current explicit-trap injector

`x86_64-unknown-none` was initially absent. The explicitly permitted `rustup
target add x86_64-unknown-none` command exited 0, then `cargo run --locked -p
inject_demo` built and ran successfully:

```text
payload: 40960 bytes, entry +0x3550, 141 relocations
mapped 41152 bytes into the target at 0x7f7cc7bda000
detached — payload will restore the exec context in-process
fspy_preload_linux: installed SIGSYS handler
openat: /etc/ld.so.cache
openat: /lib/x86_64-linux-gnu/libc.so.6
openat: test_path
SIGSYS works
/bin/cat exited with code 0
```

The `cargo run` command's exact exit status was 0.

### Payload artifact drift

Codex printed payload entry `+0x36c4` with 142 relocations. Copilot and the
strict hosted-CI builds printed `+0x3550` with 141 relocations from the same
source commit. A fresh Codex build in a new `/tmp` target directory reproduced
`+0x36c4` and 142, so stale Cargo output is ruled out. These values describe the
embedded `fspy_preload_linux` ELF, not the supervisor's ptrace control flow.
The change from single-step to explicit trap modified only `inject_demo`; it
did not modify the embedded payload sources. Therefore matching an older Codex
payload count does not imply that Codex ran the older supervisor. Both payloads
completed the functional test, and Codex compiled source that explicitly uses
`PTRACE_CONT` plus the syscall-and-trap patch. The available captures do not
explain the remaining cross-provider build difference, so this note does not
claim byte-for-byte payload reproducibility.

The first Copilot attempt,
[agent session `c71c9dde-f9ef-4260-bcf5-a498f69c4142`](https://github.com/voidzero-dev/vite-task/pull/696/agent-sessions/c71c9dde-f9ef-4260-bcf5-a498f69c4142)
on closed [PR #696](https://github.com/voidzero-dev/vite-task/pull/696), also
passed the hardened probe but could not build the injector. It exited 101
because the target component was missing and installation was not authorized.
That was not a ptrace failure. PRs #696 and #697 were closed after their task
pages and results were recorded, and only their ephemeral Copilot branches were
deleted. The shared strict source branch remains available for reproduction.

## Evidence drift from earlier tasks

Earlier evidence remains useful but does not validate the same source:

- The original
  [Codex probe task](https://chatgpt.com/codex/tasks/task_e_6a8b1de64ec0832f84c9603b3bbc0274)
  used a smaller purpose-built probe.
- The earlier
  [Codex follow-up](https://chatgpt.com/codex/tasks/task_e_6a8b1fb8c95c832fbf8651a55ac3495c)
  validated ptrace word I/O and the then-current injector, which still used the
  older single-step execution path.
- The earlier
  [Copilot session](https://github.com/voidzero-dev/vite-task/pull/695/agent-sessions/39b68ac9-ccd6-4ed6-9b89-93977885ee16)
  on [PR #695](https://github.com/voidzero-dev/vite-task/pull/695) ran a
  pre-hardening environment probe but did not run `inject_demo`.

Do not cite those earlier tasks as evidence for the current strict probe or the
explicit-trap injector. The strict reruns linked above supersede them.

## Reproduction

Use the retained source revision and compare hashes before interpreting a
provider result:

```sh
git switch research/ptrace-strict-hosted-wsl-20260824
git rev-parse HEAD
sha256sum research/ptrace-environment-probe.c crates/inject_demo/src/main.rs

gcc -O2 -Wall -Wextra -Werror \
  research/ptrace-environment-probe.c \
  -o /tmp/ptrace-environment-probe
/tmp/ptrace-environment-probe

rustup target list --installed
# If and only if the target is missing:
rustup target add x86_64-unknown-none
cargo run --locked -p inject_demo
```

Record the exact exit status of the probe and injector, not only printed PASS
lines. Also record `id`, `CapEff`, `NoNewPrivs`, `Seccomp`, `Seccomp_filters`,
Yama, cgroups, and container or VM identity.

No provider account, key, capability, paid resource, or security-policy change
was created for these tests. They used already-authorized coding-agent access.

## Side effects and cleanup

- The successful Codex, Claude, and Copilot build tasks installed only the
  `x86_64-unknown-none` Rust standard-library component inside their ephemeral
  task environments.
- The three strict-rerun Codex task pages, the Claude session, and the two
  closed Copilot PR/session pages remain as evidence.
- Copilot PRs #696 and #697 are closed, and their task branches are deleted.
  The shared strict source branch is retained because it is the reproducible
  input for all current reruns.
- The temporary local Copilot transcript copy was removed after its exact
  results were recorded here. The closed session page retains the transcript.
- The clean Codex task first searched the nonexistent
  `crates/fspy_supervisor` path and received exit 2, then found and verified the
  implementation in `crates/inject_demo`. Its optional `file` command check
  also returned exit 1 because the command was unavailable; `readelf` supplied
  the ELF metadata without installing anything.
- A later local review hardened the probe further, so the current workspace
  copy has SHA-256 `da027737ccb9cc41329ba20cc43d9d01b5371c8bfa0bd67b48ffc0452534277a`.
  The retained remote commit preserves the exact `abbb06bc...1ec814c7`
  source used by these AI-sandbox captures.
- This subtask changed research notes only. It did not modify existing local
  product-code work in `Cargo.lock`, `crates/inject_demo`, or
  `crates/fspy_loader`.

## Anthropic Claude Code cloud

The direct run is retained in
[Claude Code session `session_011gTaLZ2jTXZwKexmGHMZNu`](https://claude.ai/code/session_011gTaLZ2jTXZwKexmGHMZNu).
It checked out the same tested commit and verified that `inject_demo` uses
`ptrace::cont` plus `patch_syscall_and_trap`, with no `ptrace::step` or
`PTRACE_SINGLESTEP` reference.

### Environment and scope

The commands ran directly in the Claude Code cloud task, not in a nested test
container. Claude's task user was root and its effective capability mask
included `CAP_SYS_PTRACE`. `NoNewPrivs`, seccomp mode, and seccomp-filter count
were all zero. The kernel exposed no Yama `ptrace_scope` file. Docker marker
files were absent, while `systemd-detect-virt -c` reported Docker and cgroups
used an Anthropic-managed `process_api/.../claude-code-bash` hierarchy.

No sudo, capability, seccomp, Yama, or container-policy change was made. This
is a valid test of what the product currently permits, but it does **not** show
that Claude Code cloud permits the injector for an unprivileged,
capability-free task user. Codex and Copilot provide that stronger evidence.

### Hardened probe

The probe compiled with `-Werror`, reported `required-summary result=PASS`, and
exited 0. Required `TRACEME`/exec/regset, direct-child seize, ptrace word I/O,
and live-grandchild seize checks all passed. Both `process_vm_*` operations and
the applicable additional attach/relationship checks passed. The optional
`PR_SET_PTRACER` check returned `EINVAL`, as expected on this kernel without
Yama; it did not affect the required summary.

### Current explicit-trap injector

The first `cargo run --locked -p inject_demo` exited 101 before execution
because the pinned toolchain lacked `x86_64-unknown-none`. After explicit user
approval, the task installed only that Rust target. The exact command then
built and ran successfully:

```text
payload: 40960 bytes, entry +0x36c4, 142 relocations
mapped 41152 bytes into the target
detached — payload will restore the exec context in-process
fspy_preload_linux: installed SIGSYS handler
...
openat: /etc/ld.so.cache
openat: /lib/x86_64-linux-gnu/libc.so.6
openat: test_path
SIGSYS works
/bin/cat exited with code 0
CARGO_EXIT=0
```

The repository was clean before and after. Only the approved Rust target,
normal Cargo artifacts, and the probe binary under `/tmp` were created in the
ephemeral task.
