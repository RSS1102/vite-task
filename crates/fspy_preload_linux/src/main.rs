//! A freestanding payload for the ptrace/SIGSYS proof of concept.
//!
//! It targets `*-unknown-none` — a bare-metal target with no libc, no C
//! runtime, and no dynamic loader — so it links as a self-contained,
//! position-independent blob through `rust-lld`. Before restoring the target's
//! exec-stop register context, it installs a raw `rt_sigaction` handler. The
//! handler prints each seccomp-trapped `openat` path, reissues the syscall with
//! a filter-bypass cookie, and writes the raw result into the saved signal
//! context. On `AArch64`, `x16` holds the target entry address for the final
//! indirect branch and is the only register not restored. The payload reads no
//! process environment.
//!
//! `*-unknown-none` reports `target_os = "none"`. `fspy_nostd` deliberately
//! treats that as a freestanding Linux environment and provides its direct
//! syscall layer there. On any host target this crate is an ordinary empty
//! program, so the workspace still builds everywhere.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

#[cfg(target_os = "none")]
mod sigsys;

#[cfg(target_os = "none")]
mod payload {
    use core::{arch::naked_asm, mem::offset_of};

    use fspy_preload_linux::ResumeContext;

    /// ELF entry point. An entry point is not reached by a `call`, so there is
    /// no return address on the stack and the entry stack alignment differs from
    /// a call site (on x86-64 the kernel enters with `RSP` 16-aligned, whereas a
    /// called function expects `RSP` ≡ 8 (mod 16)). It is therefore naked — no
    /// compiler prologue making those wrong assumptions — and simply transfers
    /// to [`payload_main`] via `call`/`bl`, which re-establishes a normal,
    /// correctly aligned frame. The three argument registers the supervisor set
    /// (`data`, `len`, `context`) flow straight through.
    #[cfg(target_arch = "x86_64")]
    #[unsafe(naked)]
    #[unsafe(no_mangle)]
    pub extern "C" fn _start() -> ! {
        naked_asm!("call {main}", "ud2", main = sym payload_main)
    }

    #[cfg(target_arch = "aarch64")]
    #[unsafe(naked)]
    #[unsafe(no_mangle)]
    pub extern "C" fn _start() -> ! {
        naked_asm!("bl {main}", "brk #0", main = sym payload_main)
    }

    /// Reached from [`_start`] with a normal frame. Installs the handler, writes
    /// the supervisor's bytes, and resumes the supplied register context.
    extern "C" fn payload_main(data: *const u8, len: usize, context: *const ResumeContext) -> ! {
        if crate::sigsys::install().is_err() {
            // SAFETY: the injected process keeps its standard-error descriptor
            // open while the payload runs.
            let stderr = unsafe { fspy_nostd::stdio::stderr() };
            let _ = fspy_nostd::io::write(
                stderr,
                b"fspy_preload_linux: failed to install SIGSYS handler\n",
            );
            fspy_nostd::process::exit_group(101);
        }
        // SAFETY: the supervisor placed `len` readable bytes at `data`. A short
        // write is fine — this is a best-effort diagnostic.
        let data = unsafe { core::slice::from_raw_parts(data, len) };
        // SAFETY: the injected process keeps its standard-error descriptor
        // open while the payload runs.
        let stderr = unsafe { fspy_nostd::stdio::stderr() };
        let _ = fspy_nostd::io::write(stderr, data);
        // SAFETY: the supervisor placed a correctly aligned, initialized
        // `ResumeContext` at `context` and keeps its mapping alive.
        unsafe { resume(context) }
    }

    /// Restores every exec-stop general-purpose register and `rflags`, then
    /// returns to the original entry. `ret` obtains the destination from memory,
    /// so no restored register has to retain it.
    #[cfg(target_arch = "x86_64")]
    #[unsafe(naked)]
    unsafe extern "C" fn resume(_context: *const ResumeContext) -> ! {
        naked_asm!(
            "mov rax, rdi",
            "mov rsp, [rax + {rsp}]",
            "push qword ptr [rax + {rip}]",
            "push qword ptr [rax + {rflags}]",
            "popfq",
            "mov r15, [rax + {r15}]",
            "mov r14, [rax + {r14}]",
            "mov r13, [rax + {r13}]",
            "mov r12, [rax + {r12}]",
            "mov rbp, [rax + {rbp}]",
            "mov rbx, [rax + {rbx}]",
            "mov r11, [rax + {r11}]",
            "mov r10, [rax + {r10}]",
            "mov r9, [rax + {r9}]",
            "mov r8, [rax + {r8}]",
            "mov rcx, [rax + {rcx}]",
            "mov rdx, [rax + {rdx}]",
            "mov rsi, [rax + {rsi}]",
            "mov rdi, [rax + {rdi}]",
            "mov rax, [rax + {rax}]",
            "ret",
            rsp = const offset_of!(ResumeContext, rsp),
            rip = const offset_of!(ResumeContext, rip),
            rflags = const offset_of!(ResumeContext, rflags),
            r15 = const offset_of!(ResumeContext, r15),
            r14 = const offset_of!(ResumeContext, r14),
            r13 = const offset_of!(ResumeContext, r13),
            r12 = const offset_of!(ResumeContext, r12),
            rbp = const offset_of!(ResumeContext, rbp),
            rbx = const offset_of!(ResumeContext, rbx),
            r11 = const offset_of!(ResumeContext, r11),
            r10 = const offset_of!(ResumeContext, r10),
            r9 = const offset_of!(ResumeContext, r9),
            r8 = const offset_of!(ResumeContext, r8),
            rcx = const offset_of!(ResumeContext, rcx),
            rdx = const offset_of!(ResumeContext, rdx),
            rsi = const offset_of!(ResumeContext, rsi),
            rdi = const offset_of!(ResumeContext, rdi),
            rax = const offset_of!(ResumeContext, rax),
        )
    }

