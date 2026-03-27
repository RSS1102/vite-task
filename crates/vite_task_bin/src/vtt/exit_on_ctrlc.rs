/// exit-on-ctrlc
///
/// Sets up a Ctrl+C handler, emits a "ready" milestone, then waits.
/// When Ctrl+C is received, prints "ctrl-c received" and exits.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
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
