//! Interceptions for calls that change the shape of the tree rather than the
//! contents of a file: rename, unlink, rmdir and mkdir.
//!
//! Without these, a tool that publishes results atomically is invisible. It
//! writes a temporary and renames it over the destination, so the destination
//! never appears in a write event and the temporary — which no longer exists —
//! does. Directory renames are worse: a build that stages into `dist.tmp` and
//! swaps it into place produces no write event for any of its real outputs.
//!
//! All of these report *after* the real call, because only a successful call
//! changed anything. A failed `mkdir` is the common case, since most callers use
//! it as "ensure this exists" and expect `EEXIST`.

use fspy_shared::ipc::AccessMode;

use crate::{
    client::{convert::PathAt, handle_outcome},
    libc::{c_char, c_int},
    macros::intercept,
};

/// Whether a path is a directory, for tagging rename events.
///
/// A consumer needs this to re-attribute writes recorded beneath a staging
/// directory to the published location. Called before the rename, while the
/// source still exists.
unsafe fn is_directory_at(dirfd: c_int, path: *const c_char) -> bool {
    let mut stat_buf: libc::stat = unsafe { core::mem::zeroed() };
    // SAFETY: path is a valid C string pointer and stat_buf is a valid, owned stat struct
    let result = unsafe { libc::fstatat(dirfd, path, &raw mut stat_buf, libc::AT_SYMLINK_NOFOLLOW) };
    result == 0 && (stat_buf.st_mode & libc::S_IFMT) == libc::S_IFDIR
}

/// Report both halves of a rename: the source is gone, the destination now
/// holds whatever the source held.
unsafe fn report_rename(
    old_dirfd: c_int,
    old_path: *const c_char,
    new_dirfd: c_int,
    new_path: *const c_char,
    succeeded: bool,
    was_directory: bool,
) {
    let dir_flag = if was_directory { AccessMode::IS_DIR } else { AccessMode::empty() };
    // SAFETY: old_path and new_path are valid C string pointers from the caller
    unsafe {
        handle_outcome(
            PathAt(old_dirfd, old_path),
            AccessMode::RENAME_FROM | AccessMode::DELETED | dir_flag,
            succeeded,
        );
        handle_outcome(
            PathAt(new_dirfd, new_path),
            AccessMode::RENAME_TO | AccessMode::WRITE | dir_flag,
            succeeded,
        );
    }
}

intercept!(rename: unsafe extern "C" fn(*const c_char, *const c_char) -> c_int);
unsafe extern "C" fn rename(old_path: *const c_char, new_path: *const c_char) -> c_int {
    // SAFETY: old_path is a valid C string pointer provided by the caller
    let was_directory = unsafe { is_directory_at(libc::AT_FDCWD, old_path) };
    // SAFETY: forwarding the caller's arguments to the original libc rename()
    let result = unsafe { rename::original()(old_path, new_path) };
    // SAFETY: both paths remain valid C string pointers after the call
    unsafe {
        report_rename(
            libc::AT_FDCWD,
            old_path,
            libc::AT_FDCWD,
            new_path,
            result == 0,
            was_directory,
        );
    }
    result
}

intercept!(renameat: unsafe extern "C" fn(c_int, *const c_char, c_int, *const c_char) -> c_int);
unsafe extern "C" fn renameat(
    old_dirfd: c_int,
    old_path: *const c_char,
    new_dirfd: c_int,
    new_path: *const c_char,
) -> c_int {
    // SAFETY: old_dirfd and old_path are valid arguments provided by the caller
    let was_directory = unsafe { is_directory_at(old_dirfd, old_path) };
    // SAFETY: forwarding the caller's arguments to the original libc renameat()
    let result = unsafe { renameat::original()(old_dirfd, old_path, new_dirfd, new_path) };
    // SAFETY: both paths remain valid C string pointers after the call
    unsafe {
        report_rename(old_dirfd, old_path, new_dirfd, new_path, result == 0, was_directory);
    }
    result
}

// macOS publishes atomic swaps through renameatx_np with RENAME_SWAP, which
// mutates both paths. Vite's dependency optimizer and rustup both reach it.
#[cfg(target_os = "macos")]
intercept!(renameatx_np: unsafe extern "C" fn(c_int, *const c_char, c_int, *const c_char, libc::c_uint) -> c_int);
#[cfg(target_os = "macos")]
unsafe extern "C" fn renameatx_np(
    old_dirfd: c_int,
    old_path: *const c_char,
    new_dirfd: c_int,
    new_path: *const c_char,
    flags: libc::c_uint,
) -> c_int {
    // SAFETY: old_dirfd and old_path are valid arguments provided by the caller
    let was_directory = unsafe { is_directory_at(old_dirfd, old_path) };
    // SAFETY: forwarding the caller's arguments to the original renameatx_np()
    let result =
        unsafe { renameatx_np::original()(old_dirfd, old_path, new_dirfd, new_path, flags) };
    // SAFETY: both paths remain valid C string pointers after the call
    unsafe {
        report_rename(old_dirfd, old_path, new_dirfd, new_path, result == 0, was_directory);
    }
    result
}

