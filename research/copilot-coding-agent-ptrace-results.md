# Copilot coding-agent ptrace compatibility results

Direct run in the GitHub Copilot coding-agent sandbox (ordinary agent user, no delegation).

> Historical capture: this session used the pre-hardening probe, before the
> strict `required-summary` and `ptrace-word-io` checks, and it did not run
> `inject_demo`. See the [strict/current AI-sandbox reruns](ai-sandbox-ptrace-results.md)
> for the authoritative compatibility result. The output below is preserved as
> an exact record of the earlier session.

## Scope and inputs

- Repository: `voidzero-dev/vite-task`
- Working area: `research/` only
- Probe source compiled: `research/ptrace-environment-probe.c`
- Compile command: `gcc -O2 -Wall -Wextra -Werror research/ptrace-environment-probe.c -o /tmp/ptrace-environment-probe`
- Exec-boundary injection primitive probe on base branch (`origin/main`): `research/ptrace-exec-injection-primitives-probe.c` **absent** (not recreated)

## Environment capture

### `uname -a`

```text
Linux runnervm76f27 6.17.0-1022-azure #22-Ubuntu SMP Mon Jul 27 17:24:03 UTC 2026 x86_64 x86_64 x86_64 GNU/Linux
```

### `/etc/os-release`

```text
PRETTY_NAME="Ubuntu 24.04.4 LTS"
NAME="Ubuntu"
VERSION_ID="24.04"
VERSION="24.04.4 LTS (Noble Numbat)"
VERSION_CODENAME=noble
ID=ubuntu
ID_LIKE=debian
HOME_URL="https://www.ubuntu.com/"
SUPPORT_URL="https://help.ubuntu.com/"
BUG_REPORT_URL="https://bugs.launchpad.net/ubuntu/"
PRIVACY_POLICY_URL="https://www.ubuntu.com/legal/terms-and-policies/privacy-policy"
UBUNTU_CODENAME=noble
LOGO=ubuntu-logo
```

### `id`

```text
uid=1001(runner) gid=1001(runner) groups=1001(runner),4(adm),100(users),118(docker),999(systemd-journal)
```

### `/proc/self/status` fields

```text
CapEff:	0000000000000000
NoNewPrivs:	0
Seccomp:	0
```

### `kernel.yama.ptrace_scope`

```text
1
```

(readable: yes)

### cgroup/container hints

```text
/proc/self/cgroup:
0::/user.slice/user-0.slice/session-c1.scope/ebpf-cgroup-firewall

/proc/1/cgroup:
0::/init.scope

/.dockerenv: absent
/run/.containerenv: absent
systemd-detect-virt -c: none
```

## Probe build and execution

### Compilation result

```text
compile_exit_status=0
```

### Complete `ptrace-environment-probe` output

```text
pid=4066 uid=1001 euid=1001
CapEff:	0000000000000000
NoNewPrivs:	0
Seccomp:	0
YamaScope:	1
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
probe_exit_status=0
```

No compilation or execution blocker was encountered in this sandbox run.
