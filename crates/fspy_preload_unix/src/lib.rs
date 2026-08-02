// Linux uses the ptrace/SIGSYS backend. Keep this cdylib only for macOS.
#![cfg_attr(target_os = "macos", feature(c_variadic))]

#[cfg(target_os = "macos")]
mod client;
#[cfg(target_os = "macos")]
mod interceptions;
#[cfg(target_os = "macos")]
mod libc;
#[cfg(target_os = "macos")]
mod macros;
