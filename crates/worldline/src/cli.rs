//! Command-line interface.

use std::ffi::OsString;

use clap::Parser;

/// Run a program under fspy, snapshot every file write into a scrubbable
/// timeline, capture its terminal output, and serve a local web UI.
#[derive(Parser, Debug)]
#[command(name = "worldline", version, about)]
pub struct Cli {
    /// Localhost port to serve on (0 lets the OS choose a free port).
    #[arg(long, default_value_t = 0)]
    pub port: u16,

    /// Glob (relative to the working directory) of paths to ignore. Repeatable;
    /// added on top of the built-in ignore set.
    #[arg(long = "ignore", value_name = "GLOB")]
    pub ignore: Vec<OsString>,

    /// Disable the built-in ignore set (`.git`, `node_modules`, `target`, …).
    #[arg(long)]
    pub no_default_ignores: bool,

    /// Don't open the browser automatically.
    #[arg(long)]
    pub no_open: bool,

    /// Capture only: print a summary and exit without serving.
    #[arg(long)]
    pub no_serve: bool,

    /// Write the captured timeline (JSON) and raw output to a directory, then
    /// exit without serving. Implies `--no-serve`.
    #[arg(long, value_name = "DIR")]
    pub dump: Option<OsString>,

    /// The program to run, followed by its arguments.
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        required = true,
        value_name = "PROGRAM [ARGS...]"
    )]
    pub command: Vec<OsString>,
}
