# Userland exec compatibility study

Research date: 2026-08-02

## Verdict

The proposed architecture is feasible for fspy's build-tool workload, with an
important qualification: every logical exec must first perform a real kernel
exec of a fresh, single-threaded `fspy_host`, and only that host may enter the
target through the userland ELF loader.

That kernel exec kills sibling threads,
closes `CLOEXEC` descriptors, clears the alternate signal stack, resets caught
signal dispositions, and discards the old address space. The seccomp filter
survives. The new host must reinstall its physical `SIGSYS` handler through a
trusted raw-syscall bootstrap before it makes any syscall that the inherited
filter traps. It can then map and enter the logical target without another
kernel exec, preserving the new handler.

This does not fully implement Linux `execve`. It can cover
ordinary unprivileged build tools, including current esbuild, Node, shells,
dynamic glibc, static musl, and static Go. It cannot faithfully reproduce
target-file credential/LSM transitions or the target's kernel-owned process
identity. Those limitations need an explicit support contract.

The loader should be implemented in-house. Libreflect is the most useful small
reference and passed the broadest relevant matrix in this run, but its parser,
mapping decisions, initial stack, and auxiliary vector are not production
quality. Anvil remains a useful behavior reference, not an implementation
base.

## Environment and method

- Ubuntu 24.04.4 AArch64 in the `fspy-sigsys` Lima VM
- Linux 6.8.0-134-generic, 4 vCPUs, 6 GiB
- GCC 13.3, glibc 2.39, musl 1.2.4, Go 1.22.2, Node 18.19.1
- esbuild 0.28.1 from `@esbuild/linux-arm64`, a static Go executable
- Direct AArch64 build of libreflect's pure mapper; its configure script
  incorrectly selected the `memfd_create`/`execveat` fallback in this layout
- Anvil `ulexecve.py` at the revision in the supplied prior research
- The full trap case uses `../sigsys-prototype/trap_preload.c`, which traps the
  filesystem syscall family, `getpid`, `exit_group`, `rt_sigaction(SIGSYS)`,
  and `rt_sigprocmask`, then reissues trusted syscalls in-process

`run-study.sh` recreates the build and matrix. It accepts an output directory
as its second argument and uses a temporary-directory default. The last compact
result set is in `aarch64-lima-summary.tsv`.

The duration column is diagnostic, one sample per case, and not a benchmark.

## Compatibility results

| Workload                                     | Libreflect pure handoff | Anvil pure handoff | Interpretation                                          |
| -------------------------------------------- | ----------------------- | ------------------ | ------------------------------------------------------- |
| glibc dynamic PIE C, pthreads, `posix_spawn` | Pass                    | Pass               | Ordinary dynamic ELF works                              |
| glibc dynamic non-PIE                        | Pass                    | Pass               | Both happened to obtain the fixed mapping               |
| static musl, pthreads, `posix_spawn`         | Pass                    | Pass               | Static musl works despite imperfect auxv                |
| static Go 1.22                               | Pass                    | SIGSEGV            | Anvil reference bug, not a kernel limit                 |
| esbuild 0.28.1 CLI bundle                    | Pass                    | SIGSEGV            | Libreflect handles a current static Go frontend         |
| Node, filesystem, worker thread, shell child | Pass                    | Pass               | Main runtime functionality works                        |
| Node self-reexec                             | Fail                    | Fail               | Both expose the physical host as executable identity    |
| `/bin/sh` and `/bin/echo`                    | Pass                    | Pass               | Common dynamic tools work                               |
| Direct shebang input                         | Abort                   | Rejected           | Neither reference parses scripts                        |
| Shebang expanded to `/bin/sh script ...`     | Pass                    | Not tested         | Implementable host feature                              |
| SIGSYS handler survival, dynamic C           | Pass                    | Pass               | Pure handoff preserves the physical handler             |
| Full trapped-filesystem esbuild bundle       | Pass                    | Not tested here    | Signal virtualization is sufficient for current esbuild |

The earlier supplied evaluation reported libreflect failures for static ELF.
That is not true in this Ubuntu 24.04/Linux 6.8 run: static musl, static Go, and
current esbuild all completed. This does not make libreflect deterministic.
For an `ET_EXEC` image it uses the requested address only as an `mmap` hint and
then assumes the hint was honored. A collision will still break it.

The raw Anvil failures on Go and esbuild are also narrower than they look. The
companion SIGSYS study traced current esbuild's crash to Anvil's invalid
`AT_RANDOM` pointer; its corrected wrapper runs esbuild. That is a fixable
loader defect. Anvil still maps an entire image RWX and is unsuitable for the
production host.

## esbuild and frontend-tool result

