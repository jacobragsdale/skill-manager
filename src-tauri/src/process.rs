//! Non-interactive process execution with timeout and bounded output.

use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

const MAX_CAPTURED_STREAM_BYTES: usize = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug)]
pub(crate) struct ProcessOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

pub(crate) fn command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command.stdin(Stdio::null());
    hide_console(&mut command);
    configure_process_group(&mut command);
    command
}

pub(crate) fn run(
    mut command: Command,
    operation: &str,
    timeout: Duration,
) -> Result<ProcessOutput, String> {
    if timeout.is_zero() {
        return Err("Process timeout must be greater than zero.".to_string());
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut command);
    configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("{operation}: could not start the process: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{operation}: stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{operation}: stderr was not captured"))?;
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout_reader = spawn_reader(stdout, Arc::clone(&exceeded));
    let stderr_reader = spawn_reader(stderr, Arc::clone(&exceeded));
    let started = Instant::now();
    let status = loop {
        if exceeded.load(Ordering::Relaxed) {
            let cleanup = terminate_child(&mut child);
            let _ = join_reader(stdout_reader, operation, "stdout");
            let _ = join_reader(stderr_reader, operation, "stderr");
            return Err(format!(
                "{operation}: output exceeded 1 MB per stream.{cleanup}"
            ));
        }
        if started.elapsed() >= timeout {
            let cleanup = terminate_child(&mut child);
            let _ = join_reader(stdout_reader, operation, "stdout");
            let _ = join_reader(stderr_reader, operation, "stderr");
            return Err(format!(
                "{operation}: timed out after {} seconds.{cleanup}",
                timeout.as_secs()
            ));
        }
        match child
            .try_wait()
            .map_err(|error| format!("{operation}: could not wait for the process: {error}"))?
        {
            Some(status) => break status,
            None => thread::sleep(POLL_INTERVAL),
        }
    };
    let stdout = join_reader(stdout_reader, operation, "stdout")?;
    let stderr = join_reader(stderr_reader, operation, "stderr")?;
    if exceeded.load(Ordering::Relaxed) {
        return Err(format!("{operation}: output exceeded 1 MB per stream."));
    }
    Ok(ProcessOutput {
        status,
        stdout,
        stderr,
    })
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    exceeded: Arc<AtomicBool>,
) -> thread::JoinHandle<Result<Vec<u8>, String>> {
    thread::spawn(move || {
        let mut captured = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| format!("Could not read process output: {error}"))?;
            if read == 0 {
                break;
            }
            let remaining = MAX_CAPTURED_STREAM_BYTES.saturating_sub(captured.len());
            let retained = read.min(remaining);
            captured.extend_from_slice(&buffer[..retained]);
            if retained < read {
                exceeded.store(true, Ordering::Relaxed);
            }
        }
        Ok(captured)
    })
}

fn join_reader(
    reader: thread::JoinHandle<Result<Vec<u8>, String>>,
    operation: &str,
    stream: &str,
) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| format!("{operation}: {stream} reader panicked"))?
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

#[cfg(unix)]
fn terminate_process_tree(child: &Child) -> io::Result<()> {
    let status = process_group_kill_command(child.id()).status()?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| io::Error::other("kill returned a failure status"))
}

#[cfg(unix)]
fn process_group_kill_command(process_group: u32) -> Command {
    let mut command = Command::new("kill");
    command
        // Without `--`, procps-ng parses a negative process-group ID as an
        // option and can turn it into `kill(-1, SIGKILL)`.
        .args(["-KILL", "--", &format!("-{process_group}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[cfg(windows)]
fn terminate_process_tree(child: &Child) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    let status = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &child.id().to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| io::Error::other("taskkill returned a failure status"))
}

fn terminate_child(child: &mut Child) -> String {
    let errors = [
        terminate_process_tree(child).err(),
        child.kill().err(),
        child.wait().err(),
    ]
    .into_iter()
    .flatten()
    .map(|error| error.to_string())
    .collect::<Vec<_>>();
    if errors.is_empty() {
        String::new()
    } else {
        format!(" Process cleanup also reported: {}.", errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_command(script: &str) -> Command {
        #[cfg(unix)]
        {
            let mut command = command(Path::new("/bin/sh"));
            command.args(["-c", script]);
            command
        }
        #[cfg(windows)]
        {
            let mut command = command(Path::new("cmd.exe"));
            // `/S` rewrites the quoting around the `/C` payload and can consume
            // quotes that belong to the script itself.
            command.args(["/D", "/C", script]);
            command
        }
    }

    fn capture_command() -> Command {
        #[cfg(unix)]
        {
            shell_command("if read value; then exit 9; fi; printf stdout; printf stderr >&2")
        }
        #[cfg(windows)]
        {
            let mut command = command(Path::new("powershell.exe"));
            command.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$value = [Console]::In.ReadToEnd(); if ($value.Length -ne 0) { exit 9 }; [Console]::Out.Write('stdout'); [Console]::Error.Write('stderr')",
            ]);
            command
        }
    }

    #[test]
    fn captures_streams_and_closes_stdin() {
        let output =
            run(capture_command(), "capture", Duration::from_secs(5)).expect("process output");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"stdout");
        assert_eq!(output.stderr, b"stderr");
    }

    #[test]
    fn timeout_terminates_process() {
        #[cfg(unix)]
        let script = "sleep 5";
        #[cfg(windows)]
        let script = "ping -n 6 127.0.0.1 > nul";
        assert!(
            run(shell_command(script), "timeout", Duration::from_secs(1))
                .expect_err("timeout")
                .contains("timed out")
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_group_kill_disambiguates_the_negative_id() {
        let command = process_group_kill_command(42);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["-KILL", "--", "-42"]
        );
    }
}
