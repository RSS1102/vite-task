use fspy_shared::ipc::AccessMode;
use libc::{DIR, c_char, c_int, c_long, c_void};

use crate::{
    client::{convert::Fd, handle_outcome},
    macros::intercept,
};

intercept!(scandir(64): unsafe extern "C" fn (
    dirname: *const c_char,
    namelist: *mut c_void,
    select: *const c_void,
    compar: *const c_void,
) -> c_int);
unsafe extern "C" fn scandir(
    dirname: *const c_char,
    namelist: *mut c_void,
    select: *const c_void,
    compar: *const c_void,
) -> c_int {
    // SAFETY: calling the original libc scandir() with the same arguments forwarded from the interposed function
    let result = unsafe { scandir::original()(dirname, namelist, select, compar) };
    // SAFETY: dirname is a valid C string pointer provided by the caller of the interposed function
    unsafe { handle_outcome(dirname, AccessMode::READ_DIR, result >= 0) };
    result
}

#[cfg(target_os = "macos")]
mod macos_only {
    use super::{AccessMode, Fd, c_char, c_int, c_void, handle_outcome, intercept};

    intercept!(scandir_b: unsafe extern "C" fn (
        dirname: *const c_char,
        namelist: *mut c_void,
        select: *const c_void,
        compar: *const c_void,
    ) -> c_int);
    unsafe extern "C" fn scandir_b(
        dirname: *const c_char,
        namelist: *mut c_void,
        select: *const c_void,
        compar: *const c_void,
    ) -> c_int {
        // SAFETY: calling the original libc scandir_b() with the same arguments forwarded from the interposed function
        let result = unsafe { scandir_b::original()(dirname, namelist, select, compar) };
        // SAFETY: dirname is a valid C string pointer provided by the caller of the interposed function
        unsafe { handle_outcome(dirname, AccessMode::READ_DIR, result >= 0) };
        result
    }

    intercept!(__getdirentries64: unsafe extern "C" fn(c_int, *mut u8, usize, *mut i64) -> isize);
    unsafe extern "C" fn __getdirentries64(
        fd: c_int,
        buf: *mut u8,
        buf_len: usize,
        basep: *mut i64,
    ) -> isize {
        // SAFETY: calling the original libc __getdirentries64() with the same arguments forwarded from the interposed function
        let result = unsafe { __getdirentries64::original()(fd, buf, buf_len, basep) };
        // SAFETY: fd is a valid file descriptor provided by the caller of __getdirentries64
        unsafe { handle_outcome(Fd(fd), AccessMode::READ_DIR, result >= 0) };
        result
    }
}

intercept!(getdirentries(64): unsafe extern "C" fn (fd: c_int, buf: *mut c_char, nbytes: c_int, basep: *mut c_long) -> c_int);
unsafe extern "C" fn getdirentries(
    fd: c_int,
    buf: *mut c_char,
    nbytes: c_int,
    basep: *mut c_long,
) -> c_int {
    // SAFETY: calling the original libc getdirentries() with the same arguments forwarded from the interposed function
    let result = unsafe { getdirentries::original()(fd, buf, nbytes, basep) };
    // SAFETY: fd is a valid file descriptor provided by the caller of the interposed function
    unsafe { handle_outcome(Fd(fd), AccessMode::READ_DIR, result >= 0) };
    result
}

intercept!(fdopendir(64): unsafe extern "C" fn (fd: c_int) -> *mut DIR);
unsafe extern "C" fn fdopendir(fd: c_int) -> *mut DIR {
    // SAFETY: calling the original libc fdopendir() with the same arguments forwarded from the interposed function
    let result = unsafe { fdopendir::original()(fd) };
    // SAFETY: fd is a valid file descriptor provided by the caller of the interposed function
    unsafe { handle_outcome(Fd(fd), AccessMode::READ_DIR, !result.is_null()) };
    result
}

intercept!(opendir(64): unsafe extern "C" fn (*const c_char) -> *mut DIR);
unsafe extern "C" fn opendir(dir_name: *const c_char) -> *mut DIR {
    // SAFETY: calling the original libc opendir() with the same arguments forwarded from the interposed function
    let result = unsafe { opendir::original()(dir_name) };
    // SAFETY: dir_name is a valid C string pointer provided by the caller of the interposed function
    unsafe { handle_outcome(dir_name, AccessMode::READ_DIR, !result.is_null()) };
    result
}
