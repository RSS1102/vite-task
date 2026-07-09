#![cfg_attr(feature = "portable-pty", feature(command_resolved_envs))]

use std::{env::current_exe, ffi::OsString, process::Command};

use base64::{Engine, prelude::BASE64_STANDARD_NO_PAD};
use wincode::{SchemaReadOwned, SchemaWrite, config::DefaultConfig};

/// Copies the program, ordinary arguments, resolved environment, and cwd into a PTY command.
/// Windows raw arguments are not preserved because [`Command`] does not expose them.
///
/// # Panics
///
/// Panics if the current directory cannot be read when the command does not configure one.
#[cfg(feature = "portable-pty")]
#[must_use]
pub fn portable_pty_command_builder(command: &Command) -> portable_pty::CommandBuilder {
    let mut builder = portable_pty::CommandBuilder::new(command.get_program());
    builder.args(command.get_args());
    builder.env_clear();
    for (key, value) in command.get_resolved_envs() {
        builder.env(key, value);
    }
    builder.cwd(command.get_current_dir().map_or_else(
        || std::env::current_dir().expect("failed to get current dir"),
        std::path::Path::to_path_buf,
    ));
    builder
}

/// Creates a [`std::process::Command`] that only executes the provided function.
///
/// - $arg: The argument to pass to the function, must implement `SchemaWrite` and `SchemaReadOwned`.
/// - $f: The function to run in the separate process, takes one argument of the type of $arg.
#[macro_export]
macro_rules! command_for_fn {
    ($arg: expr, $f: expr) => {{
        // Generate a unique ID for every invocation of this macro.
        const ID: &str =
            ::core::concat!(::core::file!(), ":", ::core::line!(), ":", ::core::column!());

        fn assert_arg_type<A>(_arg: &A, _f: impl FnOnce(A)) {}
        assert_arg_type(&$arg, $f);

        // Register an initializer that runs the provided function when the process is started
        #[::ctor::ctor(unsafe)]
        unsafe fn init() {
            $crate::init_impl(ID, $f);
        }
        // Create the command
        $crate::create_command(ID, $arg)
    }};
}

/// Read command-line arguments in a way that works during `.init_array`.
///
/// On Linux, `std::env::args()` may return empty during `.init_array`
/// constructors (observed on musl targets) because the Rust runtime hasn't
/// initialized its argument storage yet. We fall back to reading
/// `/proc/self/cmdline` directly.
fn get_args() -> Vec<String> {
    let args: Vec<String> = std::env::args().collect();
    if !args.is_empty() {
        return args;
    }

    // Fallback: read /proc/self/cmdline directly.
    #[cfg(target_os = "linux")]
    {
        if let Some(args) = read_proc_cmdline() {
            return args;
        }
    }

    args
}

/// Read `/proc/self/cmdline` as a fallback that works before Rust runtime
/// initialization (during `.init_array` constructors).
#[cfg(target_os = "linux")]
fn read_proc_cmdline() -> Option<Vec<String>> {
    let buf = std::fs::read("/proc/self/cmdline").ok()?;
    if buf.is_empty() {
        return None;
    }

    // /proc/self/cmdline has null-separated args with a trailing null.
    // We must preserve empty args (e.g., empty base64 for `()` arg) but
    // remove the trailing empty entry from the final null terminator.
    let mut args: Vec<String> = buf
        .split(|&b| b == 0)
        .filter_map(|s| std::str::from_utf8(s).ok().map(String::from))
        .collect();
    // Remove trailing empty string from the final null byte
    if args.last().is_some_and(String::is_empty) {
        args.pop();
    }
    Some(args)
}

#[doc(hidden)]
pub fn init_impl<A: SchemaReadOwned<DefaultConfig, Dst = A>>(expected_id: &str, f: impl FnOnce(A)) {
    let args = get_args();
    // <test_binary> <expected_id> <arg_base64>
    let (Some(current_id), Some(arg_base64)) = (args.get(1), args.get(2)) else {
        return;
    };
    if current_id != expected_id {
        return;
    }
    let arg_bytes = BASE64_STANDARD_NO_PAD.decode(arg_base64).expect("Failed to decode base64 arg");
    let arg: A = wincode::deserialize(&arg_bytes).expect("Failed to decode wincode arg");
    f(arg);
    std::process::exit(0);
}

#[doc(hidden)]
pub fn create_command<T: SchemaWrite<DefaultConfig, Src = T>>(id: &str, arg: T) -> Command {
    let program = current_exe().unwrap().into_os_string();
    let arg_bytes = wincode::serialize(&arg).expect("Failed to encode arg");
    let arg_base64 = BASE64_STANDARD_NO_PAD.encode(&arg_bytes);

    let mut command = Command::new(program);
    command.args([OsString::from(id), OsString::from(arg_base64)]);
    command.env_clear().envs(std::env::vars_os());
    command.current_dir(std::env::current_dir().unwrap());
    command
}

#[cfg(test)]
mod tests {
    use std::str::from_utf8;

    #[test]
    #[expect(clippy::print_stdout, reason = "test diagnostics")]
    fn test_command_for_fn() {
        let mut command = command_for_fn!(42u32, |arg: u32| {
            print!("{arg}");
        });
        let output = command.output().unwrap();
        assert_eq!(from_utf8(&output.stdout), Ok("42"));
        assert!(output.status.success());
    }

    #[cfg(feature = "portable-pty")]
    #[test]
    fn converts_std_command_to_portable_pty() {
        use std::{ffi::OsStr, process::Command};

        let cwd = std::env::current_dir().unwrap();
        let mut command = Command::new("program");
        command
            .args(["first", "second"])
            .env_clear()
            .env("KEEP", "value")
            .env("REMOVE", "unused")
            .env_remove("REMOVE")
            .current_dir(&cwd);

        let builder = crate::portable_pty_command_builder(&command);
        assert_eq!(builder.get_argv(), &["program", "first", "second"]);
        assert_eq!(builder.get_env("KEEP"), Some(OsStr::new("value")));
        assert_eq!(builder.get_env("REMOVE"), None);
        assert_eq!(builder.get_cwd(), Some(&cwd.into_os_string()));
    }

    #[cfg(feature = "portable-pty")]
    #[test]
    fn resolves_inherited_environment_and_cwd_for_portable_pty() {
        let command = std::process::Command::new("program");
        let builder = crate::portable_pty_command_builder(&command);

        assert_eq!(builder.get_env("PATH"), std::env::var_os("PATH").as_deref(),);
        assert_eq!(builder.get_cwd(), Some(&std::env::current_dir().unwrap().into_os_string()),);
    }
}
