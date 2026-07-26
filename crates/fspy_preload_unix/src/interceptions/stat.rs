use fspy_shared::ipc::AccessMode;
use libc::{c_char, c_int, stat as stat_struct};

#[cfg(target_os = "linux")]
use crate::client::convert::Fd;
use crate::{
    client::{convert::PathAt, handle_outcome},
    macros::intercept,
};

intercept!(stat(64): unsafe extern "C" fn(path: *const c_char, buf: *mut stat_struct) -> c_int);
unsafe extern "C" fn stat(path: *const c_char, buf: *mut stat_struct) -> c_int {
    // SAFETY: calling the original libc stat() with the same arguments forwarded from the interposed function
    let result = unsafe { stat::original()(path, buf) };
    // SAFETY: path is a valid C string pointer provided by the caller of the interposed function
    unsafe { handle_outcome(path, AccessMode::READ, result == 0) };
    result
}

intercept!(lstat(64): unsafe extern "C" fn(path: *const c_char, buf: *mut stat_struct) -> c_int);
unsafe extern "C" fn lstat(path: *const c_char, buf: *mut stat_struct) -> c_int {
    // TODO: add accessmode ReadNoFollow
    // SAFETY: calling the original libc lstat() with the same arguments forwarded from the interposed function
    let result = unsafe { lstat::original()(path, buf) };
    // SAFETY: path is a valid C string pointer provided by the caller of the interposed function
    unsafe { handle_outcome(path, AccessMode::READ, result == 0) };
    result
}

intercept!(fstatat(64): unsafe extern "C" fn(dirfd: c_int, pathname: *const c_char, buf: *mut stat_struct, flags: c_int) -> c_int);
unsafe extern "C" fn fstatat(
    dirfd: c_int,
    pathname: *const c_char,
    buf: *mut stat_struct,
    flags: c_int,
) -> c_int {
    // SAFETY: calling the original libc fstatat() with the same arguments forwarded from the interposed function
    let result = unsafe { fstatat::original()(dirfd, pathname, buf, flags) };
    // SAFETY: dirfd and pathname are valid arguments provided by the caller of the interposed function
    unsafe { handle_outcome(PathAt(dirfd, pathname), AccessMode::READ, result == 0) };
    result
}

#[cfg(target_os = "linux")]
intercept!(statx: unsafe extern "C" fn(
    dirfd: c_int,
    pathname: *const c_char,
    flags: c_int,
    mask: libc::c_uint,
    statxbuf: *mut libc::statx,
) -> c_int);
#[cfg(target_os = "linux")]
unsafe extern "C" fn statx(
    dirfd: c_int,
    pathname: *const c_char,
    flags: c_int,
    mask: libc::c_uint,
    statxbuf: *mut libc::statx,
) -> c_int {
    let Some(original) = statx::try_original() else {
        // Rust's standard library interprets ENOSYS from its statx availability
        // probe as unsupported and falls back to stat64.
        // SAFETY: __errno_location returns the calling thread's errno storage on Linux.
        unsafe { *libc::__errno_location() = libc::ENOSYS };
        return -1;
    };

    // SAFETY: calling the original libc statx() with the same arguments forwarded from the interposed function
    let result = unsafe { original(dirfd, pathname, flags, mask, statxbuf) };
    if pathname.is_null() {
        if flags & libc::AT_EMPTY_PATH != 0 {
            // SAFETY: dirfd is provided by the statx caller.
            unsafe { handle_outcome(Fd(dirfd), AccessMode::READ, result == 0) };
        }
    } else {
        // SAFETY: pathname is a non-null C string pointer provided by the statx caller.
        unsafe { handle_outcome(PathAt(dirfd, pathname), AccessMode::READ, result == 0) };
    }
    result
}
