# Running fspy after `exec` and before the target

Status: validated research, not a production implementation

Last updated: 2026-08-23

Prototype baseline: `poc/sigsys-injection` at `66ae8432`

## Decision

No mechanism available on deployed Linux kernels is simultaneously universal,
fully exec-transparent, unprivileged, and independent of ptrace. The leading
candidate architecture is tiered:

1. For an ordinary readable dynamic ELF, execute a cached copy whose
   `PT_INTERP` names an `O_CLOEXEC` descriptor for an augmented copy of the
   target's original dynamic loader. The augmentation enters fspy first, then
   jumps to the loader's saved entry point.
2. For an ordinary readable static ELF, enter an appended fspy segment through
   a changed `e_entry`, then jump to the saved target entry point.
3. Keep optimized ptrace injection for cases where executing the original inode
   matters and tracing remains semantically valid, including scripts and
   execute-only files. Run set-ID and file-capability executables natively
   without fspy, or reject them explicitly; tracing suppresses their privilege
   transition.

The dynamic mechanism was validated with glibc and musl on native AArch64, and
with glibc x86-64 under QEMU user emulation. The x86-64 test used a stable
loader pathname rather than `/proc/self/fd/N`. Its cached warm-entry cost was
about 3 microseconds in a deliberately tiny-exec benchmark. This is much less
than the current ptrace payload transfer, which added about 5.84 milliseconds.
An optimized ptrace transfer using one `process_vm_writev` added about 110
microseconds before injected-runtime initialization.

The full custom-loader design, where the kernel executes fspy as the main image
and fspy maps the original target entirely in userspace, is feasible but is not
the preferred fast path. It recreates too much of `exec`, has greater observable
drift, and has not been benchmarked. Modifying the target's existing loader lets
the kernel retain responsibility for loading the main executable and building
the initial stack.

Future Linux kernels have an even better answer: the new `binfmt_misc` `L`
loader-substitution mode loads the original executable normally and substitutes
only its `PT_INTERP`. It removes the need to copy the target. It landed after
Linux 7.2 and therefore cannot be the default for current WSL, Docker, CI, or
Kubernetes environments.

## Required timing

There are two useful meanings of “before the target”:

- Before the main ELF's `e_entry`. Dynamic-loader hooks and constructors can
  satisfy this weaker boundary.
- Before any ordinary userspace instruction in the new image. This includes
  running before the original `ld.so`, whose startup performs filesystem and
  memory syscalls. fspy needs this stronger boundary so its `SIGSYS` handler is
  installed before an inherited seccomp filter can trap those syscalls.

This note evaluates the stronger boundary. The kernel must already have
committed the exec so that close-on-exec descriptors are closed, old signal
dispositions are reset, sibling threads are gone, and the old address space is
discarded. fspy must then be the first userspace code entered.

Linux opens the ELF `PT_INTERP` before `begin_new_exec`, performs the irreversible
exec transition, maps the main executable and interpreter, and finally starts
at the interpreter's entry point. The relevant order is visible in the Linux
6.8 sources:

