# Validation results

Validation date: 2026-08-02

## Native AArch64

Environment:

```text
Ubuntu 24.04.4 LTS
Linux 6.8.0-134-generic aarch64
GCC 13.3.0
Lima/VZ, 4 CPUs, 6 GiB RAM
```

The host workspace is mounted read-only in the VM, so the sources were copied
to a disposable writable directory before building:

```sh
probe_dir=$(mktemp -d)
repo_root=$(git rev-parse --show-toplevel)
cp "$repo_root"/research/ptrace-exec-prototype/* "$probe_dir"/
cd "$probe_dir"
make check
```

Observed output:

```text
cc -O2 -g -Wall -Wextra -Werror -std=gnu11 -o injector injector.c
cc -O2 -g -Wall -Wextra -Werror -std=gnu11 -o target target.c
./injector "$(realpath ./target)"
injector: caught PTRACE_EVENT_EXEC before target entry
injector: mapped handler at 0xe4e3b6551000, handler=0xe4e3b6551000, blob=32 bytes
injector: detached; target's trapped syscall now has no tracer
target: TracerPid=0 before trapped getpid
target: trapped getpid returned 0x51515151 (expected 0x51515151)
PASS: post-exec handler ran entirely in-process after detach
injector: target exit status 0
```

The same injector passed with a fully static target:

```sh
cc -O2 -static -Wall -Wextra -Werror -std=gnu11 -o target-static target.c
./injector "$(realpath ./target-static)"
```

Twenty additional dynamic-target runs completed successfully.

The injector also passed the dynamic-target test when compiled with
AddressSanitizer and UndefinedBehaviorSanitizer:

```sh
cc -O1 -g -fsanitize=address,undefined -fno-omit-frame-pointer \
  -Wall -Wextra -Werror -std=gnu11 -o injector-asan injector.c
ASAN_OPTIONS=detect_leaks=1 ./injector-asan "$(realpath ./target)"
```

## x86-64 build check

The x86-64 implementation was cross-compiled on macOS with Zig 0.15.2:

```sh
zig cc -target x86_64-linux-gnu -O2 -g -Wall -Wextra -Werror \
  -std=gnu11 -o /tmp/injector-x86_64 injector.c
zig cc -target x86_64-linux-gnu -O2 -g -Wall -Wextra -Werror \
  -std=gnu11 -o /tmp/target-x86_64 target.c
```

Both outputs were valid dynamically linked x86-64 ELF executables. There was no
native x86-64 Linux VM or full-system x86 emulator installed, so this branch is
build-checked but not runtime-validated. A native `ubuntu-latest` x86-64 CI job
can run the same `make check` command without special privileges on a runner
whose ptrace policy permits parent-child tracing.

## What the result establishes

- `PTRACE_EVENT_EXEC` occurs late enough that the new address space exists and
  early enough to install a handler before the ELF entry point runs.
- Remote syscalls can allocate the island and register the handler while the
  target remains at the exec stop.
- Detaching before resuming avoids ptrace signal-delivery stops for subsequent
  seccomp-generated `SIGSYS` signals.
- The target observed `TracerPid=0` before entering its trapped syscall, so the
  emulated result came from the injected handler rather than tracer mediation.
- The proof works for both dynamic and static AArch64 executables.
