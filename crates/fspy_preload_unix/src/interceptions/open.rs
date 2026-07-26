//! Interceptions for the open family.
//!
//! These report *after* forwarding to the real function, so the recorded access
//! carries whether it succeeded. That distinction matters: a tool probing for a
//! generated file it is about to write is not depending on that file, and
//! counting the failed probe as a read makes a freshly generated output look
//! like an input it read before writing.

use fspy_shared::ipc::AccessMode;
use libc::FILE;

use crate::{
    client::{
        convert::{ModeStr, OpenFlags, PathAt, ToAccessMode as _},
        handle_outcome,
    },
    libc::{c_char, c_int},
    macros::intercept,
};

const fn has_mode_arg(o_flags: c_int) -> bool {
    if o_flags & libc::O_CREAT != 0 {
        return true;
    }
    #[cfg(target_os = "linux")]
    if o_flags & libc::O_TMPFILE != 0 {
        return true;
    }
    false
}

#[cfg(not(target_os = "macos"))]
type Mode = libc::mode_t;
#[cfg(target_os = "macos")] // https://github.com/tailhook/openat/issues/21#issuecomment-535914957
type Mode = c_int;

/// The access mode implied by open flags, resolved before the call because the
/// flags are not affected by its outcome.
fn open_mode(flags: c_int) -> AccessMode {
    // SAFETY: OpenFlags holds a plain integer, so no pointer is dereferenced
    unsafe { OpenFlags(flags).to_access_mode() }
}

intercept!(open(64): unsafe extern "C" fn(*const c_char, c_int, args: ...) -> c_int);
unsafe extern "C" fn open(path: *const c_char, flags: c_int, mut args: ...) -> c_int {
    let mode = open_mode(flags);
    let result = if has_mode_arg(flags) {
        // SAFETY: when O_CREAT or O_TMPFILE is set, a mode_t argument is required by the open() contract
        let file_mode: Mode = unsafe { args.next_arg() };
        // SAFETY: calling the original libc open() with the same arguments forwarded from the interposed function
        unsafe { open::original()(path, flags, file_mode) }
    } else {
        // SAFETY: calling the original libc open() with the same arguments forwarded from the interposed function
        unsafe { open::original()(path, flags) }
    };
    // SAFETY: path is a valid C string pointer provided by the caller of the interposed function
    unsafe { handle_outcome(path, mode, result >= 0) };
    result
}

intercept!(openat(64): unsafe extern "C" fn(c_int, *const c_char, c_int, ...) -> c_int);
unsafe extern "C" fn openat(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
    mut args: ...
) -> c_int {
    let mode = open_mode(flags);
    let result = if has_mode_arg(flags) {
        // https://github.com/tailhook/openat/issues/21#issuecomment-535914957
        // SAFETY: when O_CREAT or O_TMPFILE is set, a mode_t argument is required by the openat() contract
        let file_mode: Mode = unsafe { args.next_arg() };
        // SAFETY: calling the original libc openat() with the same arguments forwarded from the interposed function
        unsafe { openat::original()(dirfd, path, flags, file_mode) }
    } else {
        // SAFETY: calling the original libc openat() with the same arguments forwarded from the interposed function
        unsafe { openat::original()(dirfd, path, flags) }
    };
    // SAFETY: dirfd and path are valid arguments provided by the caller of the interposed function
    unsafe { handle_outcome(PathAt(dirfd, path), mode, result >= 0) };
    result
}

#[cfg(target_os = "macos")]
intercept!(open_nocancel: unsafe extern "C" fn(*const c_char, c_int, ...) -> c_int);
#[cfg(target_os = "macos")]
unsafe extern "C" fn open_nocancel(path: *const c_char, flags: c_int, mut args: ...) -> c_int {
    let mode = open_mode(flags);
    let result = if has_mode_arg(flags) {
        // SAFETY: O_CREAT requires a mode argument, matching the open$NOCANCEL contract
        let file_mode: Mode = unsafe { args.next_arg() };
        // SAFETY: calling the original libc open$NOCANCEL() with the same arguments forwarded from the interposed function
        unsafe { open_nocancel::original()(path, flags, file_mode) }
    } else {
        // SAFETY: calling the original libc open$NOCANCEL() with the same arguments forwarded from the interposed function
        unsafe { open_nocancel::original()(path, flags) }
    };
    // SAFETY: path is a valid C string pointer provided by the caller of open$NOCANCEL
    unsafe { handle_outcome(path, mode, result >= 0) };
    result
}

#[cfg(target_os = "macos")]
intercept!(openat_nocancel: unsafe extern "C" fn(c_int, *const c_char, c_int, ...) -> c_int);
#[cfg(target_os = "macos")]
unsafe extern "C" fn openat_nocancel(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
    mut args: ...
) -> c_int {
    let mode = open_mode(flags);
    let result = if has_mode_arg(flags) {
        // SAFETY: O_CREAT requires a mode argument, matching the openat$NOCANCEL contract
        let file_mode: Mode = unsafe { args.next_arg() };
        // SAFETY: calling the original libc openat$NOCANCEL() with the same arguments forwarded from the interposed function
        unsafe { openat_nocancel::original()(dirfd, path, flags, file_mode) }
    } else {
        // SAFETY: calling the original libc openat$NOCANCEL() with the same arguments forwarded from the interposed function
        unsafe { openat_nocancel::original()(dirfd, path, flags) }
    };
    // SAFETY: dirfd and path are valid arguments provided by the caller of openat$NOCANCEL
    unsafe { handle_outcome(PathAt(dirfd, path), mode, result >= 0) };
    result
}

intercept!(fopen(64): unsafe extern "C" fn(path: *const c_char, mode: *const c_char) -> *mut FILE);
unsafe extern "C" fn fopen(path: *const c_char, mode: *const c_char) -> *mut libc::FILE {
    // SAFETY: mode is a valid C string pointer provided by the caller of the interposed function
    let access_mode = unsafe { ModeStr(mode).to_access_mode() };
    // SAFETY: calling the original libc fopen() with the same arguments forwarded from the interposed function
    let result = unsafe { fopen::original()(path, mode) };
    // SAFETY: path is a valid C string pointer provided by the caller of the interposed function
    unsafe { handle_outcome(path, access_mode, !result.is_null()) };
    result
}

intercept!(freopen(64): unsafe extern "C" fn(path: *const c_char, mode: *const c_char, stream: *mut FILE) -> *mut FILE);
unsafe extern "C" fn freopen(
    path: *const c_char,
    mode: *const c_char,
    stream: *mut FILE,
) -> *mut FILE {
    // SAFETY: mode is a valid C string pointer provided by the caller of the interposed function
    let access_mode = unsafe { ModeStr(mode).to_access_mode() };
    // SAFETY: calling the original libc freopen() with the same arguments forwarded from the interposed function
    let result = unsafe { freopen::original()(path, mode, stream) };
    // SAFETY: path is a valid C string pointer provided by the caller of the interposed function
    unsafe { handle_outcome(path, access_mode, !result.is_null()) };
    result
}
