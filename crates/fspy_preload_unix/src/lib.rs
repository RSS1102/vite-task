// Compile as an empty crate on non-unix targets and on musl (where seccomp
// alone handles access tracking). Guarding the feature gate keeps rustc from
// warning about unused features on those targets.
#![cfg_attr(all(unix, not(target_env = "musl")), feature(c_variadic))]

#[cfg(all(unix, not(target_env = "musl")))]
mod client;
#[cfg(all(unix, not(target_env = "musl")))]
mod interceptions;
#[cfg(all(unix, not(target_env = "musl")))]
mod libc;
#[cfg(all(unix, not(target_env = "musl")))]
mod macros;
