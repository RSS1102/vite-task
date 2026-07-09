use std::process::Stdio;

use tokio::io::AsyncReadExt as _;
use tokio_util::sync::CancellationToken;

#[test_log::test(tokio::test)]
async fn cancellation_kills_tracked_child() -> anyhow::Result<()> {
    let cmd = subprocess_test::command_for_fn!((), |()| {
        use std::io::Write as _;
        // Signal readiness via stdout
        std::io::stdout().write_all(b"ready\n").unwrap();
        std::io::stdout().flush().unwrap();
        // Block on stdin — will be killed by cancellation
        let _ = std::io::stdin().read_line(&mut String::new());
    });
    let token = CancellationToken::new();
    let cmd = tokio::process::Command::from(cmd);
    let mut child = fspy::spawn_with(cmd, token.clone(), |cmd| {
        cmd.stdout(Stdio::piped()).stdin(Stdio::piped());
    })
    .await?;

    // Wait for child to signal readiness
    let mut stdout = child.stdout.take().unwrap();
    let mut buf = vec![0u8; 64];
    let n = stdout.read(&mut buf).await?;
    assert!(std::str::from_utf8(&buf[..n])?.contains("ready"));

    // Cancel — fspy background task calls start_kill
    token.cancel();
    let termination = child.wait_handle.await?;
    assert!(!termination.status.success());
    Ok(())
}

#[test_log::test(tokio::test)]
async fn preserves_configured_environment() -> anyhow::Result<()> {
    let cmd = subprocess_test::command_for_fn!((), |()| {
        assert_eq!(std::env::var_os("FSPY_TEST_ENV").as_deref(), Some(std::ffi::OsStr::new("set")));
        assert_eq!(std::env::var_os("PATH"), None);
    });
    let mut cmd = tokio::process::Command::from(cmd);
    cmd.env("FSPY_TEST_ENV", "set").env_remove("PATH");

    let child = fspy::spawn(cmd, CancellationToken::new()).await?;
    let termination = child.wait_handle.await?;
    assert!(termination.status.success());
    Ok(())
}

#[cfg(unix)]
#[test_log::test(tokio::test)]
async fn resolves_relative_path_from_child_cwd() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let child_command = subprocess_test::command_for_fn!((), |()| {});
    let temp = tempfile::tempdir()?;
    let cwd = temp.path().join("child:cwd");
    let bin_dir = cwd.join("bin");
    std::fs::create_dir(&cwd)?;
    std::fs::create_dir(&bin_dir)?;
    symlink(std::env::current_exe()?, bin_dir.join("child"))?;

    let mut command = tokio::process::Command::new("child");
    command.args(child_command.get_args()).env("PATH", "bin").current_dir(cwd);
    let child = fspy::spawn(command, CancellationToken::new()).await?;
    let termination = child.wait_handle.await?;
    assert!(termination.status.success());
    Ok(())
}

#[cfg(unix)]
#[test_log::test(tokio::test)]
async fn resolves_program_with_default_path() -> anyhow::Result<()> {
    let mut command = tokio::process::Command::new("sh");
    command.env_clear().args(["-c", "test \"$0\" = sh"]);

    let child = fspy::spawn(command, CancellationToken::new()).await?;
    let termination = child.wait_handle.await?;
    assert!(termination.status.success());
    Ok(())
}
