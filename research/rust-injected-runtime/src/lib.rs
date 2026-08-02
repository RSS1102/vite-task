#![no_std]

use core::{
    alloc::{GlobalAlloc, Layout},
    arch::{asm, global_asm},
    cell::UnsafeCell,
    ptr::{null_mut, read_unaligned, write_unaligned},
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
};

const ABI_MAGIC: u64 = 0x4653_5059_5254_3031; // "FSPYRT01"
const ABI_VERSION: u32 = 1;
const ARENA_LEN: usize = 64 * 1024;
const MAX_SUPPORTED_ALIGN: usize = 4096;
const SYS_SECCOMP: i32 = 1;
const PROBE_RESULT: usize = 0x5151_5151;

/// The supervisor owns this fixed-size RW mapping. The injected RX blob only
/// contains a patched pointer to it, so the blob has no writable sections.
#[repr(C, align(4096))]
pub struct FixedArena {
    bytes: UnsafeCell<[u8; ARENA_LEN]>,
}

#[repr(C, align(64))]
pub struct RuntimeState {
    pub abi_magic: u64,
    pub abi_version: u32,
    pub state_size: u32,
    pub supervisor_pid: u32,
    pub bridge_signal: u32,
    pub gateway_magic: u64,
    pub trap_count: AtomicUsize,
    pub last_syscall: AtomicUsize,
    arena_next: AtomicUsize,
    arena: FixedArena,
}

impl RuntimeState {
    /// Creates the bytes that the supervisor copies into the separate RW
    /// mapping before it patches `FSPY_STATE_PTR` in the blob image.
    pub const fn new(supervisor_pid: u32, bridge_signal: u32, gateway_magic: u64) -> Self {
        Self {
            abi_magic: ABI_MAGIC,
            abi_version: ABI_VERSION,
            state_size: size_of::<Self>() as u32,
            supervisor_pid,
            bridge_signal,
            gateway_magic,
            trap_count: AtomicUsize::new(0),
            last_syscall: AtomicUsize::new(0),
            arena_next: AtomicUsize::new(0),
            arena: FixedArena { bytes: UnsafeCell::new([0; ARENA_LEN]) },
        }
    }
}

struct FixedBump;

#[global_allocator]
static ALLOCATOR: FixedBump = FixedBump;

/// A monotonic, fixed-capacity allocator for non-fast-path runtime setup.
///
/// Allocation uses only pointer arithmetic and a native `AtomicUsize` CAS.
/// Deallocation is intentionally a no-op. This makes it lock-free,
/// syscall-free, and immune to allocator lock reentrancy, but memory is not
/// reclaimed until the logical exec replaces the runtime.
unsafe impl GlobalAlloc for FixedBump {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align();
        if align > MAX_SUPPORTED_ALIGN {
            return null_mut();
        }

