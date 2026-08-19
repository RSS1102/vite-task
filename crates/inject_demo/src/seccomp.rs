//! The demo's minimal `openat` seccomp filter.

use std::io;

use fspy_preload_linux::OPENAT_COOKIE;

const fn statement(code: u16, value: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt: 0, jf: 0, k: value }
}

const fn jump(code: u16, value: u32, when_true: u8, when_false: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt: when_true, jf: when_false, k: value }
}

const LOAD_WORD: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
const JUMP_EQUAL: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
const RETURN: u16 = 0x06; // BPF_RET | BPF_K

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

const COOKIE_LOW: u32 = {
    let bytes = OPENAT_COOKIE.to_ne_bytes();
    u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
};
const COOKIE_HIGH: u32 = {
    let bytes = OPENAT_COOKIE.to_ne_bytes();
    u32::from_ne_bytes([bytes[4], bytes[5], bytes[6], bytes[7]])
};

// Offsets in `struct seccomp_data`.
const NUMBER: u32 = 0;
const ARCH: u32 = 4;
const ARGUMENT_5_LOW: u32 = 16 + 4 * 8;
const ARGUMENT_5_HIGH: u32 = ARGUMENT_5_LOW + 4;

const FILTER: [libc::sock_filter; 11] = [
    statement(LOAD_WORD, ARCH),
    jump(JUMP_EQUAL, AUDIT_ARCH, 1, 0),
    statement(RETURN, libc::SECCOMP_RET_KILL_PROCESS),
    statement(LOAD_WORD, NUMBER),
    jump(JUMP_EQUAL, syscalls::Sysno::openat as u32, 0, 5),
    statement(LOAD_WORD, ARGUMENT_5_LOW),
    jump(JUMP_EQUAL, COOKIE_LOW, 0, 2),
    statement(LOAD_WORD, ARGUMENT_5_HIGH),
    jump(JUMP_EQUAL, COOKIE_HIGH, 1, 0),
    statement(RETURN, libc::SECCOMP_RET_TRAP),
    statement(RETURN, libc::SECCOMP_RET_ALLOW),
];

/// Installs a filter that traps `openat` unless argument slot five contains the
/// injected handler's cookie.
pub fn install() -> io::Result<()> {
    // SAFETY: this is `prctl(PR_SET_NO_NEW_PRIVS, 1)` with the unused arguments
    // zeroed.
    unsafe { syscalls::syscall!(syscalls::Sysno::prctl, libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) }
        .map_err(|error| io::Error::from_raw_os_error(error.into_raw()))?;

    // The kernel copies this stack-owned program before `seccomp` returns.
    let mut filter = FILTER;
    let program = libc::sock_fprog { len: 11, filter: filter.as_mut_ptr() };
    // SAFETY: `program` points to the complete, initialized BPF array above.
    unsafe {
        syscalls::syscall!(
            syscalls::Sysno::seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            0,
            std::ptr::from_ref(&program)
        )
    }
    .map(|_| ())
    .map_err(|error| io::Error::from_raw_os_error(error.into_raw()))
}
