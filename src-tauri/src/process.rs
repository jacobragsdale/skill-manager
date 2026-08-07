//! Hardened non-interactive process execution with bounded durable output.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) const MAX_CAPTURED_STREAM_BYTES: usize = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutputStream {
    Stdout,
    Stderr,
}

pub(crate) type OutputCallback = Arc<dyn Fn(OutputStream, &[u8]) + Send + Sync>;

#[derive(Debug)]
pub(crate) struct ProcessOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) stdout_log: PathBuf,
    pub(crate) stderr_log: PathBuf,
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
    log_root: &Path,
    on_output: OutputCallback,
) -> Result<ProcessOutput, String> {
    if timeout.is_zero() || timeout > Duration::from_secs(60 * 60) {
        return Err("Process timeouts must be between 1 second and 60 minutes.".to_string());
    }
    fs::create_dir_all(log_root).map_err(|error| {
        format!(
            "Could not create process log directory {}: {error}",
            log_root.display()
        )
    })?;
    let log_directory = unique_log_directory(log_root, operation)?;
    let stdout_log = log_directory.join("stdout.log");
    let stderr_log = log_directory.join("stderr.log");

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
    let stdout_reader = spawn_reader(
        stdout,
        stdout_log.clone(),
        OutputStream::Stdout,
        Arc::clone(&exceeded),
        Arc::clone(&on_output),
    );
    let stderr_reader = spawn_reader(
        stderr,
        stderr_log.clone(),
        OutputStream::Stderr,
        Arc::clone(&exceeded),
        on_output,
    );

    let started = Instant::now();
    let status = loop {
        if exceeded.load(Ordering::Relaxed) {
            let termination = terminate_child(&mut child);
            let _ = join_reader(stdout_reader, operation, "stdout");
            let _ = join_reader(stderr_reader, operation, "stderr");
            return Err(format!(
                "{operation}: captured output exceeded 1 MB per stream.{termination} Logs: {}",
                log_directory.display()
            ));
        }
        if started.elapsed() >= timeout {
            let termination = terminate_child(&mut child);
            let _ = join_reader(stdout_reader, operation, "stdout");
            let _ = join_reader(stderr_reader, operation, "stderr");
            return Err(format!(
                "{operation}: timed out after {} seconds.{termination} Logs: {}",
                timeout.as_secs(),
                log_directory.display()
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
    sync_log_directory(&log_directory)?;
    Ok(ProcessOutput {
        status,
        stdout,
        stderr,
        stdout_log,
        stderr_log,
    })
}

fn unique_log_directory(root: &Path, operation: &str) -> Result<PathBuf, String> {
    let safe_operation = operation
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    for suffix in 0..10_000_u16 {
        let name = if suffix == 0 {
            format!("{timestamp}-{safe_operation}")
        } else {
            format!("{timestamp}-{safe_operation}-{suffix}")
        };
        let path = root.join(name);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("Could not create {}: {error}", path.display())),
        }
    }
    Err(format!(
        "Could not choose a unique log directory in {}.",
        root.display()
    ))
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    log_path: PathBuf,
    stream: OutputStream,
    exceeded: Arc<AtomicBool>,
    on_output: OutputCallback,
) -> thread::JoinHandle<Result<Vec<u8>, String>> {
    thread::spawn(move || {
        let mut log = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&log_path)
            .map_err(|error| format!("Could not create {}: {error}", log_path.display()))?;
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
            if retained > 0 {
                log.write_all(&buffer[..retained])
                    .map_err(|error| format!("Could not write {}: {error}", log_path.display()))?;
                captured.extend_from_slice(&buffer[..retained]);
                on_output(stream, &buffer[..retained]);
            }
            if retained < read {
                exceeded.store(true, Ordering::Relaxed);
            }
        }
        log.sync_all()
            .map_err(|error| format!("Could not synchronize {}: {error}", log_path.display()))?;
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
    let status = Command::new("kill")
        .args(["-KILL", &format!("-{}", child.id())])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("kill returned a failure status"))
    }
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
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("taskkill returned a failure status"))
    }
}

fn terminate_child(child: &mut Child) -> String {
    let tree_error = terminate_process_tree(child).err();
    let kill_error = child.kill().err();
    let wait_error = child.wait().err();
    let errors = [tree_error, kill_error, wait_error]
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

#[cfg(unix)]
fn sync_log_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("Could not synchronize {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn sync_log_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

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
            command.args(["/D", "/S", "/C", script]);
            command
        }
    }

    #[test]
    fn captures_streams_to_memory_logs_and_callback() {
        let logs = tempfile::tempdir().expect("logs");
        let streamed = Arc::new(Mutex::new(Vec::new()));
        let streamed_for_callback = Arc::clone(&streamed);
        let callback = Arc::new(move |stream, bytes: &[u8]| {
            streamed_for_callback
                .lock()
                .expect("streamed output")
                .push((stream, bytes.to_vec()));
        });
        let output = run(
            shell_command("printf stdout; printf stderr >&2"),
            "capture",
            Duration::from_secs(5),
            logs.path(),
            callback,
        )
        .expect("process output");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"stdout");
        assert_eq!(output.stderr, b"stderr");
        assert_eq!(fs::read(output.stdout_log).expect("stdout log"), b"stdout");
        assert_eq!(fs::read(output.stderr_log).expect("stderr log"), b"stderr");
        assert!(!streamed.lock().expect("streamed output").is_empty());
    }

    #[test]
    fn stdin_is_closed_and_timeouts_terminate_the_process() {
        let logs = tempfile::tempdir().expect("logs");
        let closed = run(
            shell_command("if read value; then exit 9; else exit 0; fi"),
            "closed-stdin",
            Duration::from_secs(5),
            logs.path(),
            Arc::new(|_, _| {}),
        )
        .expect("closed stdin");
        assert!(closed.status.success());

        let error = run(
            shell_command("sleep 5"),
            "timeout",
            Duration::from_secs(1),
            logs.path(),
            Arc::new(|_, _| {}),
        )
        .expect_err("timeout");
        assert!(error.contains("timed out"));
    }

    #[test]
    fn output_is_bounded_per_stream() {
        let logs = tempfile::tempdir().expect("logs");
        #[cfg(unix)]
        let script = "yes x | head -c 1100000";
        #[cfg(windows)]
        let script = "for /L %i in (1,1,200000) do @echo xxxxxxxxxx";
        let error = run(
            shell_command(script),
            "bounded",
            Duration::from_secs(10),
            logs.path(),
            Arc::new(|_, _| {}),
        )
        .expect_err("bounded output");
        assert!(error.contains("exceeded 1 MB"));
    }
}
