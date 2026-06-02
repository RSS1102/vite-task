//! `rename` family interception.
//!
//! Surfaces a `RENAMED` callback after a successful rename so a consumer can
//! follow the ubiquitous atomic write-temp-then-`rename` pattern to the file's
//! final name (otherwise only the temporary file's write is observed). Fires
//! only when the rename succeeds; carries no descriptor.

use crate::{
    client::{convert::PathAt, handle_rename},
    libc::{c_char, c_int},
    macros::intercept,
};

intercept!(rename(64): unsafe extern "C" fn(*const c_char, *const c_char) -> c_int);
unsafe extern "C" fn rename(old: *const c_char, new: *const c_char) -> c_int {
    // SAFETY: forwarding to the original libc rename() with the same arguments.
    let ret = unsafe { rename::original()(old, new) };
    if ret == 0 {
        // SAFETY: `old`/`new` are valid C strings provided by the caller.
        unsafe { handle_rename(old, new) };
    }
    ret
}

intercept!(renameat(64): unsafe extern "C" fn(c_int, *const c_char, c_int, *const c_char) -> c_int);
unsafe extern "C" fn renameat(
    old_dirfd: c_int,
    old: *const c_char,
    new_dirfd: c_int,
    new: *const c_char,
) -> c_int {
    // SAFETY: forwarding to the original libc renameat() with the same arguments.
    let ret = unsafe { renameat::original()(old_dirfd, old, new_dirfd, new) };
    if ret == 0 {
        // SAFETY: the dir fds and path pointers are valid arguments from the caller.
        unsafe { handle_rename(PathAt(old_dirfd, old), PathAt(new_dirfd, new)) };
    }
    ret
}

// `renameat2` is glibc-only (absent on musl, where the preload backend is not
// used anyway).
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
mod linux {
    use super::{PathAt, c_char, c_int, handle_rename, intercept};
    use crate::libc::c_uint;

    intercept!(renameat2(64): unsafe extern "C" fn(c_int, *const c_char, c_int, *const c_char, c_uint) -> c_int);
    unsafe extern "C" fn renameat2(
        old_dirfd: c_int,
        old: *const c_char,
        new_dirfd: c_int,
        new: *const c_char,
        flags: c_uint,
    ) -> c_int {
        // SAFETY: forwarding to the original libc renameat2() with the same arguments.
        let ret = unsafe { renameat2::original()(old_dirfd, old, new_dirfd, new, flags) };
        if ret == 0 {
            // SAFETY: the dir fds and path pointers are valid arguments from the caller.
            unsafe { handle_rename(PathAt(old_dirfd, old), PathAt(new_dirfd, new)) };
        }
        ret
    }
}

// macOS atomic-rename variants (`renamex_np`/`renameatx_np`), used by some
// runtimes (including libuv/Bun) to rename with flags.
#[cfg(target_os = "macos")]
mod macos {
    use super::{PathAt, c_char, c_int, handle_rename, intercept};
    use crate::libc::c_uint;

    intercept!(renamex_np: unsafe extern "C" fn(*const c_char, *const c_char, c_uint) -> c_int);
    unsafe extern "C" fn renamex_np(
        old: *const c_char,
        new: *const c_char,
        flags: c_uint,
    ) -> c_int {
        // SAFETY: forwarding to the original libc renamex_np() with the same arguments.
        let ret = unsafe { renamex_np::original()(old, new, flags) };
        if ret == 0 {
            // SAFETY: `old`/`new` are valid C strings provided by the caller.
            unsafe { handle_rename(old, new) };
        }
        ret
    }

    intercept!(renameatx_np: unsafe extern "C" fn(c_int, *const c_char, c_int, *const c_char, c_uint) -> c_int);
    unsafe extern "C" fn renameatx_np(
        old_dirfd: c_int,
        old: *const c_char,
        new_dirfd: c_int,
        new: *const c_char,
        flags: c_uint,
    ) -> c_int {
        // SAFETY: forwarding to the original libc renameatx_np() with the same arguments.
        let ret = unsafe { renameatx_np::original()(old_dirfd, old, new_dirfd, new, flags) };
        if ret == 0 {
            // SAFETY: the dir fds and path pointers are valid arguments from the caller.
            unsafe { handle_rename(PathAt(old_dirfd, old), PathAt(new_dirfd, new)) };
        }
        ret
    }
}
