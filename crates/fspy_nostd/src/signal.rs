//! Signal operations provided directly by the Linux kernel.

use core::{ffi::c_void, mem::size_of, num::NonZeroI32, ptr};

use crate::{Error, Result};

/// A signal handler receiving `siginfo_t` and `ucontext_t` pointers.
pub type SigInfoHandler = unsafe extern "C" fn(i32, *mut c_void, *mut c_void);

/// A signal number.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Signal(NonZeroI32);

impl Signal {
    /// The signal raised by a trapping seccomp filter.
    pub const SIGSYS: Self =
        Self(NonZeroI32::new(linux_raw_sys::general::SIGSYS.cast_signed()).unwrap());
}

/// A signal action using a three-argument handler.
pub struct SigAction {
    handler: SigInfoHandler,
}

impl SigAction {
    /// Creates an action with an empty signal mask.
    #[must_use]
    pub const fn new(handler: SigInfoHandler) -> Self {
        Self { handler }
    }
}

type Restorer = unsafe extern "C" fn() -> !;

#[repr(C)]
struct KernelSigaction {
    handler: SigInfoHandler,
    flags: u64,
    restorer: Restorer,
    mask: u64,
}

/// Installs `action` for `signal`.
///
/// # Errors
///
/// Returns the error reported by the kernel.
///
/// # Safety
///
/// The handler may run asynchronously at any point. It must obey the signal
/// handler ABI, access only valid signal-context data, and must not unwind.
pub unsafe fn sigaction(signal: Signal, action: &SigAction) -> Result<()> {
    let action = KernelSigaction {
        handler: action.handler,
        flags: u64::from(linux_raw_sys::general::SA_SIGINFO | linux_raw_sys::general::SA_RESTORER),
        restorer: arch::restore,
        mask: 0,
    };
    // SAFETY: `action` uses the Linux kernel's 64-bit sigaction layout. The
    // kernel copies it during the call, and a kernel signal set is one u64.
    unsafe {
        syscalls::syscall!(
            syscalls::Sysno::rt_sigaction,
            signal.0.get(),
            ptr::from_ref(&action),
            ptr::null::<KernelSigaction>(),
            size_of::<u64>()
        )
    }
    .map(|_| ())
    .map_err(Error::from)
}

#[cfg(target_arch = "x86_64")]
mod arch {
    use core::arch::naked_asm;

    #[unsafe(naked)]
    pub unsafe extern "C" fn restore() -> ! {
        naked_asm!("mov rax, 15", "syscall")
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use core::arch::naked_asm;

    #[unsafe(naked)]
    pub unsafe extern "C" fn restore() -> ! {
        naked_asm!("mov x8, #139", "svc #0")
    }
}
