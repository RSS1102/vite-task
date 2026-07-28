pub mod arg;

use std::io;

use nix::{sys::ptrace, unistd::Pid};

#[derive(Debug, Clone, Copy)]
pub struct Syscall {
    pid: libc::pid_t,
    number: i64,
    args: [u64; 6],
}

impl Syscall {
    pub(crate) fn read(pid: Pid) -> nix::Result<Self> {
        let registers = ptrace::getregs(pid)?;

        #[cfg(target_arch = "x86_64")]
        let (number, args) = (
            i64::from_ne_bytes(registers.orig_rax.to_ne_bytes()),
            [
                registers.rdi,
                registers.rsi,
                registers.rdx,
                registers.r10,
                registers.r8,
                registers.r9,
            ],
        );
        #[cfg(target_arch = "aarch64")]
        let (number, args) = (
            i64::from_ne_bytes(registers.regs[8].to_ne_bytes()),
            [
                registers.regs[0],
                registers.regs[1],
                registers.regs[2],
                registers.regs[3],
                registers.regs[4],
                registers.regs[5],
            ],
        );

        Ok(Self { pid: pid.as_raw(), number, args })
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn pid(&self) -> libc::pid_t {
        self.pid
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn number(&self) -> i64 {
        self.number
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn args(&self) -> &[u64; 6] {
        &self.args
    }
}

#[expect(clippy::module_name_repetitions, reason = "clearer as a standalone export")]
pub trait PtraceHandler {
    fn syscalls() -> &'static [syscalls::Sysno];

    /// Handles a seccomp-filtered syscall stopped by ptrace.
    ///
    /// # Errors
    /// Returns an error if the handler fails to process the syscall.
    fn handle_syscall(&mut self, syscall: &Syscall) -> io::Result<()>;
}

#[doc(hidden)] // Re-export for use in the macro
pub use syscalls::Sysno;

#[macro_export]
macro_rules! impl_handler {
    ($type:ty: $(
        $(#[$attr:meta])?
        $syscall:ident,
    )* ) => {

    impl $crate::supervisor::handler::PtraceHandler for $type {
        fn syscalls() -> &'static [$crate::supervisor::handler::Sysno] {
            &[ $(
                $(#[$attr])?
                $crate::supervisor::handler::Sysno::$syscall
            ),* ]
        }

        fn handle_syscall(
            &mut self,
            syscall: &$crate::supervisor::handler::Syscall,
        ) -> ::std::io::Result<()> {
            $crate::supervisor::handler::arg::Caller::with_pid(syscall.pid(), |caller| {
                $(
                    $(#[$attr])?
                    if syscall.number() == $crate::supervisor::handler::Sysno::$syscall as i64 {
                        return self.$syscall(
                            caller,
                            $crate::supervisor::handler::arg::FromSyscall::from_syscall(syscall)?,
                        )
                    }
                )*
                Ok(())
            })
        }
    }
    };
}
