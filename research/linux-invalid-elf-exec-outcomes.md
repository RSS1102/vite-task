# Invalid ELF execution on Linux: errno, forced `SIGSEGV`, or a later fault

Status: source-audited and experimentally checked

Last updated: 2026-08-24

Source baseline: Linux 7.2

Experiment baseline: Linux 6.8.0-100-generic, AArch64

## Conclusion

Linux does not fully validate an ELF and then atomically commit it. ELF loading
has an irreversible boundary. The observable result depends primarily on which
side of that boundary detects the problem:

| Detection point                                                                | What the caller observes                                                                                                                                                                                                                                            |
| ------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Before exec's point of no return                                               | `execve` or `execveat` returns `-1`, with an errno such as `ENOEXEC`, `EIO`, `ELIBBAD`, or an error from opening `PT_INTERP`. The old program continues.                                                                                                            |
| After the point of no return, while the kernel is still constructing the image | The syscall does not return to the old program. Unless another fatal signal is already pending, the kernel terminates the process with `SIGSEGV`. The loader's internal `EINVAL`, `ENOMEM`, `EFAULT`, and similar values are not observable as syscall errors.      |
| After the kernel considers exec successful                                     | Execution begins in the new image or its interpreter. A bad entry point or incomplete mapping can then cause an ordinary user-mode `SIGSEGV`, `SIGILL`, or `SIGBUS`; a dynamic loader may instead print an error and exit. This is not a failed exec in the kernel. |

The dividing line in the ELF loader is the call to `begin_new_exec()`. More
precisely, it is the assignment to `bprm->point_of_no_return` inside that
function. A credentials error at the very start of `begin_new_exec()` can still
return normally; after the flag is set, failures are fatal. The kernel source
describes this as the point at which the old executable is being flushed and
errors can no longer be reported to it.

Sources:

- [`load_elf_binary()` performs its early ELF checks and then calls `begin_new_exec()`](https://github.com/torvalds/linux/blob/v7.2/fs/binfmt_elf.c#L832-L1012).
- [`begin_new_exec()` sets `point_of_no_return`](https://github.com/torvalds/linux/blob/v7.2/fs/exec.c#L1104-L1135).
- [The exec error path forces fatal `SIGSEGV` after that point](https://github.com/torvalds/linux/blob/v7.2/fs/exec.c#L1791-L1805).
- [`force_fatal_sig()` uses the default disposition and `SI_KERNEL`](https://github.com/torvalds/linux/blob/v7.2/kernel/signal.c#L1656-L1667).

This boundary, rather than a broad notion of whether an ELF is “valid”, is the
reliable model.

## What is documented and what is only current implementation

Four sources describe different contracts. They should not be treated as one
exhaustive specification.

| Source                                                                                                                                        | What it documents                                                                                                                                              | What it does not document                                                                                                                   |
| --------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| [`execve(2)`](https://man7.org/linux/man-pages/man2/execve.2.html) and [`execveat(2)`](https://man7.org/linux/man-pages/man2/execveat.2.html) | Broad error classes such as `ENOEXEC`, `ELIBBAD`, and `EIO`; path, permission, argument, and resource errors; and fatal `SIGSEGV` after the point of no return | Which individual ELF field is checked at which stage; a complete mapping from malformed bytes to errno; which malformed files Linux accepts |
| [System V ELF ABI](https://refspecs.linuxfoundation.org/elf/gabi4+/ch5.pheader.html) and architecture supplements                             | What a conforming ELF must contain, including segment ordering, `p_filesz <= p_memsz`, offset/address congruence, alignment, and `PT_INTERP` constraints       | The errno for violating a rule; whether Linux validates it; whether detection occurs before or after commit                                 |
| [Linux-specific ELF documentation](https://docs.kernel.org/7.2/userspace-api/ELF.html)                                                        | A small set of intentional Linux choices, such as using the first `PT_INTERP`, the last `PT_GNU_STACK`, and the interpreter's last `PT_GNU_PROPERTY`           | The complete Linux ELF loader algorithm                                                                                                     |
| Linux source                                                                                                                                  | The exact behavior of one kernel version and architecture                                                                                                      | A stable userspace promise that later kernels must preserve for malformed input                                                             |

The documentation is not fully consistent. The generic ABI says `PT_INTERP`
must occur at most once, and the current `execve(2)` man page lists multiple
`PT_INTERP` entries as `EINVAL`. Linux's ELF guide says the first entry is used
and later entries have been ignored since Linux 2.4.11. Linux 7.2 source follows
the ELF guide. This conflict is a strong reason not to clone malformed-input
behavior from either the man page or one source snapshot.

The stable contract is therefore coarse:

- A format error may return `ENOEXEC` while the old image still exists.
- Interpreter and I/O failures may return their documented errno.
- A failure after commit terminates the process instead of returning.
- Conforming ELF structure comes from the ABI, but Linux may accept files that
  do not conform.

The exact boundary for each malformed field is useful for understanding and
testing current Linux. It should not become fspy's public compatibility
contract.

## Recommended policy for the custom loader

The custom loader should implement a strict fast path, not a second
bit-compatible Linux ELF loader.

### Delegate anything outside the supported subset

The intercepted exec path should produce one of two decisions:

```rust
enum ExecPlan {
    Custom(VerifiedLoadPlan),
    OriginalExec,
}
```

Choose `Custom` only after constructing a complete, checked load plan for a
supported ELF. Choose `OriginalExec` for every malformed, ambiguous, unusual,
or unsupported input. Reissue the original syscall through the allowed syscall
gateway with the original path, descriptor, flags, `argv`, and `envp`.

This delegation preserves behavior that is impossible to reproduce reliably:

- exact errno and point-of-no-return behavior for malformed ELF files;
- scripts and recursive shebang handling;
- configured `binfmt_misc` handlers, including QEMU;
- execute-only files that fspy cannot read;
- set-ID and file-capability transitions;
- architecture-specific ELF extensions; and
- valid but unusual files that the first implementation does not support.

A parse failure is therefore not an error returned by fspy. It is a request to
let the kernel execute the original request. If ptrace injection remains
available as a secondary mechanism, it can inject after that delegated exec.
Otherwise compatibility takes precedence over tracing that process.

The same rule applies to fspy preparation failures. If building the custom
loader state fails before commit, fall back to the original exec rather than
exposing an fspy-specific `ENOMEM`, `EIO`, or `EINVAL` to the application.

### Validate the custom fast path strictly

`VerifiedLoadPlan` should guarantee every property the userspace mapper relies
on, even when Linux happens not to reject the corresponding malformed field:

- supported ELF class, byte order, machine, type, and ABI version;
- exact ELF and program-header sizes;
- checked table, file-offset, virtual-address, and size arithmetic;
- complete program-header and segment reads;
- ABI-conforming `PT_LOAD` order, page congruence, and `p_filesz <= p_memsz`;
- a supported interpreter and supported architecture properties;
- internally consistent, non-overflowing load ranges and a runtime reservation
  strategy that does not overwrite unrelated mappings; and
- an immutable or revalidated source image between planning and mapping.

Do not parse or validate section headers. They are not part of the execution
image. Do not add strict checks that the mapper does not need, such as requiring
the entry to lie in a `PF_X` segment, unless the production loader deliberately
wants a smaller compatibility subset. A rejected unusual file can always take
`OriginalExec`.

Follow documented Linux idiosyncrasies only when they are easy and required by
real files. It is also correct for the first version to delegate duplicate
`PT_INTERP`, duplicate `PT_GNU_STACK`, or unsupported GNU properties rather
than emulate their first/last-selection rules.

### Preserve the commit boundary

Before executing `fspy_loader`, all operations that may need to report an
error must be complete: target resolution, policy checking, parsing, source
snapshotting, load-plan construction, shared-memory attachment, and loader
state preparation.

The final `execve` of `fspy_loader` still uses the application's original
`argv` and `envp`. The kernel therefore performs its normal pointer and size
checks and can return `EFAULT` or `E2BIG` before committing the loader. If that
exec returns for any other reason, fspy can still delegate to the original
exec request.

After `fspy_loader` starts, nothing can return to the old program. The loader
should recheck all arithmetic and file bounds for defense in depth. If an
unexpected load failure remains, terminate with fatal `SIGSEGV` after restoring
the default disposition. This keeps the same externally meaningful boundary as
the kernel: errors before commit return, and errors after commit are fatal.
Debug builds may print a diagnostic before raising the signal.

### Do not mistake `AT_EXECVE_CHECK` for ELF validation

When available, call `execveat` with `AT_EMPTY_PATH | AT_EXECVE_CHECK` on the
already-open target descriptor to apply the kernel's non-destructive execution
policy check. The [kernel documentation](https://docs.kernel.org/7.2/userspace-api/check_exec.html#at-execve-check)
explicitly says that this check ignores file format and interpreter
dependencies. It cannot replace the ELF parser.

It is still valuable because executing `fspy_loader` instead of the target
would otherwise bypass target-specific execute permission, `noexec` mount, and
some security-policy checks. Use the same descriptor for checking and planning
to avoid a pathname race.

`AT_EXECVE_CHECK` is not a complete solution:

- older kernels do not support it;
- it intentionally does not parse ELF, shebangs, or dependencies;
- the current implementation does not invoke every LSM hook used by a real
  exec; and
- its temporary deny-write protection ends when the check returns, so it does
  not freeze target contents until the custom loader maps them.

For production, the safe default on a kernel without `AT_EXECVE_CHECK` is to
delegate to the original exec unless fspy has an explicitly scoped mode for a
controlled environment. A best-effort `faccessat2(X_OK)` check is not an exact
replacement for exec-specific LSM and credential checks.

The load plan must also be protected from concurrent file mutation. Prefer an
immutable snapshot, such as a sealed memfd containing the bytes the loader will
map. Keeping only a readable descriptor does not reproduce the kernel's
`exe_file_deny_write_access()` interval. Sealing prevents changes after the
copy; it does not make a copy from a concurrently modified source atomic.
Linux exposes no general userspace equivalent of holding the kernel exec
loader's deny-write state. A production implementation must either constrain
the source to immutable or trusted storage, use a supported file-lease strategy
where available, detect an unstable copy and delegate, or accept and document
this race.

### Concrete preflight sequence

Use this order for the first implementation. `OriginalExec` means closing all
temporary fspy resources and reissuing the untouched original syscall.

| Step | Operation                                                                                                                                                                           | Failure or unsupported result                                                                                                                   |
| ---- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| 1    | Capture the original `execve` or `execveat` request without changing `argv`, `envp`, path bytes, descriptor, or flags.                                                              | `OriginalExec` if the request cannot be inspected safely.                                                                                       |
| 2    | Resolve and open the requested file with the original `dirfd`, path, `AT_EMPTY_PATH`, and `AT_SYMLINK_NOFOLLOW` semantics. Obtain a readable descriptor for a possible custom load. | `OriginalExec`. The kernel then produces the authoritative path or descriptor error.                                                            |
| 3    | Read only the first `BINPRM_BUF_SIZE` bytes as an advisory classifier. Linux defines this buffer as 256 bytes.                                                                      | A shebang, non-ELF magic, foreign architecture, or unrecognized format takes `OriginalExec`. This preserves shebang and `binfmt_misc` behavior. |
| 4    | Inspect file metadata needed to exclude unsupported privilege transitions. Reject set-ID, file-capability, execute-only, and otherwise unsupported candidates from the custom path. | `OriginalExec`.                                                                                                                                 |
| 5    | Run `execveat(target_fd, "", check_argv, check_envp, AT_EMPTY_PATH                                                                                                                  | AT_EXECVE_CHECK)` on the original target descriptor. This is the target execution-policy check.                                                 | `OriginalExec` on denial or when the kernel does not support the flag, unless an explicitly configured controlled-environment mode applies. |
| 6    | Copy the target into private handoff storage and classify the completed snapshot again. Use stability checks around the copy. The complete per-exec handoff is sealed in step 9.    | `OriginalExec` if the copy is unstable or no longer matches the advisory classification.                                                        |
| 7    | Parse the target ELF and construct its complete checked load plan. Select the first-version supported subset strictly.                                                              | `OriginalExec` for every parse error, overflow, unsupported property, or unusual layout.                                                        |
| 8    | If the target has `PT_INTERP`, resolve and open that path, run `AT_EXECVE_CHECK` on the interpreter descriptor, snapshot it, and construct its checked load plan.                   | `OriginalExec` for any failure or unsupported interpreter. Do not recursively process a `PT_INTERP` found inside the interpreter.               |
| 9    | Build and seal the complete loader handoff. It contains the loader executable, immutable target/interpreter bytes, checked plans, target identity, and fspy configuration.          | `OriginalExec`.                                                                                                                                 |
| 10   | Execute the prepared `fspy_loader` with the application's original `argv` and `envp`.                                                                                               | If the loader exec syscall returns, discard the handoff and take `OriginalExec`; the old image still exists.                                    |

The read in step 3 is only a routing hint. Never return an error based on it.
Step 6 rechecks the immutable bytes that the loader will actually consume.

#### `AT_EXECVE_CHECK` ordering for shebangs

Kernel exec policy applies to the requested script before the kernel selects
its shebang interpreter. A userspace implementation that interprets a script
must therefore use this semantic order:

1. Open the script.
2. Run `AT_EXECVE_CHECK` on the script descriptor.
3. Read and parse the shebang.
4. Resolve and open the interpreter.
5. Apply the interpreter's normal execution checks.
6. Continue recursive format handling.

Checking only the interpreter would bypass execution policy on the script.
Checking the script after parsing does not necessarily change the result, but
it reverses the kernel's permission-before-format error precedence and lets
untrusted content influence work before its execution policy is accepted.

The first implementation should not handle shebangs. It may inspect the first
two bytes before `AT_EXECVE_CHECK` solely to choose `OriginalExec`. In that
case, fspy skips its own check because the delegated kernel exec will check the
script and every interpreter in the correct order. This advisory sniff is not
semantic shebang parsing.

The same rule applies to ELF: an optional cheap magic/architecture sniff may
route obvious non-candidates to the kernel, but a candidate target receives
`AT_EXECVE_CHECK` before fspy trusts or fully parses its contents.

Pass the original `argv` and `envp` to `AT_EXECVE_CHECK`. This makes the check
observe the same argument validity, size limits, and security context as the
requested exec. It traverses the arrays a second time when the final loader
exec succeeds, but correctness is the better first-version tradeoff. Consider
a fixed minimal pair only after benchmarking and verifying that no supported
policy consumes argument or environment context.

### Handoff produced for `fspy_loader`

Prefer one sealed, per-exec memfd containing both the loader ELF and its input:

```text
+---------------------------+
| fspy_loader ELF           |  Kernel executes this portion
+---------------------------+
| aligned handoff header    |
+---------------------------+
| target ImagePlan          |
| target SegmentPlan[]      |
| target ELF bytes          |
+---------------------------+
| interpreter ImagePlan?    |
| interpreter SegmentPlan[]?|
| interpreter ELF bytes?    |
+---------------------------+
| target identity + execfn  |
| fspy runtime configuration|
+---------------------------+
```

Align each embedded ELF image to the system page size. Then an original
segment offset retains its page offset when the loader maps
`handoff_image_offset + p_offset`.

Create the memfd without `CLOEXEC`. Before sealing it, patch a dedicated
bootstrap slot in the loader image with the memfd number and the handoff range:

```rust
#[repr(C)]
struct Bootstrap {
    abi_version: u32,
    handoff_fd: i32,
    handoff_offset: u64,
    handoff_len: u64,
}
```

The loader reads this mapped bootstrap directly. It does not need an added
argument, environment variable, fixed descriptor number, `/proc`, or a scan of
the descriptor table. Execute the memfd with
`execveat(loader_fd, "", original_argv, original_envp, AT_EMPTY_PATH)`. The
non-`CLOEXEC` descriptor survives into the loader and is closed before control
reaches the target.

Embedding the images makes the executed bytes immutable, but their mappings
are backed by the fspy memfd rather than the original executable in
`/proc/PID/maps`. `/proc/PID/exe` also continues to identify `fspy_loader`.
These are custom-loader compatibility drifts, not handoff-format details. If
preserving original VMA backing identity becomes necessary, the loader must map
the original descriptors and accept or separately solve the mutation race.

The variable-sized handoff should use offsets from the handoff base, never raw
pointers. A representative semantic layout is:

```rust
#[repr(C)]
struct PreparedExec {
    abi_version: u32,
    architecture: Architecture,
    page_size: u32,
    target: ImagePlan,
    interpreter: OptionOffset<ImagePlan>,
    original_execfn: ByteSpan,
    target_comm: ByteSpan,
    fspy: FspyPlan,
}

#[repr(C)]
struct ImagePlan {
    bytes: ByteSpan,
    elf_type: ElfType,
    entry: u64,
    phdr_virtual_address: u64,
    phnum: u32,
    maximum_alignment: u64,
    segments: SliceSpan<SegmentPlan>,
    properties: ArchitectureProperties,
}

#[repr(C)]
struct SegmentPlan {
    image_offset: u64,
    virtual_address: u64,
    file_size: u64,
    memory_size: u64,
    protection: SegmentProtection,
    alignment: u64,
}
```

These are design shapes, not final Rust types. The wire representation must
avoid Rust layout-dependent enums and `Option`. Use fixed-width integer tags,
checked offsets and lengths, an explicit byte order, and an ABI version.

Do not put absolute load addresses in the handoff. The preflight validates the
relative layout. `fspy_loader` chooses ASLR load biases and reserves address
ranges in the new process.

The original `argv` and `envp` do not need to be serialized. The kernel builds
them on the loader's initial stack because the final loader exec receives the
original arrays unchanged. The handoff must contain values that the kernel's
loader stack cannot supply correctly for the target:

- the original target `AT_EXECFN` string;
- target and interpreter program-header metadata;
- target entry, interpreter base, and target-specific auxiliary-vector values;
- executable-stack and architecture-property decisions;
- target `comm`/identity information needed by fspy; and
- shared-memory and SIGSYS runtime configuration.

The loader consumes the handoff in this order:

1. Validate the already-mapped `Bootstrap` without issuing an intercepted
   syscall. The inherited seccomp filter is active and exec has reset the old
   SIGSYS disposition.
2. Use the seccomp-allowed internal syscall gateway to inspect and map the
   handoff, then validate the memfd seals, ABI version, and every span.
3. Attach fspy shared memory through the internal gateway and install the
   virtualized SIGSYS machinery before issuing any ordinary intercepted
   syscall. Loader-internal filesystem operations must remain bypassed so they
   are not reported as target accesses.
4. Read the kernel-created loader stack and preserve the original `argv`,
   `envp`, and reusable auxiliary values such as random bytes and hardware
   capabilities.
5. Reserve address ranges, choose load biases, and map target and interpreter
   segments from the embedded immutable images.
6. Zero BSS tails, establish the target program break, and apply required
   architecture setup.
7. Build the target's initial stack with corrected `AT_PHDR`, `AT_PHENT`,
   `AT_PHNUM`, `AT_BASE`, `AT_ENTRY`, and `AT_EXECFN` values.
8. Set the target process name, close the handoff descriptor, and remove the
   loader mappings through a small surviving trampoline.
9. Enter the interpreter when `PT_INTERP` exists; otherwise enter the target's
   `e_entry`.

Any validation or mapping failure in this loader sequence is post-commit and
must terminate rather than attempt to return.

### Practical first implementation

The first production increment should support ordinary native, readable,
non-privileged ELF files with conventional `PT_LOAD` layouts and either no
interpreter or a recognized glibc/musl interpreter. It should delegate:

- every non-ELF file;
- every parser or validation failure;
- unsupported machines and program-header features;
- execute-only, set-ID, or file-capability executables;
- files whose execution policy cannot be checked safely; and
- any failure to create immutable loader input.

This gives fspy a small load path that can become more capable incrementally.
Each later compatibility addition expands `VerifiedLoadPlan`; it does not add
case-by-case guesses to a synthetic errno table.

## Invalidity detected before commit: errno

All checks in this section run before `point_of_no_return`. The errno values are
the Linux 7.2 common ELF loader's behavior. Architecture hooks may return an
architecture-specific error.

### Main ELF header and program-header table

The common loader returns `ENOEXEC` for these cases:

- The first four bytes are not the ELF magic.
- `e_type` is neither `ET_EXEC` nor `ET_DYN`.
- `elf_check_arch()` rejects the file, commonly because `e_machine` is wrong.
- The file cannot be memory-mapped by the loader.
- `e_phentsize` is not the kernel's program-header size.
- `e_phnum` is zero or the complete program-header table exceeds 65,536 bytes.
- The program-header table cannot be read in full from `e_phoff`.
- Allocation or another failure inside `load_elf_phdrs()` causes it to return
  null. `load_elf_binary()` initializes its result to `ENOEXEC`, so this helper
  does not preserve a more specific error for the main ELF.

See [the initial checks](https://github.com/torvalds/linux/blob/v7.2/fs/binfmt_elf.c#L855-L871)
and [`load_elf_phdrs()`](https://github.com/torvalds/linux/blob/v7.2/fs/binfmt_elf.c#L520-L552).
The 65,536-byte limit is version-sensitive: older kernels also limited the
table to `ELF_MIN_ALIGN`, commonly one 4 KiB page.

These checks are not a complete validation of `Elf_Ehdr`. In particular, the
common loader checks only the four magic bytes directly. `elf_check_arch()` is
architecture-defined:

- Linux 7.2 [AArch64](https://github.com/torvalds/linux/blob/v7.2/arch/arm64/include/asm/elf.h#L94-L98)
  and [x86-64](https://github.com/torvalds/linux/blob/v7.2/arch/x86/include/asm/elf.h#L142-L147)
  check `e_machine`, not all of `EI_CLASS`, `EI_DATA`, and `EI_VERSION`.
- [RISC-V also checks `EI_CLASS`](https://github.com/torvalds/linux/blob/v7.2/arch/riscv/include/asm/elf.h#L34-L39).

The AArch64 experiment successfully executed otherwise valid files after
changing `EI_CLASS`, `EI_DATA`, `EI_VERSION`, `e_version`, or `e_ehsize` to
inconsistent values. Those fields must not be treated as universally validated
by `execve`.

### `PT_INTERP` before commit

The kernel opens and inspects the ELF interpreter before discarding the old
image. These failures therefore return to the caller:

| Invalidity                                                                                            | Errno                                                                            |
| ----------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| `p_filesz < 2` or `p_filesz > PATH_MAX`                                                               | `ENOEXEC`                                                                        |
| Interpreter bytes cannot be read in full from the main ELF                                            | `EIO`, or the underlying read error                                              |
| The final byte in the declared interpreter string is not NUL                                          | `ENOEXEC`                                                                        |
| The interpreter path cannot be opened                                                                 | The path/open error, such as `ENOENT`, `EACCES`, `ELOOP`, `EISDIR`, or `ETXTBSY` |
| The interpreter is shorter than a complete ELF header                                                 | `EIO`                                                                            |
| The interpreter has bad ELF magic, the wrong architecture, or an FDPIC format rejected by this loader | `ELIBBAD`                                                                        |
| The interpreter's program-header table is structurally invalid or cannot be loaded                    | `ELIBBAD`                                                                        |

The relevant code is [the `PT_INTERP` read/open path](https://github.com/torvalds/linux/blob/v7.2/fs/binfmt_elf.c#L873-L936)
and [the interpreter's early validation](https://github.com/torvalds/linux/blob/v7.2/fs/binfmt_elf.c#L957-L990).

There is a non-obvious exception: the early checks do **not** validate the
interpreter's `e_type`. `load_elf_interp()` checks whether it is `ET_EXEC` or
`ET_DYN` only after the point of no return. A `PT_INTERP` file with valid magic,
architecture, and program-header structure but `e_type = ET_REL` therefore
terminates the invoking process instead of returning `ELIBBAD`.

### GNU properties and architecture-specific checks

When enabled for the architecture, malformed `PT_GNU_PROPERTY` data is parsed
before commit. Oversized data, an invalid note identity, inconsistent lengths,
unsorted or duplicate property types, and truncated properties produce
`ENOEXEC`; a short property read produces `EIO`. Architecture property parsers
may return another errno. See [`parse_elf_properties()`](https://github.com/torvalds/linux/blob/v7.2/fs/binfmt_elf.c#L775-L829).

The `PT_LOPROC` to `PT_HIPROC` hooks and the final `arch_check_elf()` also run
before commit and return their errors to the caller. The source explicitly
places `arch_check_elf()` there so the exec caller can still receive the error.

## Invalidity detected after commit: forced `SIGSEGV`

After `begin_new_exec()` sets `point_of_no_return`, the kernel has committed to
replacing the process. The old image cannot safely resume. If any later step
returns an error, the generic exec path installs a fatal `SIGSEGV` with
`si_code = SI_KERNEL` and the default disposition. A user-installed SIGSEGV
handler cannot recover from it. If the task already has another fatal signal
pending, that signal is used instead.

Malformed ELF data can reach this path in the following ways.

### Main `PT_LOAD` failures

- A load segment cannot be mapped. Examples include a page-incongruent
  `p_offset` and `p_vaddr`, an invalid map range, a fixed-address collision, or
  resource exhaustion.
- `elf_load()` cannot zero or allocate the segment's memory tail.
- A load segment has `p_filesz > p_memsz`.
- `p_vaddr`, `p_memsz`, or their range is outside the architecture's user task
  address space or overflows it.
- An `ET_DYN` image has a first `PT_LOAD`, but the collection of load segments
  computes to a total mapping size of zero. A zero-sized load segment is one
  way to reach this check.

The kernel maps each segment and then performs several of these size checks;
they are not part of the pre-commit validation pass. See [the main mapping
loop](https://github.com/torvalds/linux/blob/v7.2/fs/binfmt_elf.c#L1040-L1232).

### Interpreter mapping failures

The interpreter's header and program-header table are read before commit, but
its load geometry is not validated or mapped until afterward. The process is
therefore killed with `SIGSEGV` if:

- its `e_type` is neither `ET_EXEC` nor `ET_DYN`;
- it has no `PT_LOAD`, or its total load range computes to zero;
- a load mapping fails;
- a load has `p_filesz > p_memsz`; or
- a load address or size is outside the task address space.

See [`load_elf_interp()`](https://github.com/torvalds/linux/blob/v7.2/fs/binfmt_elf.c#L645-L720).

### Entry and final image construction failures

- For an image without `PT_INTERP`, an `e_entry` whose numeric value is outside
  `TASK_SIZE` produces an internal `EINVAL` after commit.
- For a dynamic executable, an invalid computed interpreter entry does the
  same, or propagates an error from `load_elf_interp()`.
- Failure to create the argument/auxiliary-vector tables, map architecture
  pages such as the vDSO, or finish the new stack also occurs after commit.

These all end as a fatal signal, not a returned errno. See [entry validation and
final table construction](https://github.com/torvalds/linux/blob/v7.2/fs/binfmt_elf.c#L1245-L1301).

## Malformed files that exec accepts

`execve` success does not prove that the image is a valid or runnable ELF.
Several important checks are absent or intentionally permissive.

### Entry-point membership and permissions

For an image without `PT_INTERP`, the kernel checks only whether the final entry
address is below `TASK_SIZE`. It does not require `e_entry` to fall inside a
mapped, executable `PT_LOAD` segment. Consequently:

- an entry in an unmapped range normally faults with `SIGSEGV`/`SEGV_MAPERR`;
- an entry in a mapped but non-executable range normally faults with
  `SIGSEGV`/`SEGV_ACCERR`; and
- an entry in executable bytes that are not valid instructions may produce
  `SIGILL`.

Those are faults in the successfully installed new image. They are distinct
from the exec error path's forced `SIGSEGV`/`SI_KERNEL`.

For an image with `PT_INTERP`, the kernel initially enters the interpreter. The
dynamic loader receives the main ELF's entry through the auxiliary vector. It
may reject bad metadata, exit with a diagnostic, or eventually branch to the
bad entry and fault. That behavior belongs to the particular loader, not
`execve`.

### No `PT_LOAD`

The main ELF loader does not have a general “at least one `PT_LOAD`” check. If
there are no load segments, it can still finish exec when `e_entry` is a
numerically permitted address. Control then normally reaches an unmapped
address and faults. By contrast, `load_elf_interp()` explicitly rejects an
interpreter with no nonzero total load range, after commit.

### File shorter than a mapped segment

An ELF can claim a file-backed segment extending past the real end of the
file. Establishing that mapping can succeed. Accessing a wholly beyond-EOF page
then normally raises `SIGBUS`; the failure need not occur inside `execve`.

### Fields the kernel does not need

- Section headers are not used to execute an ELF. Invalid `e_shoff`,
  `e_shentsize`, `e_shnum`, and `e_shstrndx` can be ignored completely.
- On Linux 7.2, the common loader skips a non-power-of-two `p_align` when
  calculating an optional large alignment instead of rejecting the file. See
  [`maximum_alignment()`](https://github.com/torvalds/linux/blob/v7.2/fs/binfmt_elf.c#L491-L509).
- Trailing file contents not referenced by load metadata do not affect exec.
- Some identification fields are accepted or rejected only through
  architecture-specific checks, as described above.

## `ENOEXEC` may invoke another kernel format handler

`ENOEXEC` has special meaning inside the kernel. `search_binary_handler()`
continues to the next registered binary-format handler only when the previous
handler returned `ENOEXEC` and did not cross its point of no return. This is how
scripts and `binfmt_misc` participate in exec. An `EIO`, `ELIBBAD`, or other
error stops the search.

Therefore a wrong-architecture ELF may not return `ENOEXEC` on a machine with a
matching QEMU `binfmt_misc` rule: the ELF loader rejects it, then the QEMU rule
accepts it. This happened during the experiment when an AArch64 ELF's
`e_machine` was changed to x86-64. Changing it to an unregistered value made
raw `execve` return `ENOEXEC` as expected.

See [`search_binary_handler()`](https://github.com/torvalds/linux/blob/v7.2/fs/exec.c#L1672-L1706).
A shell may also react to a final `ENOEXEC` by trying to interpret the file as
a script, but that fallback is outside the kernel. Tests of kernel behavior
must call `execve` directly.

## Experimental cross-check

The experiment generated a minimal static ELF for the VM's native AArch64
architecture, mutated one field at a time, and invoked each fixture with a raw
`execve` from a forked child. A close-on-exec pipe distinguished a returned
errno from a committed exec; `waitpid` recorded the child's exit or signal.
Core dumps were disabled.

Representative results on Linux 6.8.0-100-generic were:

| Mutation                                                        | Observed result | Source classification                     |
| --------------------------------------------------------------- | --------------- | ----------------------------------------- |
| Bad magic, `ET_REL` main image, unregistered `e_machine`        | `ENOEXEC`       | Rejected before commit                    |
| Wrong `e_phentsize`, zero `e_phnum`, program headers beyond EOF | `ENOEXEC`       | Rejected before commit                    |
| `PT_INTERP.p_filesz = 1` or missing final NUL                   | `ENOEXEC`       | Rejected before commit                    |
| Missing interpreter path                                        | `ENOENT`        | Rejected before commit                    |
| Interpreter shorter than one ELF header                         | `EIO`           | Rejected before commit                    |
| Full-size non-ELF interpreter                                   | `ELIBBAD`       | Rejected before commit                    |
| Main `PT_LOAD.p_filesz > p_memsz`                               | `SIGSEGV`       | Kernel loader error after commit          |
| Page-incongruent main load offset                               | `SIGSEGV`       | Kernel mapping error after commit         |
| Static entry numerically outside the task address space         | `SIGSEGV`       | Kernel entry check after commit           |
| Interpreter with no `PT_LOAD`                                   | `SIGSEGV`       | Kernel interpreter error after commit     |
| Interpreter `PT_LOAD.p_filesz > p_memsz`                        | `SIGSEGV`       | Kernel interpreter error after commit     |
| Interpreter with `e_type = ET_REL`                              | `SIGSEGV`       | Kernel interpreter error after commit     |
| Static entry in an unmapped or non-executable range             | `SIGSEGV`       | Exec succeeded; CPU faulted afterward     |
| Main image with no `PT_LOAD`                                    | `SIGSEGV`       | Exec succeeded; entry was unmapped        |
| Segment claims entry page past end of file                      | `SIGBUS`        | Exec succeeded; page fault hit beyond EOF |
| Invalid section-header table                                    | Exit 0          | Ignored by the exec loader                |
| `PT_LOAD.p_align = 3`                                           | Exit 0          | Accepted by the common loader             |
| Inconsistent `EI_CLASS`, `EI_DATA`, or version markers          | Exit 0          | Accepted by AArch64's loader checks       |

The result table alone cannot distinguish the two kinds of `SIGSEGV`; both are
reported as signal 11 to an ordinary parent. The classification comes from the
source path. A tracer that inspects the delivered `siginfo` can distinguish the
forced exec failure's `SI_KERNEL` from ordinary `SEGV_MAPERR` or `SEGV_ACCERR`
faults. The `sched_process_exec` tracepoint also occurs only after the new image
has been installed successfully.

The Linux 7.2 source retains the same point-of-no-return and mapping order used
by the tested 6.8 kernel. Exact limits, architecture hooks, accepted malformed
fields, and errno values can change across kernel releases and architectures;
the tables above should be read against the stated baseline.