        let state = runtime_state();
        let mut current = state.arena_next.load(Relaxed);
        loop {
            let Some(aligned) = current.checked_add(align - 1).map(|value| value & !(align - 1))
            else {
                return null_mut();
            };
            let Some(end) = aligned.checked_add(layout.size()) else {
                return null_mut();
            };
            if end > ARENA_LEN {
                return null_mut();
            }

            match state.arena_next.compare_exchange_weak(current, end, Relaxed, Relaxed) {
                Ok(_) => {
                    // SAFETY: the successful CAS reserves the disjoint range
                    // [aligned, end), and FixedArena has 4096-byte alignment.
                    return unsafe { state.arena.bytes.get().cast::<u8>().add(aligned) };
                }
                Err(observed) => current = observed,
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

/// Exported only so the research build retains and audits the allocator.
/// Production handler code must use preallocated stack/ring/scratch records
/// instead of allocating in the SIGSYS fast path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fspy_alloc(size: usize, align: usize) -> *mut u8 {
    let Ok(layout) = Layout::from_size_align(size, align) else {
        return null_mut();
    };
    // SAFETY: `layout` was validated above.
    unsafe { ALLOCATOR.alloc(layout) }
}

#[inline(always)]
fn runtime_state() -> &'static RuntimeState {
    let slot: *const *mut RuntimeState;

    #[cfg(target_arch = "x86_64")]
    // SAFETY: the linker script defines this local PC-relative symbol inside
    // the copied blob. The supervisor patches its pointer-sized contents.
    unsafe {
        asm!(
            "lea {slot}, [rip + FSPY_STATE_PTR]",
            slot = out(reg) slot,
            options(nostack, preserves_flags),
        );
    }

    #[cfg(target_arch = "aarch64")]
    // SAFETY: the blob is asserted to remain inside ADR's +/-1 MiB range.
    unsafe {
        asm!(
            "adr {slot}, FSPY_STATE_PTR",
            slot = out(reg) slot,
            options(nostack, preserves_flags),
        );
    }

    // SAFETY: injection is not allowed to install the handler until the slot
    // points at a fully initialized, suitably aligned RuntimeState mapping.
    unsafe { &**slot }
}

/// Minimal ABI probe for the final artifact. A production version dispatches
/// the trapped syscall and writes its raw result at the same ucontext offset.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.fspy_entry")]
pub unsafe extern "C" fn fspy_sigsys_handler(_signal: i32, siginfo: *const u8, ucontext: *mut u8) {
    if siginfo.is_null() || ucontext.is_null() {
        return;
    }

    // Linux's SIGSYS siginfo layout has si_code at byte 8 and si_syscall at
    // byte 24 on both supported 64-bit architectures. Native C offsetof tests
    // must remain an acceptance gate for these constants.
    let code = unsafe { read_unaligned(siginfo.add(8).cast::<i32>()) };
    if code != SYS_SECCOMP {
        return;
    }
    let syscall = unsafe { read_unaligned(siginfo.add(24).cast::<i32>()) };

    let state = runtime_state();
    state.last_syscall.store(syscall as usize, Relaxed);
    state.trap_count.fetch_add(1, Relaxed);

    #[cfg(target_arch = "x86_64")]
    const RETURN_REGISTER_OFFSET: usize = 144; // ucontext.uc_mcontext.rax
    #[cfg(target_arch = "aarch64")]
    const RETURN_REGISTER_OFFSET: usize = 184; // ucontext.uc_mcontext.regs[0]

    // SAFETY: the kernel supplied this ucontext to an SA_SIGINFO handler, and
    // the architecture-specific offset is validated against Linux headers.
    unsafe {
        write_unaligned(ucontext.add(RETURN_REGISTER_OFFSET).cast::<usize>(), PROBE_RESULT);
    }
}

/// Raw six-argument Linux syscall gateway. Do not add `nomem`: the kernel may
/// read or write memory named by the arguments.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fspy_raw_syscall6(
    number: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
) -> isize {
    #[cfg(target_arch = "x86_64")]
    {
        let result: isize;
        // SAFETY: the caller owns the raw Linux syscall contract.
        unsafe {
            asm!(
                "syscall",
                inlateout("rax") number as isize => result,
                in("rdi") arg0,
                in("rsi") arg1,
                in("rdx") arg2,
                in("r10") arg3,
                in("r8") arg4,
                in("r9") arg5,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack),
            );
        }
        result
    }

    #[cfg(target_arch = "aarch64")]
    {
        let result: isize;
        // SAFETY: the caller owns the raw Linux syscall contract.
        unsafe {
            asm!(
                "svc #0",
                in("x8") number,
                inlateout("x0") arg0 as isize => result,
                in("x1") arg1,
                in("x2") arg2,
                in("x3") arg3,
                in("x4") arg4,
                in("x5") arg5,
                options(nostack),
            );
        }
        result
    }
}

#[cfg(target_arch = "x86_64")]
global_asm!(
    ".pushsection .text.fspy_restorer,\"ax\",@progbits",
    ".global fspy_rt_sigreturn",
    ".type fspy_rt_sigreturn,@function",
    "fspy_rt_sigreturn:",
    "mov rax, 15",
    "syscall",
    "ud2",
    ".size fspy_rt_sigreturn, .-fspy_rt_sigreturn",
    ".popsection",
);

#[cfg(target_arch = "aarch64")]
global_asm!(
    ".pushsection .text.fspy_restorer,\"ax\",@progbits",
    ".global fspy_rt_sigreturn",
    ".type fspy_rt_sigreturn,%function",
    "fspy_rt_sigreturn:",
    "mov x8, #139",
    "svc #0",
    "brk #0",
    ".size fspy_rt_sigreturn, .-fspy_rt_sigreturn",
    ".popsection",
);

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    #[cfg(target_arch = "x86_64")]
    const EXIT_GROUP: usize = 231;
    #[cfg(target_arch = "aarch64")]
    const EXIT_GROUP: usize = 94;

    // SAFETY: exit_group does not return and all unused arguments are zero.
    unsafe {
        let _ = fspy_raw_syscall6(EXIT_GROUP, 127, 0, 0, 0, 0, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("the injected runtime only supports x86-64 and AArch64 Linux");