1. [`load_elf_binary` reads and opens `PT_INTERP`](https://github.com/torvalds/linux/blob/v6.8/fs/binfmt_elf.c#L868-L917).
2. [`begin_new_exec` commits the new image](https://github.com/torvalds/linux/blob/v6.8/fs/binfmt_elf.c#L984-L998), including [closing `CLOEXEC` descriptors](https://github.com/torvalds/linux/blob/v6.8/fs/exec.c#L1342-L1349).
3. [The interpreter is mapped from the kernel's retained file](https://github.com/torvalds/linux/blob/v6.8/fs/binfmt_elf.c#L1199-L1221).
4. [The kernel transfers control to the interpreter](https://github.com/torvalds/linux/blob/v6.8/fs/binfmt_elf.c#L1282-L1299).

That ordering makes an augmented interpreter a real post-exec, pre-loader
entry point without ptrace or elevated privileges.

## Recommended dynamic fast path

### 1. Select transformed artifacts before exec

The in-process `SIGSYS` exec handler resolves the requested executable and its
original `PT_INTERP`. It looks up or creates two immutable artifacts:

- a copy of the target with only `PT_INTERP` changed;
- an augmented copy of that exact original loader.

The loader artifact can be shared by every target using the same loader build.
The target artifact must be invalidated whenever the source executable changes.
The cache key must include content or stable file identity and metadata, not
just a pathname.

The changed `PT_INTERP` can be `/proc/self/fd/N`. If it fits in the existing
string, overwrite it in place and zero the remainder. Otherwise append the new
string to the artifact and update the `PT_INTERP` program header. The string
does not need its own mapped segment because the kernel reads it from the file.

### 2. Augment the target's original loader

The validated transformer performs these operations on a copy of `ld.so`:

1. Save the original ELF `e_entry`.
2. Preserve every original program header, including GNU properties, notes,
   unwind metadata, `PT_GNU_STACK`, and `PT_GNU_RELRO`.
3. Copy the program-header table into a new aligned area after the existing
   file contents.
4. Add one final `PT_LOAD` with read and execute permissions. It contains the
   copied program-header table and the processed fspy blob.
5. Change `e_phoff`, `e_phnum`, and `e_entry` to describe the new table and
   fspy entry.

Relocating the table is necessary in real loaders. The tested glibc AArch64
loader had no unused program-header slot; its table ended immediately before
the first note. Growing it in place would overwrite live metadata.

The transform must respect the loader's own assumptions in addition to the ELF
specification. In the musl experiment, putting the new table at file offset
`0xc0000` but virtual address `0xd0000` crashed. Placing it at equal file and
virtual offsets fixed the loader because its startup addressed the table as
`load_bias + e_phoff`. A production transformer must validate the complete
load layout and update `PT_PHDR` if present. It must not assume that merely
satisfying `p_vaddr % p_align == p_offset % p_align` is enough for every loader.

The appended fspy blob should reuse the target-none injected runtime:

- no libc and no Rust standard library;
- position-independent code;
- supervisor-prepared layout and a compact relative-relocation table;
- direct syscalls through `fspy_nostd`;
- no process-global allocator;
- architecture-specific entry and handoff stubs around ordinary Rust logic.

It cannot reuse `fspy_blob::bind` unchanged. Ptrace injection knows the remote
mapping base before writing the payload, so the supervisor can write absolute
`base + addend` values. The kernel chooses the augmented loader's ASLR load bias
during exec, after the supervisor has finished constructing the file. A minimal
entry stub must therefore derive that load bias and apply the payload's
`R_*_RELATIVE` relocations before entering Rust. The alternative is to prove
that the final payload contains no runtime relocations, which is too fragile for
future complex Rust dependencies. The original dynamic loader cannot do this
first because fspy deliberately runs before its self-relocation code.

The supervisor can still do the complex work: flatten the target-none ELF,
place it at the chosen loader-relative virtual address, validate that every
relocation is relative, and emit a compact list of patch offsets and addends.
The in-image stub only adds the runtime load bias. The marker-only proof used
pure position-independent assembly and did not validate this Rust relocation
step.

### 3. Execute through close-on-exec descriptors

Open the augmented loader at the descriptor named by the patched `PT_INTERP`,
with `O_CLOEXEC`. Execute the transformed target by pathname or with
`execveat(target_fd, "", argv, envp, AT_EMPTY_PATH)`. The target descriptor may
also be `O_CLOEXEC`.

The kernel resolves both files before closing descriptors and keeps its own
references while loading them. [`alloc_bprm` retains the main executable
file](https://github.com/torvalds/linux/blob/v6.8/fs/exec.c#L1535-L1552), while
the ELF loader retains the interpreter as described above. A dedicated AArch64
test used fd 3 for the main ELF and fd 9 for its interpreter, both `CLOEXEC`.
The interpreter's first instructions called `fcntl(F_GETFD)` and observed:

```text
before: main_fd=3/CLOEXEC interp_fd=9/CLOEXEC
MAIN_CLOSED
INTERP_CLOSED
```

This prevents transport descriptors from leaking into the target. It requires
procfs when `PT_INTERP` uses `/proc/self/fd/N`; executing the target itself with
`execveat(AT_EMPTY_PATH)` does not.

Scripts are an exception. Linux returns `ENOENT` when a shebang script is
executed through an inaccessible `O_CLOEXEC` descriptor because the script
interpreter needs a pathname by which it can reopen the script. The kernel
documents that case in [`binfmt_script.c`](https://github.com/torvalds/linux/blob/v6.8/fs/binfmt_script.c#L87-L94).

### 4. Initialize fspy and hand off

The kernel enters the appended blob before the original loader executes. It:

1. installs the `SIGSYS` action with the raw `rt_sigaction` wrapper;
2. reads the shared-memory path from the initial environment;
3. opens and maps the existing fspy channel;
4. initializes any immutable handler state;
5. transfers control to the saved loader entry without returning.

The handoff stub must preserve the process-entry register contract. Prefer a
direct relative branch where the architecture's range permits it. On AArch64,
a direct `b` avoids both changing `x30` and Branch Target Identification rules
for indirect calls. The glibc proof used `blr x16` because its loader advertised
BTI and began with `bti c`; `br x16` raised `SIGILL`. The musl proof, which did
not advertise BTI, used `br x16`. On x86-64, preserve CET properties and ensure
every injected indirect entry has `ENDBR64` when IBT is active.

Do not remove or weaken GNU property notes to make a prototype run. The kernel
selects architecture properties from the interpreter, so the augmented loader
must preserve the original security contract and make fspy code conform to it.

## Static ELF fast path

A static ELF has no `PT_INTERP`. Its equivalent transformation is:

1. copy the executable;
2. save `e_entry`;
3. append an aligned read-execute `PT_LOAD` containing fspy;
4. relocate or extend the program-header table safely;
5. change `e_entry` to fspy;
6. after initialization, jump to the saved entry.

The kernel still performs the real exec transition and maps the executable.
The cached warm overhead of a minimal added segment and entry branch measured
about 3 microseconds. The same executable-copy policy and identity problems as
the dynamic fast path remain.

Static PIE requires a position-independent payload and a load-bias-aware saved
entry. Fixed-address `ET_EXEC` needs collision checks when choosing the new
segment address. A transformed artifact must preserve architecture properties,
stack permissions, RELRO metadata, and every original load segment.

### Static ELF with an added `PT_INTERP`

Linux also accepts `PT_INTERP` on an ELF with no dynamic section. This gives a
second static strategy: add a fspy interpreter to a copy of the static target,
let the kernel map both images, then have fspy read `AT_ENTRY` from the initial
stack and jump there. The target's ELF `e_entry` field remains unchanged. The
shim uses the kernel-relocated `AT_ENTRY`, so it needs no per-target saved-entry
metadata. One interpreter artifact can serve many static targets of the same
architecture and ELF ABI whose GNU-property contracts are compatible.

This was validated in default Docker/Colima on Linux 6.8 AArch64 without added
capabilities or seccomp changes:

```text
static ET_EXEC:       SHIM, then TARGET, exit 0
static PIE ET_DYN:    SHIM, then TARGET_PIE, exit 0
Debian busybox-static SHIM, then BUSYBOX_TARGET, exit 0
busybox-static true:  SHIM, then exit 0
```

The same design also completed for a tiny static `ET_EXEC` and Debian
`busybox-static` x86-64 under the local Docker QEMU/binfmt emulation, producing
the shim marker before the target marker and exiting with status 0. This
validates x86-64 entry handoff, but is not a native x86-64 performance result.

`PT_INTERP=/proc/self/fd/9` also completed when fd 9 was `O_CLOEXEC`. This is
the same kernel ordering used by the dynamic design; that descriptor test was
native AArch64.

The proof reused `PT_GNU_STACK` as its `PT_INTERP` slot. That was acceptable
only for the experiment. Production must preserve `PT_GNU_STACK` and every
other header, relocating or extending the table when no slot is free. The
interpreter was a minimal static PIE that parsed auxv, printed the marker, and
branched to `AT_ENTRY`.

Compared with appending fspy to the target, this variant avoids embedding the
runtime in every target artifact and avoids storing a per-target saved entry.
It adds a separate mapping and changes more kernel-visible ELF semantics:

- a formerly static program now has a nonzero interpreter mapping and
  `AT_BASE`;
- on Linux 6.8, static PIE load bias, numeric `AT_ENTRY`, and `brk` placement
  follow the interpreted-ELF path rather than the native no-interpreter path;
- the kernel selects GNU architecture properties from the fspy interpreter;
- programs that assume they have no interpreter can observe the difference.

The transformed-copy identity and security-policy limits are unchanged. Keep
the appended-entry design as the lower-drift static candidate and benchmark the
shared-interpreter design with real static Go and musl programs before choosing
between them.

## Full custom loader plus userspace exec

The alternative design proposed for comparison is:

1. replace each requested exec with an exec of a standalone fspy loader;
2. let that real exec perform all kernel cleanup;
3. install fspy in the loader;
4. map the requested executable and its interpreter in userspace;
5. construct the target's initial stack and jump to its loader or entry.

This does satisfy the timing requirement. It is also the most expensive design
to make generally correct. The implementation must reproduce at least:

- ELF validation and `PT_LOAD` mapping for `ET_EXEC`, PIE, and static PIE;
- load-bias selection without colliding with itself, the vDSO, or the stack;
- BSS zeroing, partial-page handling, permissions, RELRO, and executable stack;
- `brk` placement;
- the initial argument, environment, and auxiliary-vector layout;
- `AT_PHDR`, `AT_PHENT`, `AT_PHNUM`, `AT_ENTRY`, `AT_BASE`, and `AT_EXECFN`;
- GNU CET, BTI, PAC, and other architecture properties;
- the handoff ABI for glibc, musl, and other loaders;
- shebang and nested-interpreter behavior if scripts are in scope.

The real dynamic loader can still perform relocations and TLS setup. fspy does
not need to reimplement that part. It must nevertheless create a kernel-like
mapping and stack that the loader accepts.

The larger problem is semantic, not mechanical. The kernel executed the fspy
loader, not the requested file. `/proc/self/exe`, committed credentials,
security-module checks, IMA appraisal, process accounting, and the kernel's
saved aux vector all describe the loader. Editing the userspace stack cannot
repair those kernel-owned facts. The loader also remains mapped so its SIGSYS
handler remains callable.

For those reasons, prefer changing `PT_INTERP`: the kernel then maps the target
as its main executable, constructs its real initial stack, and invokes fspy as
the interpreter. Prefer augmenting the original loader over a generic shim that
maps a second loader, because the former also avoids rewriting `AT_BASE` and
leaving two loader mappings.

## Performance

### Measurement boundary

The microbenchmarks measured wall time from `fork()` through child exit. The
native baseline therefore includes the `execve` syscall itself, kernel ELF
loading, ordinary dynamic-loader startup, and target exit. The incremental
figures subtract that complete baseline.

Tests ran warm, with repeated rounds and medians, in default Docker under
Colima on Linux/AArch64. Common fspy work was deliberately excluded: reading
the environment, opening and mapping shared memory, installing the final
signal action, and executing the injected runtime. The table compares entry
and transport mechanisms, not complete production implementations.

| Mechanism                                | Native baseline | Measured total |      Increment | What the result covers                                                                          |
| ---------------------------------------- | --------------: | -------------: | -------------: | ----------------------------------------------------------------------------------------------- |
| Augmented original `ld.so`               |      156.207 µs |     159.131 µs |     about 3 µs | Cached transformed target and loader, minimal entry marker, handoff to original loader          |
| nix-ld-style `PT_INTERP` shim            |      156.207 µs |     178.251 µs |    about 22 µs | Shim maps a second real loader, rewrites the userspace aux vector, and hands off                |
| Ptrace exec stop plus detach             |          150 µs |         180 µs |    about 30 µs | No remote mapping or payload transfer                                                           |
| Ptrace remote `mmap` plus one bulk write |          150 µs |         260 µs |   about 110 µs | Exec stop, register work, remote syscall, one 143,680-byte `process_vm_writev`, restore, detach |
| Current ptrace word transfer             |          150 µs |       5.991 ms | about 5.841 ms | Same remote mapping plus 17,960 `PTRACE_POKEDATA` calls for a release-sized payload             |
| Static ELF appended entry segment        |           78 µs |          81 µs |     about 3 µs | Cached transformed static target and minimal entry branch                                       |

The precise 3-microsecond loader result is machine- and harness-specific. Its
defensible conclusion is that the cached mechanism adds low-single-digit
microseconds before fspy initialization. The current ptrace result strongly
identifies the present bottleneck: transferring 143,680 bytes eight bytes per
ptrace call. It is still a pre-initialization lower bound rather than full
`inject_demo` wall time.

The bulk-write ptrace experiment is also a transport proxy, not an implemented
production total. It excludes payload relocation work, seccomp setup, fspy
initialization, and any descendant exec handshake. It demonstrates that ptrace
does not inherently require milliseconds per exec: replacing the word loop
with `process_vm_writev` removes most of that cost.

The full custom loader plus userspace exec was not benchmarked. nix-ld is the
closest structural reference, but it is not an adequate performance substitute:
the kernel still maps the main target and constructs its stack for nix-ld. A
full userspace exec must do those jobs itself. Its magnitude remains unknown
until there is a prototype; it should not be reported as a 30–100 microsecond
result or any other numeric estimate.

### Cache-miss cost

The transformed approaches separate one-time artifact work from every-exec
work:

```text
total = native exec
      + target-transform cache miss, when needed
      + loader-transform cache miss, when needed
      + cached entry mechanism, every exec
      + fspy initialization, every exec
```

Creating an artifact requires parsing and copying or reflinking the ELF, then
writing and sealing the changed image. A plain copy is proportional to file
size and can be millisecond-class for a large frontend tool. Reflinks can make
this cheap but are not portable across WSL filesystems, Docker storage drivers,
CI workspaces, and Kubernetes volumes. The transformed loader can be shared;
the target copy cannot.

No cold or cache-miss timing was obtained for the augmented-loader design. A
measured 116-microsecond partial-cold increment belonged to the nix-ld test and
must not be attributed to the augmented loader. “Partial cold” used
`POSIX_FADV_DONTNEED` on owned files while the parent runtime remained resident;
it was not a machine-cold measurement.

### Steady-state syscall handling

Both ptrace injection and loader entry detach or hand off before the target
runs. Neither adds a ptrace stop to ordinary signals or `SIGSYS` delivery. Once
the identical fspy runtime has been installed, the in-process SIGSYS handler and
shared-memory writer have the same per-syscall cost under either mechanism.
The performance difference is paid at each exec, not at each intercepted
filesystem syscall.

This remains slower per intercepted call than an LD_PRELOAD wrapper because a
real signal frame must be built and restored. That cost belongs to the SIGSYS
design and is independent of how the handler reached the new image.

## Validation results

### Augmented original loader

The prototype copied the target loader, appended one executable segment,
relocated its program-header table, changed `e_entry`, and changed a copied
target's `PT_INTERP` to the transformed loader. The entry wrote
`FSPY_LOADER_FIRST` and transferred to the saved loader entry.

Results:

- Debian trixie, native AArch64 glibc: the marker appeared first, transformed
  `/bin/cat` printed the requested file, and exited with status 0.
- Alpine 3.22, native AArch64 musl: the marker appeared first, transformed
  BusyBox `cat` printed the requested file, and exited with status 0.
- Debian x86-64 under QEMU/binfmt emulation: the marker appeared first,
  transformed `cat` completed, and exited with status 0.

The x86-64 test used a stable in-container loader pathname because the outer
QEMU/binfmt wrapper did not preserve the test's `/proc/self/fd/9` arrangement.
Native x86-64 validation of the descriptor path remains required. The kernel
open-before-CLOEXEC ordering is architecture-independent, and it was directly
validated on native AArch64.

The tests used default Docker settings. They did not add `SYS_PTRACE`, use
`--privileged`, or disable Docker seccomp. The augmented-loader mechanism uses
ordinary file, descriptor, and exec operations and requires no ptrace at all.

### nix-ld handoff reference

Unmodified nix-ld commit
[`334ce05`](https://github.com/nix-community/nix-ld/tree/334ce05c3c02938c9335723b81dd91bae03f500e)
was built and used as `PT_INTERP`. It mapped the real glibc loader, changed the
userspace `AT_BASE`, and transferred the original initial stack successfully.
Both a synthetic entry program and a real `cat` completed.

This validates the narrower “shim maps a second dynamic loader” technique, but
also exposes its drift. `/proc/PID/auxv` retained the kernel's `AT_BASE` for the
shim even after the userspace stack named the real loader. The shim remained
mapped. The augmented-original-loader design needs neither change.

### Loader hooks are too late

Entry probes compared glibc and musl behavior:

```text
glibc LD_PRELOAD constructor: HOOK, then ENTRY
glibc LD_AUDIT:               AUDIT, then ENTRY
glibc DT_PREINIT_ARRAY:       PREINIT, then ENTRY
musl preload/preinit probes:  ENTRY first
```

The glibc behavior follows its runtime-loader initialization and constructor
order in [`rtld.c`](https://github.com/bminor/glibc/blob/04e750e75b73957cf1c791535a3f4319534a52fc/elf/rtld.c#L1763-L1947)
and [`dl-init.c`](https://github.com/bminor/glibc/blob/04e750e75b73957cf1c791535a3f4319534a52fc/elf/dl-init.c#L79-L121).
Musl hands off to the program entry before its C runtime runs constructors; see
[`dynlink.c`](https://git.musl-libc.org/cgit/musl/tree/ldso/dynlink.c?id=f21a96538f78fa8e2040831b4209b35f2fb581da#n1965)
and [`__libc_start_main.c`](https://git.musl-libc.org/cgit/musl/tree/src/env/__libc_start_main.c?id=f21a96538f78fa8e2040831b4209b35f2fb581da#n59).

These hooks can run before application `main`, and on glibc before the main
ELF's raw entry in the tested cases. They cannot install fspy before the
dynamic loader's own startup syscalls, and they are not a cross-libc answer.

## Compatibility and security boundaries

Transforming a copy preserves real exec cleanup, but the kernel executes the
copy. That distinction prevents it from being a universal replacement for
ptrace.

| Area                                    | Behavior                                                                                                                                                                  | Required policy                                                                                                        |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| Threads, old mappings, signals, CLOEXEC | Real kernel exec semantics are preserved.                                                                                                                                 | Fast path is sound.                                                                                                    |
| `argv` and environment                  | Preserved when passed unchanged. `argv[0]` is caller-owned.                                                                                                               | Avoid hidden transport arguments; remove transport env keys if target visibility matters.                              |
| Original executable authorization       | Kernel `MAY_EXEC` and LSM checks apply to the transformed copy, not the requested file.                                                                                   | Do not use the fast path when policy equivalence matters.                                                              |
| setuid, setgid, file capabilities       | Credentials derive from the copy's inode, mode, owner, mount, and xattrs. An unprivileged cache cannot reproduce them. Ptrace and `no_new_privs` also suppress elevation. | Run natively without fspy or reject explicitly.                                                                        |
| SELinux, AppArmor, Landlock             | Policy observes the cache file's path, inode, and label.                                                                                                                  | Treat enforcing or path-sensitive policy as unsupported unless explicitly validated.                                   |
| IMA/EVM appraisal                       | The modified image no longer has the original appraisal identity or signature.                                                                                            | Fall back; runtime re-signing is generally unavailable.                                                                |
| `/proc/self/exe`, `current_exe()`       | Points to the transformed target copy. With full custom-loader exec it points to the loader.                                                                              | Expect resource-discovery breakage; prefer a stable on-disk cache over an anonymous memfd, but neither is transparent. |
| Kernel-saved aux vector                 | Augmented original loader retains native `AT_BASE`; target-copy path still changes `AT_EXECFN`. User-stack edits cannot update every kernel view.                         | Test tools that inspect `/proc/PID/auxv`.                                                                              |
| Execute-only file                       | Native exec can succeed without read permission; transformation cannot read it.                                                                                           | Ptrace or future kernel loader substitution.                                                                           |
| `noexec` mount                          | Copying to an executable cache can accidentally bypass the original mount decision.                                                                                       | First establish equivalent authorization or fall back; do not silently change policy.                                  |
| Executable memfd policy                 | `vm.memfd_noexec=2` can reject executable memfds.                                                                                                                         | Provide an on-disk path or fallback.                                                                                   |
| Scripts                                 | No `PT_INTERP`; CLOEXEC descriptor execution has the shebang `ENOENT` edge case.                                                                                          | Handle separately, normally by intercepting the resolved script interpreter exec or falling back.                      |
| GNU properties                          | The kernel reads properties from the interpreter. Incorrect CET/BTI/PAC metadata can crash or weaken the process.                                                         | Preserve properties and make all injected control transfers conform.                                                   |
| Self re-exec                            | `/proc/self/exe` names the copy, which can be transformed again, but the path and cache identity remain observable.                                                       | Test explicitly; do not call it transparent.                                                                           |

The kernel evidence for the most important boundaries is direct:

- [Executable open and `MAY_EXEC`](https://github.com/torvalds/linux/blob/e8f897f4afef0031fe618a8e94127a0934896aba/fs/exec.c#L911-L952)
- [Credential preparation from `bprm->file`](https://github.com/torvalds/linux/blob/e8f897f4afef0031fe618a8e94127a0934896aba/fs/exec.c#L1644-L1700)
- [`mm->exe_file` assignment](https://github.com/torvalds/linux/blob/e8f897f4afef0031fe618a8e94127a0934896aba/fs/exec.c#L1292-L1304)
- [`PR_SET_MM_EXE_FILE` capability check](https://github.com/torvalds/linux/blob/e8f897f4afef0031fe618a8e94127a0934896aba/kernel/sys.c#L2157-L2169)
- [SELinux transition based on the executable inode label](https://github.com/torvalds/linux/blob/e8f897f4afef0031fe618a8e94127a0934896aba/security/selinux/hooks.c#L2293-L2354)
- [IMA executable appraisal](https://github.com/torvalds/linux/blob/e8f897f4afef0031fe618a8e94127a0934896aba/security/integrity/ima/ima_main.c#L513-L540)

## Ranked mechanism matrix

| Mechanism                                     | Strong pre-entry boundary                                             | Coverage                                                                                                               | Works without sudo                                                                        | Exec transparency                                                                                        |                                                              Measured warm entry cost | Decision                                                      |
| --------------------------------------------- | --------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------: | ------------------------------------------------------------- |
| Augmented original `PT_INTERP` loader         | Yes                                                                   | Readable dynamic ELF                                                                                                   | Yes, with ordinary executable files and procfs                                            | Real exec cleanup, but target is a transformed copy                                                      |                                                                   Low-single-digit µs | Leading dynamic candidate; production validation remains      |
| Appended static ELF entry segment             | Yes                                                                   | Readable static ELF                                                                                                    | Yes                                                                                       | Real exec cleanup, but target is a transformed copy                                                      |                                                                   Low-single-digit µs | Lower-drift static candidate                                  |
| Added static `PT_INTERP` fspy shim            | Yes                                                                   | Synthetic static `ET_EXEC`/`ET_DYN` plus real Debian BusyBox on AArch64; `ET_EXEC`/BusyBox also under x86-64 emulation | Yes in default Docker/Colima; CLOEXEC procfd tested on native AArch64 and requires procfs | Real exec cleanup and unchanged ELF `e_entry`; static PIE load bias and interpreter-visible state change |                                                                          Not measured | Validated shared-runtime static candidate                     |
| Ptrace exec-stop injection                    | Yes                                                                   | ELF, scripts, and other successful exec formats                                                                        | Yes for an owned child where ptrace policy permits; validated in default local Docker     | Executes the original inode, but tracing suppresses privilege elevation                                  |                                  ~0.1 ms with bulk transfer; current transfer ~5.8 ms | Compatibility fallback where tracing semantics are acceptable |
| Augmented `ld.so` invoked directly            | In principle; current transformed-loader proof crashes in direct mode | Dynamic ELF accepted by that loader                                                                                    | Yes                                                                                       | Kernel identity and auxv describe `ld.so`, not target                                                    | Direct unmodified loader showed no measurable warm penalty; augmented mode unmeasured | Investigate only as an opt-in compatibility mode              |
| Generic `PT_INTERP` shim mapping real `ld.so` | Yes                                                                   | Readable dynamic ELF copy                                                                                              | Yes                                                                                       | Target copy plus extra loader mapping and kernel/userspace auxv disagreement                             |                                                                        Low tens of µs | Superseded by augmenting the original loader                  |
| Standalone fspy loader plus userspace exec    | Yes                                                                   | Potentially all readable ELF formats                                                                                   | Yes                                                                                       | Kernel identity describes fspy loader; substantial mapping drift                                         |                                                                            Unmeasured | High-complexity experiment, not preferred                     |
| Private `binfmt_misc` interpreter             | Yes                                                                   | Whatever the custom loader supports                                                                                    | Only where unprivileged user and mount namespaces are permitted                           | Classic mode rewrites invocation and runs interpreter as main image                                      |                                                                            Unmeasured | Niche; unavailable in default Docker test                     |
| Future `binfmt_misc` `L` loader substitution  | Yes                                                                   | Native dynamic ELF                                                                                                     | Registration still needs an allowed namespaced or host instance                           | Native main image, credentials, layout, and auxv                                                         |                                                    Expected near native; not measured | Ideal future path when deployed                               |
| glibc preload, audit, or preinit hook         | No, original loader runs first                                        | glibc dynamic ELF, with differing feature coverage                                                                     | Yes                                                                                       | Several secure-exec restrictions; musl behavior differs                                                  |                                                                          Not isolated | Too late for inherited SIGSYS interception                    |
| Seccomp alone                                 | No                                                                    | All exec descendants retain the filter                                                                                 | Yes                                                                                       | Filter persists, but caught signal actions do not                                                        |                                                                                   N/A | Cannot introduce post-exec code by itself                     |
| QEMU user mode                                | Yes                                                                   | Supported guest architectures and syscalls                                                                             | Usually yes when explicitly invoked                                                       | Emulator is the executed image                                                                           |                                                   ~7.76 ms for a tiny same-ISA target | Far too expensive and semantically broad                      |
| Valgrind                                      | Yes                                                                   | Supported targets                                                                                                      | Yes                                                                                       | Runs under a synthetic runtime and address space                                                         |                                        ~74 ms with no analysis; ~161 ms with Memcheck | Diagnostic tool only                                          |
| DynamoRIO-style DBI                           | Yes                                                                   | Supported targets                                                                                                      | Usually yes                                                                               | Runtime controls the process from startup                                                                |                            Published steady-state overhead; startup not isolated here | Reference implementation, not a fast path                     |

### Ptrace remains the broad compatibility reference

`PTRACE_TRACEME` produces an exec stop after the kernel has installed the new
image and before it executes an instruction. The current demo uses that exact
boundary and then detaches, so ordinary signals and SIGSYS do not return to the
supervisor. It works in default Docker for a child that requests tracing; it
does not require `--cap-add SYS_PTRACE` or `seccomp=unconfined` in the validated
environment. That is not a universal availability guarantee. Yama
`ptrace_scope=3`, container seccomp, or another LSM can deny even
`PTRACE_TRACEME`.

Unlike transformation, ptrace lets the kernel execute the originally requested
inode. It preserves that file's identity, LSM and IMA evaluation,
`/proc/self/exe`, `AT_EXECFN`, scripts, execute-only files, and the behavior of
other binary-format handlers. It does not preserve privilege-changing exec:
Linux ignores setuid, setgid, and file-capability elevation for a traced task,
and fspy's `no_new_privs` does the same independently. See
[`execve(2)`](https://man7.org/linux/man-pages/man2/execve.2.html). Retain ptrace
where these tracing semantics are acceptable, not as a claim of universal
native equivalence.

The first ptrace optimization is to replace the
`PTRACE_POKEDATA` loop in `inject_demo` with a single `process_vm_writev` or a
small number of bulk writes, while preserving the ptrace-controlled remote
mapping and register restoration. The measured order-of-magnitude improvement
is large enough to do before judging ptrace by the current prototype.

### Directly executing an augmented dynamic loader

glibc and musl loaders can be invoked as programs:

```text
/lib64/ld-linux-x86-64.so.2 PROGRAM [ARGUMENTS...]
/lib/ld-musl-aarch64.so.1 PROGRAM [ARGUMENTS...]
```

Executing an augmented copy of the loader should give fspy the first instruction
after a real exec, then reuse the loader's existing direct-invocation path to
map the requested program. It would avoid both target transformation and a
custom userspace ELF mapper. Direct invocation of unmodified glibc `ld.so` had
no measurable warm cost in the tiny-target benchmark.

The current AArch64 augmented-loader proof is not yet valid in this mode. When
executed as `PT_INTERP`, it prints the marker, hands off, and completes. When
executed as the main ELF, it prints the same marker and then exits on `SIGSEGV`
(status 139). The original unmodified loader succeeds in direct mode. This
shows that the scratch program-header relocation or entry handoff violates a
different glibc self-bootstrap assumption when `ld.so` is the main image. Do
not transfer either the `PT_INTERP` compatibility result or unmodified-loader
timing to this variant until that failure is understood.

This is a useful experiment or opt-in compatibility mode, but it is not
transparent. The kernel executes the loader: `/proc/self/exe`, `AT_EXECFN`,
credentials, LSM checks, IMA, process accounting, and the initial aux vector
describe the loader. The requested target is only data mapped later. `argv[0]`,
secure-execution behavior, `$ORIGIN`, loader command-line options, and
self-location logic can also differ across libc versions. Static binaries and
scripts are outside this path.

### Dynamic-loader hooks

The following mechanisms all run after the original dynamic loader has begun:

- `LD_PRELOAD` constructors and interposed functions;
- `LD_AUDIT` callbacks;
- `DT_PREINIT_ARRAY`, `.init`, and `.init_array`;
- an early `DT_NEEDED` library;
- `__libc_start_main` interposition.

They can be useful if “before `main`” is enough. They cannot protect the
loader's own syscalls with an in-process SIGSYS handler. Environment-based
hooks are also suppressed or restricted for secure execution, and musl does
not reproduce glibc's entry ordering or audit interface.

### Why seccomp cannot finish the job alone

Seccomp filters survive exec, which makes them useful for selecting the
syscalls fspy wants to intercept. Caught signal dispositions do not survive
exec. Therefore an inherited `SECCOMP_RET_TRAP` filter reaches the default
`SIGSYS` action until new-image code installs a handler.

Other seccomp returns do not create an in-process entry point:

- `SECCOMP_RET_USER_NOTIF` sends the syscall to another process and has the
  cross-process cost this design is trying to avoid.
- `SECCOMP_RET_TRACE` requires ptrace and creates supervisor stops.
- `SECCOMP_RET_ERRNO`, `LOG`, `KILL`, and `ALLOW` never run target code.

A filter also cannot make mappings, signal actions, or descriptors survive the
address-space replacement. It is part of the final design, not an alternative
to pre-entry installation.

### Dynamic instrumentation and emulation

QEMU user mode, Valgrind, DynamoRIO, and similar runtimes demonstrate that a
userspace loader can take control before application code without sudo. They
also virtualize much more than fspy needs.

The local same-architecture QEMU TCG test took about 7.763 milliseconds versus
149 microseconds natively for the tiny dynamic target. Valgrind 3.24 took about
73.7 milliseconds with `--tool=none` and 160.9 milliseconds with Memcheck.
These systems keep charging translation or instrumentation cost after entry.

DynamoRIO reports roughly 11% average whole-program overhead for its base
runtime in an older SPEC CPU2006 evaluation. Persistent caches reduce its
startup work but require a large code cache and do not make short-lived process
startup free. See the [DynamoRIO transparency paper](https://www.burningcutlery.com/derek/docs/transparency-VEE12.pdf),
[QEMU user-mode documentation](https://www.qemu.org/docs/master/user/main.html),
and [Valgrind's startup explanation](https://valgrind.org/docs/manual/faq.html#faq.attach).

An early-detach version of a DBI runtime would stop paying steady-state
instrumentation cost, but at that point it has become the same full userspace
exec problem described above.

### Mechanisms that only observe

Audit, fanotify, inotify, perf events, uprobes, and most eBPF tracing can observe
an exec or an instruction boundary. They do not install callable code in the
new address space. Many system-wide variants also require capabilities or host
configuration. `userfaultfd` can stall selected memory faults but offers no
general first-instruction callback and is restricted in hardened environments.

No ordinary mapping or caught signal handler survives `exec`. There is no
descriptor flag, `madvise` option, vDSO hook, or inherited function pointer that
turns into a post-exec callback.

Code in `fork`, `vfork`, `clone`, `posix_spawn`, or Rust `pre_exec` hooks is on
the wrong side of the boundary. It can prepare descriptors, filters, and the
exec request, but it runs in the old image before signal reset, CLOEXEC closure,
thread teardown, and address-space replacement. A custom kernel module or LSM
could add the desired hook, but installing one requires administrative control
and is outside the no-sudo requirement.

## `binfmt_misc`: current limitations and future answer

### Private instances on deployed kernels

Linux 6.7 added user-namespace ownership for `binfmt_misc` instances in commit
[`21ca59b`](https://github.com/torvalds/linux/commit/21ca59b365c091d583f36ac753eaa8baf947be6f).
In principle, an unprivileged process can create a user and mount namespace,
mount its own `binfmt_misc`, and register an interpreter without changing the
host instance.

This does not require host root only when the environment permits all of those
namespace operations. The default local Docker/Colima test did not:

```text
$ unshare -Ur true
unshare: unshare failed: Operation not permitted
$ grep '^Seccomp:' /proc/self/status
Seccomp: 2
```

Inside the Colima VM, creating a user namespace alone worked, but establishing
the requested UID mapping did not. CI providers and Kubernetes policies vary;
many disable unprivileged user namespaces, namespace creation through seccomp,
or new mounts. Moving the whole workload into an extra user/mount namespace can
also be an observable behavior change.

Classic `binfmt_misc` dispatch executes the registered interpreter as the main
image and modifies its invocation. It therefore needs the full userspace-loader
work and retains most of that design's identity drift. It is not a better
deployed-kernel default than an explicit loader exec.

### Post-7.2 loader substitution

Current upstream development after Linux 7.2 contains three relevant additions:

- [`L`, loader substitution](https://github.com/torvalds/linux/commit/83cd3989ba0971693461088d35142ad52d862135):
  the kernel loads the matched native ELF as the real main image and substitutes
  the registered interpreter for its `PT_INTERP`.
- [`T`, transparent interpreter mode](https://github.com/torvalds/linux/commit/75e536852f9a5f1880091d58f46cdf2fce2101b4):
  the kernel preserves argv and executable identity while a custom interpreter
  maps the executable from `AT_EXECFD`; the address-space layout still differs.
- [`B`, BPF-selected handlers](https://github.com/torvalds/linux/commit/ceb912149e5e60fbb1c762603f8c4ce257b97501):
  a BPF handler can inspect the executable and choose an interpreter and mode,
  including loader substitution, per exec.

The upstream [development documentation](https://github.com/torvalds/linux/blob/2709dd5ae32f0828f386327c76bba9f39f63a1c6/Documentation/admin-guide/binfmt-misc.rst#L332-L376)
describes `L` as a native exec: argv is untouched, credentials and `AT_SECURE`
derive from the target, the target occupies the normal main-image address, and
the substitute runs where the original loader would have run. This is exactly
fspy's desired dynamic boundary.

With `L`, fspy would register an augmented loader and no longer patch or copy
each dynamic target. The kernel would preserve `/proc/self/exe`, target LSM and
IMA checks, set-ID semantics, `AT_EXECFN`, `brk`, and native mappings. Static
ELFs and scripts deliberately fall back to their ordinary handlers. This is the
kernel mechanism's behavior; fspy's current pre-exec `no_new_privs` setting
would still suppress privilege elevation before `L` is reached.

The feature does not eliminate deployment constraints. The kernel must include
the new code, `binfmt_misc` must be available, and fspy must be allowed to
register in a host or private instance. BPF selection additionally requires
the relevant BPF configuration and authority; it is not a general no-sudo API
for an ordinary process. Use `L` as a feature-detected future fast path, not a
current baseline.

## Proposed implementation sequence

### Phase 1: fix the ptrace baseline

Replace word-at-a-time `PTRACE_POKEDATA` payload transfer with
`process_vm_writev`, retaining a small ptrace fallback if the bulk call is
blocked. Benchmark the actual relocated blob and full fspy initialization.
This produces a usable compatibility path before introducing transformation.

### Phase 2: build an ELF transformation library

Create a supervisor-only crate that uses the existing workspace `goblin`
dependency for parsing and explicit checked writes for transformation. Keep it
separate from the target-none runtime. It should:

- identify ELF class, byte order, machine, type, loader, and static PIE;
- reject malformed, overlapping, or unsupported layouts;
- preserve all program headers and GNU properties;
- relocate the program-header table when no safe slots exist;
- patch or append `PT_INTERP` strings;
- append aligned segments for loader and static transformations;
- return the saved entry and every address needed by the handoff stub;
- verify the produced image by parsing it again and checking load ranges;
- use atomic cache publication and immutable permissions.

Extend `fspy_blob` only if its abstraction remains “processed position-independent
runtime image.” Do not teach the injected runtime to parse general ELF files.

### Phase 3: augment original loaders

Generate architecture-specific handoff stubs around the existing Rust
target-none runtime. The transform supplies the original entry as generated
code or immutable blob data, rather than hard-coding offsets as the scratch
prototype did.

Before Rust runs, the stub must derive the augmented loader's ASLR bias and
apply the compact relative-relocation table prepared by the supervisor. Keep
this loop independent of libc, the stack allocator, and any relocated Rust
global it is about to initialize.

Start with:

- x86-64 glibc and musl;
- AArch64 glibc and musl;
- direct-branch handoff where possible;
- CET/IBT and BTI test binaries;
- W^X mapping permissions, never an RWX final segment.

The fspy initialization path should map the existing shared-memory channel by
name from the initial environment, install the final virtualized SIGSYS action,
remove or hide fspy transport environment entries as required, and jump to the
saved loader entry.

### Phase 4: connect descendant exec interception

The initial process can be launched as a transformed artifact directly; its
`pre_exec` hook installs the seccomp filter before the real exec. Descendant
`execve` and `execveat` calls are trapped in-process. The handler must not build
ELFs itself. It should make an allocation-free request to the supervisor,
receive or reopen the selected immutable artifact, open the transformed loader,
and reissue exec through the filter's constant-cookie path.

The request protocol must handle:

- concurrent exec attempts from multiple processes;
- target and loader cache misses;
- changed mount namespaces and paths visible only to the target;
- `execveat` descriptors and `AT_EMPTY_PATH`;
- native errno preservation when the original exec would fail;
- cleanup and descriptor restoration when transformed exec fails;
- recursion when executing fspy artifacts themselves.

Artifact creation in the supervisor avoids performing heap allocation, complex
parsing, or filesystem publication inside a signal handler. A path-based reply
is simplest only when supervisor and target share a mount namespace. An
inherited Unix-domain control socket can transfer descriptors with
`SCM_RIGHTS`, but its lifetime and async-signal-safe direct-syscall protocol
must be designed explicitly. Shared memory alone cannot transfer a descriptor.

### Phase 5: compatibility gates

Before selecting transformation, inspect the requested executable and current
environment. Select ptrace, native uninstrumented execution, or an explicit
unsupported result according to the reason:

- ptrace can cover non-ELF files, scripts, unreadable or execute-only files,
  unsupported ELF layouts, and cache failures when local ptrace policy permits;
- setuid, setgid, and file-capability files require native uninstrumented
  execution or rejection because tracing also suppresses elevation;
- strict identity, IMA appraisal, and LSM-sensitive cases need an explicit
  policy based on whether tracing itself is allowed and semantically acceptable.

Add explicit workload coverage for Node.js, esbuild, oxlint, Vitest browser
mode, Chromium with `chromiumSandbox: true`, Go, static musl, shell scripts,
self-reexec, Docker, WSL2, representative CI, and Kubernetes. The present
loader proof establishes mechanism feasibility, not that compatibility suite.

### Phase 6: detect future kernel support

When loader substitution is available and a usable private or host
`binfmt_misc` instance exists, prefer it for dynamic ELF. Reuse the same
augmented-loader artifact and runtime, but let the kernel substitute it for the
original loader. Keep transformation and ptrace for older or constrained
environments.

## Reproduction inventory

The durable ptrace reproduction is
[`crates/inject_demo`](../crates/inject_demo/README.md). It builds the Rust
target-none payload as an artifact dependency, injects it at the exec stop, and
demonstrates in-process `openat` SIGSYS handling.

The loader experiments used short scratch programs rather than production
code. A reproduction needs these operations:

1. Copy the target's actual `PT_INTERP` loader.
2. Assemble a position-independent entry stub that writes a marker, restores
   entry state, and branches to the loader's saved `e_entry`.
3. Copy all original program headers, append a new RX `PT_LOAD` containing the
   enlarged table and stub, then change `e_phoff`, `e_phnum`, and `e_entry`.
4. Copy the target and replace its `PT_INTERP` with the transformed loader path
   or `/proc/self/fd/N`.
5. Run a target that performs dynamic-loader filesystem work, such as
   `/bin/cat`, and verify marker ordering and exit status.
6. Repeat for glibc and musl on x86-64 and AArch64, inspecting GNU property
   notes before and after.

The scratch transformer was ELF64 little-endian only and its assembly stubs
hard-coded the difference between old and new entry addresses. Those are
deliberate prototype limits, not an implementation template. Production code
must derive all values, validate arithmetic and ranges, support each declared
architecture explicitly, and fail closed on unknown layouts.

## Open validation work

The research establishes a viable alternative, but these items remain before a
production decision:

- native x86-64 `/proc/self/fd/N` loader execution;
- the direct-mode `SIGSEGV` in the augmented AArch64 glibc loader;
- an actual self-relocating Rust fspy blob appended to glibc and musl loaders;
- full SIGSYS installation and shared-memory reporting through that entry;
- static-PIE and fixed `ET_EXEC` transformation across both architectures;
- cold transform and cache-miss measurements on overlayfs, WSL, and CI filesystems;
- target-copy identity impact on the named frontend and browser workloads;
- behavior under SELinux, AppArmor, IMA, `noexec`, and memfd no-exec policy;
- descendant exec request/response transport across mount namespaces;
- failure-path errno and descriptor equivalence;
- detection and validation of the future `binfmt_misc` loader-substitution API.

Until those are complete, the recommendation is architectural: build the
augmented-loader candidate behind conservative gates, retain optimized ptrace
for cases whose semantics it preserves, and use native uninstrumented execution
or an explicit unsupported result for privilege-changing exec.
