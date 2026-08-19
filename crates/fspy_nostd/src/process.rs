//! Process operations provided directly by the Linux kernel.

/// Terminates every thread in the current process with `status`.
pub fn exit_group(status: i32) -> ! {
    loop {
        // SAFETY: `exit_group` accepts the status by value and accesses no
        // caller-owned memory. Retry rather than return from this diverging
        // function if the normally non-returning syscall fails.
        unsafe {
            syscalls::raw_syscall!(syscalls::Sysno::exit_group, status);
        }
    }
}