Three esbuild paths passed:

1. Libreflect directly loaded the static esbuild 0.28.1 executable and bundled
   the two-file TypeScript fixture with a source map.
2. Userland-loaded Node invoked the esbuild JavaScript API. The API kernel-
   execed the test host wrapper, which userland-loaded the static esbuild
   service. Build and transform output matched the native control exactly:
   esbuild 0.28.1, a 1,395-byte bundled output, and `const answer=42;`.
3. Libreflect loaded esbuild under the full seccomp trap prototype. Bundle and
   minification succeeded with this final counter line:

   ```text
   fspy-sigsys-probe: fs=86 getpid=0 sigaction=2 sigprocmask=14
   ```

The third case is the meaningful SIGSYS result. The simpler inherited-handler
probe trapped only `getpid`; esbuild happened not to call it after Go startup,
so that apparent pass did not exercise the collision.

The full trap also ran Node's main thread, filesystem operations, and worker
thread. Its child execs did not work because the research DSO does not yet
transform `execve` into a fresh host. A real exec clears the handler while
leaving the filter installed, so a child cannot run until the new host
reinstalls the handler.

The companion `reexec_bootstrap.c` closes the basic bootstrap question: a
static-musl program installed the filter, performed a trusted real exec of its
own image, reinstalled the handler with a magic-bypassed raw `rt_sigaction`,
then successfully trapped `getpid` in the fresh image. What remains unproven
end to end is decoding an arbitrary trapped `execve`/`execveat`, carrying its
target fd, argv, and environment into the host, and completing Node's real
child-process chain.

## SIGSYS requirements

A naive preserved handler is not compatible with Go or multithreaded Node:

- Static Go installs its own `SIGSYS` action. With the simple filter, its next
  trapped `getpid` entered Go's handler and terminated with `SIGSYS: bad system
