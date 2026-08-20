//! In-process handling for `SECCOMP_RET_TRAP` on `openat`.

use core::{ffi::c_void, fmt::Write as _, ptr};

use fspy_nostd::{
    CStr, Error, Thin,
    signal::{self, SigAction, Signal},
};
use fspy_preload_linux::OPENAT_COOKIE;

/// Installs the handler using the kernel ABI, without libc.
pub fn install() -> Result<(), Error> {
    let action = SigAction::new(handle_sigsys);
    // SAFETY: the handler has the required ABI, does not unwind, and uses only
    // the writable signal context and direct system calls.
    unsafe { signal::sigaction(Signal::SIGSYS, &action) }
}

unsafe extern "C" fn handle_sigsys(_signal: i32, _info: *mut c_void, context: *mut c_void) {
    // SAFETY: SA_SIGINFO supplies a writable architecture-specific ucontext.
    let arguments = unsafe { arch::openat_arguments(context) };
    // SAFETY: the trapped call supplied this pointer as its pathname. Invalid
    // target pointers are deliberately left to a later EFAULT-correctness pass.
    unsafe { print_path(ptr::with_exposed_provenance(arguments[1])) };

    // SAFETY: these are the original `openat` arguments. Linux ignores the
    // sixth argument, while the seccomp filter recognizes it as the bypass
    // cookie. Use the raw return value so negative errno values go straight
    // back into the interrupted program's return register.
    let result = unsafe {
        syscalls::raw_syscall!(
            syscalls::Sysno::openat,
            arguments[0],
            arguments[1],
            arguments[2],
            arguments[3],
            arguments[4],
            OPENAT_COOKIE
        )
    };
    // SAFETY: this is the same writable ucontext supplied above.
    unsafe { arch::set_result(context, result) };
}

unsafe fn print_path(path: *const u8) {
    // SAFETY: upheld by the caller.
    let path = unsafe { CStr::<Thin>::from_ptr(path) };

    // SAFETY: the process keeps its standard-error descriptor open.
    let mut stderr = unsafe { fspy_nostd::stdio::stderr() };
    let _ = writeln!(stderr, "openat: {path}");
}

#[repr(C)]
struct Stack {
    pointer: *mut c_void,
    flags: i32,
    size: usize,
}

#[cfg(target_arch = "x86_64")]
mod arch {
    use core::{ffi::c_void, mem::offset_of};

    use super::Stack;

    const R8: usize = 0;
    const R10: usize = 2;
    const RDI: usize = 8;
    const RSI: usize = 9;
    const RDX: usize = 12;
    const RAX: usize = 13;

    #[repr(C)]
    struct MachineContext {
        registers: [u64; 23],
    }

    #[repr(C)]
    struct UserContext {
        flags: usize,
        link: *mut Self,
        stack: Stack,
        machine: MachineContext,
    }

    pub unsafe fn openat_arguments(context: *mut c_void) -> [usize; 5] {
        // SAFETY: the signal ABI supplies this x86-64 ucontext layout.
        let registers = unsafe { &(*context.cast::<UserContext>()).machine.registers };
        [
            usize::from_ne_bytes(registers[RDI].to_ne_bytes()),
            usize::from_ne_bytes(registers[RSI].to_ne_bytes()),
            usize::from_ne_bytes(registers[RDX].to_ne_bytes()),
            usize::from_ne_bytes(registers[R10].to_ne_bytes()),
            usize::from_ne_bytes(registers[R8].to_ne_bytes()),
        ]
    }

    pub unsafe fn set_result(context: *mut c_void, result: usize) {
        // SAFETY: the signal ABI supplies a writable x86-64 ucontext.
        unsafe {
            (*context.cast::<UserContext>()).machine.registers[RAX] =
                u64::from_ne_bytes(result.to_ne_bytes());
        };
    }

    const _: () = assert!(offset_of!(UserContext, machine) == 40);
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use core::{ffi::c_void, mem::offset_of};

    use super::Stack;

    #[repr(C, align(16))]
    struct MachineContext {
        fault_address: u64,
        registers: [u64; 31],
    }

    #[repr(C)]
    struct UserContext {
        flags: usize,
        link: *mut Self,
        stack: Stack,
        // The arm64 kernel reserves 1024 bits here to match glibc's sigset_t.
        signal_mask: [u64; 16],
        machine: MachineContext,
    }

    pub unsafe fn openat_arguments(context: *mut c_void) -> [usize; 5] {
        // SAFETY: the signal ABI supplies this AArch64 ucontext layout.
        let registers = unsafe { &(*context.cast::<UserContext>()).machine.registers };
        [
            usize::from_ne_bytes(registers[0].to_ne_bytes()),
            usize::from_ne_bytes(registers[1].to_ne_bytes()),
            usize::from_ne_bytes(registers[2].to_ne_bytes()),
            usize::from_ne_bytes(registers[3].to_ne_bytes()),
            usize::from_ne_bytes(registers[4].to_ne_bytes()),
        ]
    }

    pub unsafe fn set_result(context: *mut c_void, result: usize) {
        // SAFETY: the signal ABI supplies a writable AArch64 ucontext.
        unsafe {
            (*context.cast::<UserContext>()).machine.registers[0] =
                u64::from_ne_bytes(result.to_ne_bytes());
        };
    }

    const _: () = assert!(offset_of!(UserContext, machine) == 176);
    const _: () = assert!(offset_of!(MachineContext, registers) == 8);
}