    /// Restores the exec-stop context and branches to the original entry through
    /// `x16`. `AArch64` has no memory-indirect branch, so `x16` intentionally
    /// retains the entry address; every other general-purpose register and the
    /// condition flags are restored.
    #[cfg(target_arch = "aarch64")]
    #[unsafe(naked)]
    unsafe extern "C" fn resume(_context: *const ResumeContext) -> ! {
        naked_asm!(
            "mov x16, x0",
            "ldr x15, [x16, #{sp}]",
            "mov sp, x15",
            "ldr x15, [x16, #{pstate}]",
            "msr nzcv, x15",
            "ldp x0, x1, [x16, #{r0}]",
            "ldp x2, x3, [x16, #{r2}]",
            "ldp x4, x5, [x16, #{r4}]",
            "ldp x6, x7, [x16, #{r6}]",
            "ldp x8, x9, [x16, #{r8}]",
            "ldp x10, x11, [x16, #{r10}]",
            "ldp x12, x13, [x16, #{r12}]",
            "ldp x14, x15, [x16, #{r14}]",
            "ldp x17, x18, [x16, #{r17}]",
            "ldp x19, x20, [x16, #{r19}]",
            "ldp x21, x22, [x16, #{r21}]",
            "ldp x23, x24, [x16, #{r23}]",
            "ldp x25, x26, [x16, #{r25}]",
            "ldp x27, x28, [x16, #{r27}]",
            "ldp x29, x30, [x16, #{r29}]",
            "ldr x16, [x16, #{pc}]",
            "br x16",
            sp = const offset_of!(ResumeContext, sp),
            pstate = const offset_of!(ResumeContext, pstate),
            r0 = const offset_of!(ResumeContext, registers),
            r2 = const offset_of!(ResumeContext, registers) + 2 * core::mem::size_of::<u64>(),
            r4 = const offset_of!(ResumeContext, registers) + 4 * core::mem::size_of::<u64>(),
            r6 = const offset_of!(ResumeContext, registers) + 6 * core::mem::size_of::<u64>(),
            r8 = const offset_of!(ResumeContext, registers) + 8 * core::mem::size_of::<u64>(),
            r10 = const offset_of!(ResumeContext, registers) + 10 * core::mem::size_of::<u64>(),
            r12 = const offset_of!(ResumeContext, registers) + 12 * core::mem::size_of::<u64>(),
            r14 = const offset_of!(ResumeContext, registers) + 14 * core::mem::size_of::<u64>(),
            r17 = const offset_of!(ResumeContext, registers) + 17 * core::mem::size_of::<u64>(),
            r19 = const offset_of!(ResumeContext, registers) + 19 * core::mem::size_of::<u64>(),
            r21 = const offset_of!(ResumeContext, registers) + 21 * core::mem::size_of::<u64>(),
            r23 = const offset_of!(ResumeContext, registers) + 23 * core::mem::size_of::<u64>(),
            r25 = const offset_of!(ResumeContext, registers) + 25 * core::mem::size_of::<u64>(),
            r27 = const offset_of!(ResumeContext, registers) + 27 * core::mem::size_of::<u64>(),
            r29 = const offset_of!(ResumeContext, registers) + 29 * core::mem::size_of::<u64>(),
            pc = const offset_of!(ResumeContext, pc),
        )
    }

    #[panic_handler]
    fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
        use core::fmt::Write as _;

        // A panic here is a bug — the payload's code cannot normally panic.
        // Fail closed: report the panic (message and location) and abort the
        // whole process (with the Rust panic exit code) rather than continue in
        // an undefined state.
        // SAFETY: the injected process keeps its standard-error descriptor
        // open while the payload runs.
        let mut stderr = unsafe { fspy_nostd::stdio::stderr() };
        let _ = writeln!(stderr, "fspy_preload_linux: {info}");
        fspy_nostd::process::exit_group(101)
    }
}

#[cfg(not(target_os = "none"))]
fn main() {}
