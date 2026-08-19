//! Data shared by the ptrace injector and its freestanding payload.

#![no_std]

/// Registers the payload needs to resume an x86-64 program at its exec entry.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy)]
#[repr(C)]
pub struct ResumeContext {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rsp: u64,
    pub rip: u64,
    pub rflags: u64,
}

/// Registers the payload needs to resume an `AArch64` program at its exec entry.
///
/// The resume trampoline uses `x16` for the final indirect branch, so that one
/// register intentionally contains `pc` rather than `registers[16]` afterward.
#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy)]
#[repr(C)]
pub struct ResumeContext {
    pub registers: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
}

impl ResumeContext {
    /// Returns the native-endian in-memory representation copied into the
    /// target. The supervisor and payload always have the same architecture.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        // SAFETY: both architecture-specific definitions are `repr(C)` and
        // consist entirely of `u64` fields, so every byte is initialized and
        // there is no padding.
        unsafe {
            core::slice::from_raw_parts(
                core::ptr::from_ref(self).cast(),
                core::mem::size_of::<Self>(),
            )
        }
    }
}

#[cfg(target_arch = "x86_64")]
const _: () = assert!(core::mem::size_of::<ResumeContext>() == 18 * size_of::<u64>());

#[cfg(target_arch = "aarch64")]
const _: () = assert!(core::mem::size_of::<ResumeContext>() == 34 * size_of::<u64>());