call`.
- Go repeatedly changes signal masks while creating threads.
- Alternate signal stacks are per-thread. A handler that requires the host's
  original alternate stack failed when Node trapped on a worker thread.

The full prototype fixes the tested Go case by keeping the physical host action
installed while presenting a virtual action to the target, and by removing
`SIGSYS` from the real kernel mask while preserving a target-visible mask. Its
successful esbuild run proves this approach, including direct Go syscalls and
Go's signal initialization.

Production work remains:

- make virtual masks per-thread rather than global;
- mediate `rt_sigreturn` and syscalls with temporary signal masks such as
  `pselect6`, `ppoll`, and `epoll_pwait`;
- define `sigaltstack`, `signalfd(SIGSYS)`, and explicit `kill(SIGSYS)` behavior;
- run the host handler on the target stack by default, or fully multiplex the
  one per-thread alternate-stack slot;
- keep the handler, restorer, syscall gate, and state free of TLS, allocation,
  libc locks, and callbacks into the abandoned host runtime;
- use `SA_NODEFER` or an equivalent trusted path for nested traps.

This is deliberate signal virtualization, not native signal compatibility.

## Exec transition that should be built

The safe sequence is:

1. A target thread calls `execve` or `execveat` and receives `SIGSYS`.
2. The handler resolves the logical target, preserves an executable fd when
   needed, and reissues a trusted real `execve` of `fspy_host`. The original
   logical argv should be passed as the host argv; target metadata belongs in
   a reserved fd or scrubbed environment entry, not as an extra argv prefix.
3. Kernel exec performs normal thread, fd, signal, and address-space cleanup.
4. A minimal static host bootstrap uses a trusted syscall gate to reinstall
   the physical handler under the inherited filter.
5. The host parses scripts or ELF, validates and maps the image, constructs the
   initial stack, removes private metadata from the logical environment, and
   enters the target.

Steps 3 and the early handler reinstall in step 4 passed in the companion
static-musl bootstrap probe. The target transformation and loader integration
around them still need to be built.

Passing the original argv to the host matters. A host-shaped Node experiment
made `/proc/self/cmdline` exactly logical and retained `process.argv0` as
`/usr/bin/node`. Node still derived `process.execPath` and `process.argv[0]`
from `/proc/self/exe`, so identity virtualization is separately required.

The host must not create a background thread before handoff. The residual-
thread probe showed that a pure mapper leaves such a thread alive. This is
avoidable because a freshly kernel-execed host can remain single-threaded.

## In-house ELF loader requirements

Start from the behavior of libreflect, not its API or unchecked code:

- Accept a bounded byte slice or fd and validate ELF magic, class, endian,
  machine, ABI, header sizes, table bounds, segment bounds, and all arithmetic.
- Parse shebangs before ELF, including Linux optional-argument rules,
  `/usr/bin/env -S`, recursion limits, and `execveat(AT_EMPTY_PATH)` inputs.
- Reserve the complete image span, choose aligned load biases for `ET_DYN`, and
  use checked fixed placement for `ET_EXEC`. Reject collisions with retained
  host mappings instead of overwriting or silently relocating.
- Map `PT_LOAD` from the target fd where useful, zero partial-page and full BSS
  correctly, and apply exact final W^X permissions.
- Handle `PT_INTERP`, `PT_GNU_STACK`, `PT_GNU_RELRO`, large `p_align`, and the
  supported static-PIE relocation set.
- Build a properly aligned owned initial stack. Copy argv, environment,
  platform, exec filename, and 16 genuine random bytes into it.
- Construct correct `AT_PHDR`, `AT_PHENT`, `AT_PHNUM`, `AT_ENTRY`, `AT_BASE`,
  `AT_RANDOM`, `AT_EXECFN`, HWCAP, UID/GID, vDSO, and architecture entries.
- Keep a small position-independent survivor region for the handler, raw
  syscall gate, restorer, immutable logical-exec metadata, and mutable state.
- Trap `mmap(MAP_FIXED)`, `mremap`, `munmap`, and `mprotect` operations that
  would replace or alter the survivor region.

Libreflect's observed auxv remains wrong even where programs tolerate it. It
omits `AT_EXECFN`, uses auxiliary-vector bytes as `AT_RANDOM`, and reports a
nonzero `AT_BASE` for a static musl executable. These are required fixes.

## Identity and kernel-semantic boundary

The userland target still has the kernel identity of `fspy_host`:

- `/proc/self/exe`, `/proc/<pid>/exe`, kernel `PR_GET_AUXV`, audit and ptrace
  exec events, and external observers identify the host;
- the address map retains the host/survivor mappings;
- kernel exec applies the host file's credentials and LSM transition, not the
  target file's set-user-ID bits, file capabilities, or exec labels.

For ordinary build tools, mediate self-inspection calls for `/proc/self/exe`,
`/proc/self/auxv`, and `PR_GET_AUXV`, and construct the correct user stack.
The exec handler already knows the logical executable, so it can also treat an
exec of `/proc/self/exe` as a logical self-reexec. This should fix the Node and
Go self-reexec failures seen here. It cannot make external observers or kernel
subsystems see the logical target.

Declare these cases unsupported on the SIGSYS backend:

- set-user-ID/set-group-ID and file-capability executables;
- programs that require a target-specific LSM exec transition;
- hostile programs that deliberately bypass the trusted gate or overwrite
  retained mappings;
- an ELF whose mandatory fixed mappings collide with the survivor region;
- exact ptrace/audit/procfs executable identity.

`PR_SET_MM_EXE_FILE` is an optional privileged improvement, not a general
solution; it is unavailable in normal rootless containers and CI jobs.

## Performance implication

The companion SIGSYS prototype on the same VM measured approximately:

| Path                                               |   Median |
| -------------------------------------------------- | -------: |
| register-only in-process trap                      | 0.566 us |
| trap plus trusted syscall reissue                  | 0.714 us |
| trapped filesystem path with safe copy and reissue | 1.452 us |
| cross-process seccomp user notification            |  13.9 us |

The in-process path is about an order of magnitude faster than
user notification for a representative filesystem trap, while unlike
`LD_PRELOAD` it sees direct syscalls. A minimal preload interposer was within
measurement noise of native dispatch; the SIGSYS backend necessarily costs
more per intercepted syscall.

Single, unpinned compatibility samples showed native versus libreflect times
of 5 versus 13 ms for the esbuild CLI and 55 versus 68 ms for the Node esbuild
API chain. These include process startup, mapping, and workload time and should
not be treated as stable benchmark numbers. A production benchmark must
compare actual fspy recording work, repeated exec-heavy graphs, and
filesystem-heavy builds on native x86-64 and AArch64 CI machines.

## Recommendation

Proceed with an in-house prototype behind a feature flag. The next milestone
should turn the proven AArch64 host bootstrap into one complete path:

- custom trusted host bootstrap under an inherited filter;
- trapped `execve`/`execveat` transformed to a real exec of the host;
- correct dynamic PIE, static Go, and shebang loading;
- process-wide virtual SIGSYS action plus per-thread virtual signal masks;
- Node `child_process`, Node self-spawn, Go self-reexec, and esbuild API tests;
- explicit failure for credentials, LSM transitions, and mapping collisions.

Then port the survivor/gate ABI to x86-64 and run the same matrix in Docker,
WSL2, GitHub Actions, and Kubernetes. Keep the current backend as a fallback
until that matrix and workload benchmarks pass.
