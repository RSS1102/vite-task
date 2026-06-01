use std::{ffi::OsString, process::ExitCode};

use clap::Parser as _;
use vite_path::{AbsolutePathBuf, current_dir};
use vite_str::Str;
use worldline::{
    cli::Cli,
    ignore::IgnoreSet,
    run::{Captured, RunOptions, run},
    serve::{reconstruct, serve},
};

fn main() -> ExitCode {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    match runtime.block_on(run_main()) {
        Ok(code) => code,
        Err(err) => {
            report(&err);
            ExitCode::FAILURE
        }
    }
}

async fn run_main() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();

    let mut command = cli.command.into_iter();
    let program = command.next().expect("clap requires at least one command argument");
    let args: Vec<OsString> = command.collect();

    let cwd = current_dir()?;
    let patterns: Vec<Str> =
        cli.ignore.iter().map(|p| Str::from(p.to_string_lossy().as_ref())).collect();
    let ignore = IgnoreSet::new(cwd.clone(), !cli.no_default_ignores, &patterns)?;

    let captured = run(RunOptions { program, args, cwd: cwd.clone(), ignore }).await?;
    let exit_code = child_exit_code(&captured);

    if let Some(dir) = cli.dump {
        dump(&captured, &cwd, &dir)?;
    } else if cli.no_serve {
        summary(&captured);
    } else {
        serve(&captured, cli.port, !cli.no_open).await?;
    }

    Ok(exit_code)
}

/// Map the child's exit code into an [`ExitCode`] (defaulting to failure when
/// the child was killed by a signal).
fn child_exit_code(captured: &Captured) -> ExitCode {
    captured.meta.exit_code.map_or(ExitCode::FAILURE, |code| {
        ExitCode::from(u8::try_from(code.rem_euclid(256)).unwrap_or(1))
    })
}

/// Write the reconstructed timeline JSON and raw output log to `dir` (resolved
/// relative to `cwd`).
fn dump(captured: &Captured, cwd: &AbsolutePathBuf, dir: &OsString) -> anyhow::Result<()> {
    let dir = cwd.join(dir);
    std::fs::create_dir_all(&dir)?;
    let api = reconstruct(captured);
    std::fs::write(dir.join("worldline.json"), serde_json::to_vec_pretty(&api)?)?;
    std::fs::write(dir.join("output.bin"), &captured.data.output)?;
    Ok(())
}

#[expect(clippy::print_stdout, reason = "worldline is a user-facing CLI")]
fn summary(captured: &Captured) {
    println!(
        "worldline: captured {} file-write event(s), {} byte(s) of output (exit {})",
        captured.data.events.len(),
        captured.data.output.len(),
        captured.meta.exit_code.map_or_else(|| "signal".to_owned(), |c| c.to_string()),
    );
}

#[expect(clippy::print_stderr, reason = "worldline is a user-facing CLI")]
fn report(err: &anyhow::Error) {
    eprintln!("worldline: {err:#}");
}
