//! `access` and `faccessat` are pure existence and permission probes, so the
//! outcome is the whole signal. Reported after the call.

use fspy_shared::ipc::AccessMode;
use libc::{c_char, c_int};

use crate::{
    client::{convert::PathAt, handle_outcome},
    macros::intercept,
};

intercept!(access(64): unsafe extern "C" fn(pathname: *const c_char, mode: c_int) -> c_int);
unsafe extern "C" fn access(pathname: *const c_char, mode: c_int) -> c_int {
    // SAFETY: calling the original libc access() with the same arguments forwarded from the interposed function
    let result = unsafe { access::original()(pathname, mode) };
    // SAFETY: pathname is a valid C string pointer provided by the caller of the interposed function
    unsafe { handle_outcome(pathname, AccessMode::READ, result == 0) };
    result
}

intercept!(faccessat(64): unsafe extern "C" fn(dirfd: c_int, pathname: *const c_char, mode: c_int, flags: c_int) -> c_int);
unsafe extern "C" fn faccessat(
    dirfd: c_int,
    pathname: *const c_char,
    mode: c_int,
    flags: c_int,
) -> c_int {
    // SAFETY: calling the original libc faccessat() with the same arguments forwarded from the interposed function
    let result = unsafe { faccessat::original()(dirfd, pathname, mode, flags) };
    // SAFETY: dirfd and pathname are valid arguments provided by the caller of the interposed function
    unsafe { handle_outcome(PathAt(dirfd, pathname), AccessMode::READ, result == 0) };
    result
}
