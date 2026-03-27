/// exit-on-ctrlc
///
/// Sets up a Ctrl+C handler, emits a "ready" milestone, then waits.
/// When Ctrl+C is received, prints "ctrl-c received" and exits.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // On Windows, Rust's runtime sets `SetConsoleCtrlHandler(NULL, TRUE)` which
    // ignores CTRL_C_EVENT. This flag is inherited by child processes and takes
    // precedence over registered handlers. Clear it before registering ours.
    #[cfg(windows)]
    {
        // SAFETY: Passing (None, FALSE) removes the "ignore CTRL_C" flag.
        unsafe extern "system" {
            fn SetConsoleCtrlHandler(
                handler: Option<unsafe extern "system" fn(u32) -> i32>,
                add: i32,
            ) -> i32;
        }
        // SAFETY: Clearing the Rust runtime's ignore flag.
        unsafe {
            SetConsoleCtrlHandler(None, 0);
        }
    }

    ctrlc::set_handler(move || {
        use std::io::Write;
        let _ = write!(std::io::stdout(), "ctrl-c received");
        let _ = std::io::stdout().flush();
        std::process::exit(0);
    })?;

    pty_terminal_test_client::mark_milestone("ready");

    loop {
        std::thread::park();
    }
}