// Linux's renameat2 can also exchange two paths, in which case both sides are
// mutated rather than one replacing the other.
#[cfg(target_os = "linux")]
intercept!(renameat2: unsafe extern "C" fn(c_int, *const c_char, c_int, *const c_char, libc::c_uint) -> c_int);
#[cfg(target_os = "linux")]
unsafe extern "C" fn renameat2(
    old_dirfd: c_int,
    old_path: *const c_char,
    new_dirfd: c_int,
    new_path: *const c_char,
    flags: libc::c_uint,
) -> c_int {
    // SAFETY: old_dirfd and old_path are valid arguments provided by the caller
    let was_directory = unsafe { is_directory_at(old_dirfd, old_path) };
    // SAFETY: forwarding the caller's arguments to the original renameat2()
    let result = unsafe { renameat2::original()(old_dirfd, old_path, new_dirfd, new_path, flags) };
    let exchanged = flags & libc::RENAME_EXCHANGE != 0;
    // SAFETY: both paths remain valid C string pointers after the call
    unsafe {
        report_rename(old_dirfd, old_path, new_dirfd, new_path, result == 0, was_directory);
        if exchanged {
            // An exchange mutates the source too, so it is not simply gone.
            handle_outcome(
                PathAt(old_dirfd, old_path),
                AccessMode::RENAME_TO | AccessMode::WRITE,
                result == 0,
            );
        }
    }
    result
}

intercept!(unlink: unsafe extern "C" fn(*const c_char) -> c_int);
unsafe extern "C" fn unlink(path: *const c_char) -> c_int {
    // SAFETY: forwarding the caller's argument to the original libc unlink()
    let result = unsafe { unlink::original()(path) };
    // SAFETY: path remains a valid C string pointer after the call
    unsafe { handle_outcome(path, AccessMode::DELETED, result == 0) };
    result
}

intercept!(unlinkat: unsafe extern "C" fn(c_int, *const c_char, c_int) -> c_int);
unsafe extern "C" fn unlinkat(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int {
    // SAFETY: forwarding the caller's arguments to the original libc unlinkat()
    let result = unsafe { unlinkat::original()(dirfd, path, flags) };
    let removed_directory = flags & libc::AT_REMOVEDIR != 0;
    let dir_flag = if removed_directory { AccessMode::IS_DIR } else { AccessMode::empty() };
    // SAFETY: path remains a valid C string pointer after the call
    unsafe {
        handle_outcome(PathAt(dirfd, path), AccessMode::DELETED | dir_flag, result == 0);
    }
    result
}

intercept!(remove: unsafe extern "C" fn(*const c_char) -> c_int);
unsafe extern "C" fn remove(path: *const c_char) -> c_int {
    // SAFETY: forwarding the caller's argument to the original libc remove()
    let result = unsafe { remove::original()(path) };
    // SAFETY: path remains a valid C string pointer after the call
    unsafe { handle_outcome(path, AccessMode::DELETED, result == 0) };
    result
}

intercept!(rmdir: unsafe extern "C" fn(*const c_char) -> c_int);
unsafe extern "C" fn rmdir(path: *const c_char) -> c_int {
    // SAFETY: forwarding the caller's argument to the original libc rmdir()
    let result = unsafe { rmdir::original()(path) };
    // SAFETY: path remains a valid C string pointer after the call
    unsafe {
        handle_outcome(path, AccessMode::DELETED | AccessMode::IS_DIR, result == 0);
    }
    result
}

intercept!(mkdir: unsafe extern "C" fn(*const c_char, libc::mode_t) -> c_int);
unsafe extern "C" fn mkdir(path: *const c_char, mode: libc::mode_t) -> c_int {
    // SAFETY: forwarding the caller's arguments to the original libc mkdir()
    let result = unsafe { mkdir::original()(path, mode) };
    // Only a successful mkdir means this run created the directory. Callers
    // routinely ignore EEXIST, and treating that as creation would claim every
    // pre-existing directory.
    // SAFETY: path remains a valid C string pointer after the call
    unsafe {
        handle_outcome(
            path,
            AccessMode::CREATED_DIR | AccessMode::IS_DIR | AccessMode::WRITE,
            result == 0,
        );
    }
    result
}

intercept!(mkdirat: unsafe extern "C" fn(c_int, *const c_char, libc::mode_t) -> c_int);
unsafe extern "C" fn mkdirat(dirfd: c_int, path: *const c_char, mode: libc::mode_t) -> c_int {
    // SAFETY: forwarding the caller's arguments to the original libc mkdirat()
    let result = unsafe { mkdirat::original()(dirfd, path, mode) };
    // SAFETY: path remains a valid C string pointer after the call
    unsafe {
        handle_outcome(
            PathAt(dirfd, path),
            AccessMode::CREATED_DIR | AccessMode::IS_DIR | AccessMode::WRITE,
            result == 0,
        );
    }
    result
}
