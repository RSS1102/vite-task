//! Proof-of-concept ptrace injection.
//!
//! Spawns `/bin/cat test_path`, stops it at the moment `execve` finishes (the new
//! program is loaded but has not run an instruction yet), injects the
//! [`fspy_preload_linux`] payload, and lets it install a SIGSYS handler before
//! the target starts. A seccomp filter traps `openat`; the handler prints its
//! path and performs the syscall in-process with a bypass cookie.
//!
//! Expected output includes `openat: test_path` on stderr and the test file's
//! contents on stdout.

#[cfg(target_os = "linux")]
mod seccomp;

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("inject_demo only runs on Linux");
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    linux::run()
}

#[cfg(target_os = "linux")]
mod linux {
    #![expect(clippy::print_stdout, reason = "a demo binary reporting its progress")]
    #![expect(
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss,
        reason = "ptrace words and addresses round-trip through u64/i64 on 64-bit"
    )]

    use std::{
        mem::{align_of, size_of},
        os::unix::process::CommandExt as _,
        process::Command,
    };

    use anyhow::{Context as _, Result, bail, ensure};
    use fspy_blob::Blob;
    use fspy_preload_linux::ResumeContext;
    use nix::{
        sys::{
            ptrace::{self, Options},
            signal::{Signal, kill},
            wait::{WaitStatus, waitpid},
        },
        unistd::Pid,
    };

    /// The freestanding payload, built for our target and embedded at compile time.
    const PAYLOAD: &[u8] = include_bytes!(env!("CARGO_BIN_FILE_FSPY_PRELOAD_LINUX"));

    const MESSAGE: &[u8] = b"fspy_preload_linux: installed SIGSYS handler\n";

    pub fn run() -> Result<()> {
        let blob =
            Blob::from_elf(PAYLOAD).context("payload is not an injectable static-PIE ELF")?;
        println!(
            "payload: {} bytes, entry +{:#x}, {} relocations",
            blob.image_len(),
            blob.entry(),
            blob.relocation_count(),
        );

        let directory = tempfile::tempdir().context("create demo directory")?;
        std::fs::write(directory.path().join("test_path"), b"SIGSYS works\n")
            .context("write test_path")?;
        let seccomp_filter = crate::seccomp::compile().context("compile seccomp filter")?;

        // Spawn `/bin/cat test_path` tracing itself, so it stops at the exec.
        let mut command = Command::new("/bin/cat");
        command.arg("test_path").current_dir(directory.path());
        // SAFETY: the child performs only ptrace, prctl, and seccomp operations
        // and does not allocate or access shared process-runtime state.
        unsafe {
            command.pre_exec(move || {
                ptrace::traceme()?;
                crate::seccomp::apply(&seccomp_filter)
            });
        }
        let child = command.spawn().context("spawn /bin/cat")?;
        let pid = Pid::from_raw(child.id().cast_signed());

        let result = (|| {
            // The exec-stop: /bin/cat is mapped but has not run an instruction.
            expect_trap(waitpid(pid, None)?, "exec")?;
            // If the supervisor terminates unexpectedly while attached, never
            // leave a stopped or partially modified target behind.
            ptrace::setoptions(pid, Options::PTRACE_O_EXITKILL)?;
            inject(pid, &blob)
        })();
        if result.is_err() {
            terminate_tracee(pid);
        }
        result
    }

    /// Drive the stopped target through the injection.
    fn inject(pid: Pid, blob: &Blob) -> Result<()> {
        // Registers at the entry of the freshly exec'd program. A copy goes
        // into the target so the payload can resume the program itself.
        let saved = ptrace::getregs(pid)?;
        let resume_context = arch::resume_context(&saved);

        // 1. Map an RWX region for the payload image, the string handed to it,
        //    and an aligned resume context.
        let data_offset = blob.image_len();
        let context_offset = data_offset
            .checked_add(MESSAGE.len())
            .and_then(|offset| offset.checked_next_multiple_of(align_of::<ResumeContext>()))
            .context("resume context offset overflows")?;
        let mapping_len = context_offset
            .checked_add(size_of::<ResumeContext>())
            .context("injected mapping size overflows")?;
        let base = remote_mmap(pid, &saved, mapping_len as u64)?;
        println!("mapped {mapping_len} bytes into the target at {base:#x}");

        // 2. Write the relocated payload, supervisor string, and register
        //    context. Padding aligns the typed context for the payload.
        let mut contents = blob.bind(base);
        contents.extend_from_slice(MESSAGE);
        contents.resize(context_offset, 0);
        contents.extend_from_slice(resume_context.as_bytes());
        debug_assert_eq!(contents.len(), mapping_len);
        write_bytes(pid, base, &contents)?;
        let data = base + data_offset as u64;
        let context = base + context_offset as u64;

        // 3. Point the program counter at the payload entry and hand it the
        //    string and resume context in the first three ABI argument
        //    registers.
        let mut running = saved;
        arch::set_pc(&mut running, base + blob.entry() as u64);
        arch::set_args(&mut running, data, MESSAGE.len() as u64, context);
        ptrace::setregs(pid, running)?;

        // 4. Detaching resumes the payload. Its in-process trampoline restores
        //    the exec context and transfers directly to the original entry,
        //    avoiding a completion signal and second ptrace turn.
        ptrace::detach(pid, None)?;
        println!("detached — payload will restore the exec context in-process");
        match waitpid(pid, None)? {
            WaitStatus::Exited(exited, 0) if exited == pid => {
                println!("/bin/cat exited with code 0");
            }
            WaitStatus::Exited(_, code) => bail!("/bin/cat exited with code {code}"),
            WaitStatus::Signaled(_, signal, dumped) => {
                bail!("/bin/cat was killed by {signal} (core dumped: {dumped})")
            }
            other => bail!("unexpected /bin/cat wait status: {other:?}"),
        }
        Ok(())
    }

    /// Runs `mmap(NULL, len, RWX, PRIVATE|ANON, -1, 0)` inside the target by
    /// borrowing its program counter for a syscall followed by an explicit
    /// trap, then restoring the instructions it clobbered.
    fn remote_mmap(pid: Pid, saved: &arch::Regs, len: u64) -> Result<u64> {
        let pc = arch::pc(saved);
        let original = read_word(pid, pc)?;
        write_word(pid, pc, arch::patch_syscall_and_trap(original))?;

        let mut regs = *saved;
        arch::set_mmap(&mut regs, len);
        ptrace::setregs(pid, regs)?;
        ptrace::cont(pid, None)?;
        expect_trap(waitpid(pid, None)?, "remote mmap")?;

        let result = arch::syscall_result(&ptrace::getregs(pid)?);
        // The saved register set is installed below before the payload starts.
        // On every error, the outer cleanup kills the stopped tracee.
        write_word(pid, pc, original)?;

        // mmap reports failure as a small negative errno in [-4095, -1].
        let signed = result as i64;
        ensure!(!(-4096..0).contains(&signed), "remote mmap failed");
        Ok(result)
    }

    /// Writes `bytes` into the target one word at a time. The destination is a
    /// fresh (zeroed) mapping, so padding the final word with zeros is safe.
    fn write_bytes(pid: Pid, addr: u64, bytes: &[u8]) -> Result<()> {
        for (index, chunk) in bytes.chunks(8).enumerate() {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            write_word(pid, addr + index as u64 * 8, u64::from_le_bytes(word))?;
        }
        Ok(())
    }

    fn read_word(pid: Pid, addr: u64) -> Result<u64> {
        Ok(ptrace::read(pid, addr as ptrace::AddressType)? as u64)
    }

    fn write_word(pid: Pid, addr: u64, word: u64) -> Result<()> {
        ptrace::write(pid, addr as ptrace::AddressType, word as _)?;
        Ok(())
    }

    fn expect_trap(status: WaitStatus, phase: &str) -> Result<()> {
        match status {
            WaitStatus::Stopped(_, Signal::SIGTRAP) => Ok(()),
            _ => bail!("expected a SIGTRAP stop during {phase}, got {status:?}"),
        }
    }

    fn terminate_tracee(pid: Pid) {
        let _ = kill(pid, Signal::SIGKILL);
        let _ = waitpid(pid, None);
    }

    /// Architecture-specific register access and syscall encoding.
    #[cfg(target_arch = "x86_64")]
    mod arch {
        pub type Regs = libc::user_regs_struct;

        pub const fn pc(r: &Regs) -> u64 {
            r.rip
        }
        pub const fn set_pc(r: &mut Regs, v: u64) {
            r.rip = v;
        }
        /// Put the payload entry's arguments in the C ABI argument registers.
        pub const fn set_args(r: &mut Regs, arg0: u64, arg1: u64, arg2: u64) {
            r.rdi = arg0;
            r.rsi = arg1;
            r.rdx = arg2;
        }
        pub const fn resume_context(r: &Regs) -> fspy_preload_linux::ResumeContext {
            fspy_preload_linux::ResumeContext {
                r15: r.r15,
                r14: r.r14,
                r13: r.r13,
                r12: r.r12,
                rbp: r.rbp,
                rbx: r.rbx,
                r11: r.r11,
                r10: r.r10,
                r9: r.r9,
                r8: r.r8,
                rax: r.rax,
                rcx: r.rcx,
                rdx: r.rdx,
                rsi: r.rsi,
                rdi: r.rdi,
                rsp: r.rsp,
                rip: r.rip,
                rflags: r.eflags,
            }
        }
        pub const fn syscall_result(r: &Regs) -> u64 {
            r.rax
        }
        /// Overlay `syscall; int3` onto the low bytes of `word`.
        pub const fn patch_syscall_and_trap(word: u64) -> u64 {
            (word & !0xff_ffff) | 0xcc_050f
        }
        /// Arrange registers for `mmap(NULL, len, RWX, PRIVATE|ANON, -1, 0)`.
        pub const fn set_mmap(r: &mut Regs, len: u64) {
            r.rax = 9; // mmap
            r.rdi = 0; // let the kernel choose the address
            r.rsi = len;
            r.rdx = 0x7; // PROT_READ|WRITE|EXEC
            r.r10 = 0x22; // MAP_PRIVATE|MAP_ANONYMOUS
            r.r8 = u64::MAX; // fd -1
            r.r9 = 0;
        }
    }

    #[cfg(target_arch = "aarch64")]
    mod arch {
        pub type Regs = libc::user_regs_struct;

        pub const fn pc(r: &Regs) -> u64 {
            r.pc
        }
        pub const fn set_pc(r: &mut Regs, v: u64) {
            r.pc = v;
        }
        /// Put the payload entry's arguments in the C ABI argument registers.
        pub const fn set_args(r: &mut Regs, arg0: u64, arg1: u64, arg2: u64) {
            r.regs[0] = arg0;
            r.regs[1] = arg1;
            r.regs[2] = arg2;
        }
        pub const fn resume_context(r: &Regs) -> fspy_preload_linux::ResumeContext {
            fspy_preload_linux::ResumeContext {
                registers: r.regs,
                sp: r.sp,
                pc: r.pc,
                pstate: r.pstate,
            }
        }
        pub const fn syscall_result(r: &Regs) -> u64 {
            r.regs[0]
        }
        /// Overlay `svc #0; brk #0` onto `word`.
        pub const fn patch_syscall_and_trap(_word: u64) -> u64 {
            0xd420_0000_d400_0001
        }
        /// Arrange registers for `mmap(NULL, len, RWX, PRIVATE|ANON, -1, 0)`.
        pub const fn set_mmap(r: &mut Regs, len: u64) {
            r.regs[8] = 222; // mmap
            r.regs[0] = 0; // let the kernel choose the address
            r.regs[1] = len;
            r.regs[2] = 0x7; // PROT_READ|WRITE|EXEC
            r.regs[3] = 0x22; // MAP_PRIVATE|MAP_ANONYMOUS
            r.regs[4] = u64::MAX; // fd -1
            r.regs[5] = 0;
        }
    }
}
