use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const GIT_TIMEOUT: Duration = Duration::from_secs(120);
const GIT_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_CAPTURED_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GitSourceIdentity {
    pub(crate) canonical_url: String,
    pub(crate) source_id: String,
    pub(crate) display_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteHead {
    pub(crate) branch: String,
    pub(crate) commit: String,
}

#[derive(Debug)]
struct GitOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

struct CaptureDirectory {
    path: Option<PathBuf>,
}

impl CaptureDirectory {
    fn path(&self) -> &Path {
        self.path
            .as_deref()
            .expect("capture directory remains available until cleanup")
    }

    fn cleanup(mut self, operation: &str) {
        let Some(path) = self.path.take() else {
            return;
        };
        if let Err(error) = fs::remove_dir_all(&path) {
            eprintln!(
                "{operation}: could not remove Git output capture {}: {error}",
                path.display()
            );
        }
    }
}

impl Drop for CaptureDirectory {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

pub(crate) fn source_identity(input: &str) -> Result<GitSourceIdentity, String> {
    let canonical_url = canonicalize_repository_url(input)?;
    Ok(GitSourceIdentity {
        source_id: stable_source_id(&canonical_url),
        display_name: repository_display_name(&canonical_url)?,
        canonical_url,
    })
}

pub(crate) fn validate_repository_url(input: &str) -> Result<GitSourceIdentity, String> {
    source_identity(input)
}

pub(crate) fn canonicalize_repository_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    let (scheme, remainder) = input
        .split_once("://")
        .ok_or_else(|| repository_url_error("Use an https:// or ssh:// URL."))?;

    let scheme = if scheme.eq_ignore_ascii_case("https") {
        "https"
    } else if scheme.eq_ignore_ascii_case("ssh") {
        "ssh"
    } else {
        return Err(repository_url_error(
            "Only https:// and ssh:// URLs are supported.",
        ));
    };

    if remainder.is_empty()
        || remainder
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        || remainder.contains('\\')
    {
        return Err(repository_url_error(
            "The URL contains an invalid character.",
        ));
    }

    let (authority, path) = remainder
        .split_once('/')
        .ok_or_else(|| repository_url_error("The URL must include a repository path."))?;
    if authority.is_empty() || path.is_empty() {
        return Err(repository_url_error(
            "The URL must include a host and repository path.",
        ));
    }
    if path.contains('?') || path.contains('#') {
        return Err(repository_url_error(
            "Query strings and fragments are not supported.",
        ));
    }

    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return Err(repository_url_error(
            "The URL must include a repository path.",
        ));
    }

    let (username, host_port) = match authority.rsplit_once('@') {
        Some((userinfo, host_port)) => {
            if userinfo.is_empty() || host_port.is_empty() || userinfo.contains('@') {
                return Err(repository_url_error(
                    "The URL has invalid user information.",
                ));
            }
            if scheme == "https" {
                return Err(repository_url_error(
                    "HTTPS URLs cannot contain usernames or passwords.",
                ));
            }
            let lowercase_userinfo = userinfo.to_ascii_lowercase();
            if userinfo.contains(':') || lowercase_userinfo.contains("%3a") {
                return Err(repository_url_error("SSH URLs cannot contain passwords."));
            }
            (Some(userinfo), host_port)
        }
        None => (None, authority),
    };

    let (host, port) = canonical_host_and_port(host_port, scheme)?;
    let mut canonical = String::with_capacity(input.len());
    canonical.push_str(scheme);
    canonical.push_str("://");
    if let Some(username) = username {
        canonical.push_str(username);
        canonical.push('@');
    }
    canonical.push_str(&host);
    if let Some(port) = port {
        canonical.push(':');
        canonical.push_str(port);
    }
    canonical.push('/');
    canonical.push_str(path);
    Ok(canonical)
}

pub(crate) fn stable_source_id(canonical_url: &str) -> String {
    let identity_url = canonical_url.strip_suffix(".git").unwrap_or(canonical_url);
    let digest = Sha256::digest(identity_url.as_bytes());
    let mut id = String::with_capacity("source-".len() + 16);
    id.push_str("source-");
    for byte in &digest[..8] {
        write!(&mut id, "{byte:02x}").expect("writing to a String cannot fail");
    }
    id
}

pub(crate) fn repository_display_name(canonical_url: &str) -> Result<String, String> {
    let path = canonical_url
        .split_once("://")
        .and_then(|(_, remainder)| remainder.split_once('/'))
        .map(|(_, path)| path)
        .ok_or_else(|| repository_url_error("The URL must include a repository path."))?;
    let last_segment = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let display_name = last_segment
        .strip_suffix(".git")
        .unwrap_or(last_segment)
        .trim();
    if display_name.is_empty() {
        return Err(repository_url_error(
            "The repository path must end with a displayable name.",
        ));
    }
    Ok(display_name.to_string())
}

pub(crate) fn query_remote_head(repository_url: &str) -> Result<RemoteHead, String> {
    let repository_url = transport_url(repository_url)?;
    let mut command = git_command();
    command.args(["ls-remote", "--symref"]);
    command.arg(&repository_url);
    command.arg("HEAD");
    let output = run_git(command, "Could not query the repository's default branch")?;
    parse_remote_head(&output.stdout)
}

pub(crate) fn remote_head(repository_url: &str) -> Result<String, String> {
    query_remote_head(repository_url).map(|head| head.commit)
}

pub(crate) fn clone_default_branch(
    repository_url: &str,
    staging_path: &Path,
) -> Result<String, String> {
    let repository_url = transport_url(repository_url)?;
    ensure_staging_path_is_available(staging_path)?;

    let mut command = git_command();
    command.args([
        "clone",
        "--quiet",
        "--depth",
        "1",
        "--no-tags",
        "--filter=blob:none",
        "--sparse",
    ]);
    command.arg(&repository_url);
    command.arg(staging_path);
    run_git(command, "Could not clone the repository")?;

    let mut sparse_command = git_command();
    sparse_command.arg("-C");
    sparse_command.arg(staging_path);
    sparse_command.args(["sparse-checkout", "set", "skills"]);
    run_git(
        sparse_command,
        "Could not select the repository's skills directory",
    )?;
    cloned_head(staging_path)
}

pub(crate) fn cloned_head(repository_path: &Path) -> Result<String, String> {
    let mut command = git_command();
    command.arg("-C");
    command.arg(repository_path);
    command.args(["rev-parse", "--verify", "HEAD"]);
    let output = run_git(command, "Could not read the cloned repository commit")?;
    let commit = String::from_utf8(output.stdout)
        .map_err(|_| "Git returned a non-UTF-8 commit identifier.".to_string())?;
    let commit = commit.trim();
    if !valid_git_object_id(commit) {
        return Err("Git returned an invalid commit identifier.".to_string());
    }
    Ok(commit.to_string())
}

fn repository_url_error(detail: &str) -> String {
    format!("Invalid repository URL. {detail}")
}

fn canonical_host_and_port<'a>(
    host_port: &'a str,
    scheme: &str,
) -> Result<(String, Option<&'a str>), String> {
    let (host, port) = if let Some(bracketed) = host_port.strip_prefix('[') {
        let closing_bracket = bracketed
            .find(']')
            .ok_or_else(|| repository_url_error("The URL has an invalid IPv6 host."))?;
        let host_end = closing_bracket + 1;
        let host = &host_port[..=host_end];
        let suffix = &host_port[host_end + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(
                suffix
                    .strip_prefix(':')
                    .ok_or_else(|| repository_url_error("The URL has an invalid host."))?,
            )
        };
        (host, port)
    } else {
        if host_port.matches(':').count() > 1 {
            return Err(repository_url_error(
                "IPv6 hosts must be enclosed in brackets.",
            ));
        }
        match host_port.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_port, None),
        }
    };

    if host.is_empty()
        || host == "[]"
        || host
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        || host.contains(['/', '@', '%'])
    {
        return Err(repository_url_error("The URL has an invalid host."));
    }

    let port = match port {
        Some(port) => {
            let parsed_port = port
                .parse::<u16>()
                .map_err(|_| repository_url_error("The URL has an invalid port."))?;
            if parsed_port == 0 {
                return Err(repository_url_error("The URL has an invalid port."));
            }
            let is_default =
                (scheme == "https" && parsed_port == 443) || (scheme == "ssh" && parsed_port == 22);
            (!is_default).then_some(port)
        }
        None => None,
    };

    Ok((host.to_ascii_lowercase(), port))
}

fn transport_url(repository_url: &str) -> Result<String, String> {
    #[cfg(not(test))]
    {
        canonicalize_repository_url(repository_url)
    }
    #[cfg(test)]
    {
        canonicalize_repository_url(repository_url).or_else(|error| {
            if Path::new(repository_url).is_absolute() {
                Ok(repository_url.to_string())
            } else {
                Err(error)
            }
        })
    }
}

fn ensure_staging_path_is_available(staging_path: &Path) -> Result<(), String> {
    if !staging_path.exists() {
        return Ok(());
    }
    if !staging_path.is_dir() {
        return Err("The Git staging path exists and is not a directory.".to_string());
    }
    let is_empty = staging_path
        .read_dir()
        .map_err(|error| format!("Could not inspect the Git staging directory: {error}"))?
        .next()
        .is_none();
    if !is_empty {
        return Err("The Git staging directory is not empty.".to_string());
    }
    Ok(())
}

fn git_command() -> Command {
    let mut command = Command::new("git");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command.env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null());
    command
}

fn run_git(mut command: Command, operation: &str) -> Result<GitOutput, String> {
    let capture_directory = create_capture_directory()?;
    let stdout_path = capture_directory.path().join("stdout");
    let stderr_path = capture_directory.path().join("stderr");
    let stdout_file = create_capture_file(&stdout_path)?;
    let stderr_file = create_capture_file(&stderr_path)?;
    command
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    let spawn_result = command.spawn();
    drop(command);
    let mut child = spawn_result.map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            "System Git is required for custom sources but was not found on PATH.".to_string()
        } else {
            format!("{operation}: could not start Git: {error}")
        }
    })?;

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                match captured_output_is_too_large(&stdout_path, &stderr_path) {
                    Ok(true) => {
                        let cleanup = terminate_child(&mut child);
                        return Err(format!(
                            "{operation}: Git output exceeded {} MB.{cleanup}",
                            MAX_CAPTURED_OUTPUT_BYTES / 1024 / 1024
                        ));
                    }
                    Ok(false) => {}
                    Err(error) => {
                        let _ = terminate_child(&mut child);
                        return Err(format!("{operation}: {error}"));
                    }
                }
                if started.elapsed() >= GIT_TIMEOUT {
                    let cleanup = terminate_child(&mut child);
                    return Err(format!(
                        "{operation}: Git timed out after {} seconds.{cleanup}",
                        GIT_TIMEOUT.as_secs()
                    ));
                }
                std::thread::sleep(GIT_POLL_INTERVAL);
            }
            Err(error) => {
                let _ = terminate_child(&mut child);
                return Err(format!("{operation}: could not monitor Git: {error}"));
            }
        }
    };

    if captured_output_is_too_large(&stdout_path, &stderr_path)? {
        return Err(format!(
            "{operation}: Git output exceeded {} MB.",
            MAX_CAPTURED_OUTPUT_BYTES / 1024 / 1024
        ));
    }
    let stdout = read_capture_file(&stdout_path, "stdout")?;
    let stderr = read_capture_file(&stderr_path, "stderr")?;
    capture_directory.cleanup(operation);
    let output = GitOutput {
        status,
        stdout,
        stderr,
    };
    if output.status.success() {
        Ok(output)
    } else {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        if detail.is_empty() {
            Err(format!("{operation}: Git exited with {}.", output.status))
        } else {
            Err(format!("{operation}: {detail}"))
        }
    }
}

fn create_capture_directory() -> Result<CaptureDirectory, String> {
    let base = std::env::temp_dir();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for suffix in 0..100_u8 {
        let candidate = base.join(format!(
            ".skill-manager-git-{}-{nonce}-{suffix}",
            std::process::id()
        ));
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            let mut builder = builder;
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
            builder
        };
        match builder.create(&candidate) {
            Ok(()) => {
                return Ok(CaptureDirectory {
                    path: Some(candidate),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!(
                    "Could not create Git output capture {}: {error}",
                    candidate.display()
                ));
            }
        }
    }
    Err("Could not choose a unique Git output capture directory.".to_string())
}

fn create_capture_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path).map_err(|error| {
        format!(
            "Could not create Git output capture {}: {error}",
            path.display()
        )
    })
}

fn captured_output_is_too_large(stdout_path: &Path, stderr_path: &Path) -> Result<bool, String> {
    let mut total_bytes = 0_u64;
    for path in [stdout_path, stderr_path] {
        let length = fs::metadata(path)
            .map_err(|error| format!("Could not inspect Git output capture: {error}"))?
            .len();
        total_bytes = total_bytes.saturating_add(length);
        if total_bytes > MAX_CAPTURED_OUTPUT_BYTES as u64 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_capture_file(path: &Path, stream_name: &str) -> Result<Vec<u8>, String> {
    let file = File::open(path)
        .map_err(|error| format!("Could not open Git {stream_name} capture: {error}"))?;
    read_capped(file).map_err(|error| format!("Could not read Git {stream_name}: {error}"))
}

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
        Err(io::Error::other(format!(
            "kill exited with status {status}"
        )))
    }
}

#[cfg(windows)]
fn terminate_process_tree(child: &Child) -> io::Result<()> {
    let status = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &child.id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "taskkill exited with status {status}"
        )))
    }
}

fn optional_io_error(error: Option<io::Error>) -> String {
    error.map_or_else(|| "none".to_string(), |error| error.to_string())
}

fn terminate_child(child: &mut Child) -> String {
    let tree_kill_error = terminate_process_tree(child).err();
    let kill_error = child.kill().err();
    let wait_error = child.wait().err();
    match (tree_kill_error, kill_error, wait_error) {
        (None, None, None) => String::new(),
        (tree_kill_error, kill_error, wait_error) => format!(
            " Process cleanup also failed (tree: {}; kill: {}; wait: {}).",
            optional_io_error(tree_kill_error),
            optional_io_error(kill_error),
            optional_io_error(wait_error)
        ),
    }
}

fn read_capped(mut reader: impl Read) -> io::Result<Vec<u8>> {
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = MAX_CAPTURED_OUTPUT_BYTES.saturating_sub(captured.len());
        captured.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    Ok(captured)
}

fn parse_remote_head(stdout: &[u8]) -> Result<RemoteHead, String> {
    let stdout = std::str::from_utf8(stdout)
        .map_err(|_| "Git returned non-UTF-8 remote reference data.".to_string())?;
    let mut branch = None;
    let mut commit = None;

    for line in stdout.lines() {
        if let Some(reference) = line.strip_prefix("ref: ") {
            let Some((reference, target)) = reference.split_once('\t') else {
                continue;
            };
            if target == "HEAD" {
                branch = reference
                    .strip_prefix("refs/heads/")
                    .map(ToString::to_string);
            }
            continue;
        }

        let mut fields = line.split_whitespace();
        if let (Some(object_id), Some("HEAD"), None) = (fields.next(), fields.next(), fields.next())
        {
            if valid_git_object_id(object_id) {
                commit = Some(object_id.to_string());
            }
        }
    }

    let branch = branch
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| "The repository does not advertise a default branch.".to_string())?;
    let commit = commit
        .ok_or_else(|| "The repository does not advertise a valid HEAD commit.".to_string())?;
    Ok(RemoteHead { branch, commit })
}

fn valid_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
fn local_repository_url(path: &Path) -> String {
    assert!(path.is_absolute(), "test repository path must be absolute");
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn canonicalizes_supported_repository_urls() {
        let https = validate_repository_url(" HTTPS://GitHub.COM:443/acme/skills.git/ ")
            .expect("valid HTTPS repository");
        assert_eq!(https.canonical_url, "https://github.com/acme/skills.git");
        assert_eq!(https.display_name, "skills");
        assert_eq!(
            https.source_id,
            validate_repository_url("https://github.com/acme/skills")
                .expect("same repository without .git")
                .source_id
        );

        let ssh = source_identity("ssh://git@GitHub.COM:22/acme/private-skills.git")
            .expect("valid SSH repository");
        assert_eq!(
            ssh.canonical_url,
            "ssh://git@github.com/acme/private-skills.git"
        );
        assert_eq!(ssh.display_name, "private-skills");
        assert!(ssh.source_id.starts_with("source-"));
        assert_eq!(ssh.source_id.len(), 23);
    }

    #[test]
    fn rejects_unsupported_or_credential_bearing_urls() {
        for invalid in [
            "http://github.com/acme/skills.git",
            "git@github.com:acme/skills.git",
            "https://user@github.com/acme/skills.git",
            "https://user:secret@github.com/acme/skills.git",
            "ssh://git:secret@github.com/acme/skills.git",
            "ssh://git%3Asecret@github.com/acme/skills.git",
            "ssh://github.com",
            "ssh://github.com/acme/skills.git?branch=main",
        ] {
            assert!(
                canonicalize_repository_url(invalid).is_err(),
                "{invalid} should be rejected"
            );
        }
    }

    #[test]
    fn source_ids_are_stable_and_url_specific() {
        let canonical =
            canonicalize_repository_url("https://github.com/acme/skills").expect("valid URL");
        assert_eq!(stable_source_id(&canonical), stable_source_id(&canonical));
        assert_eq!(
            stable_source_id(&canonical),
            stable_source_id(
                &canonicalize_repository_url("https://github.com/acme/skills.git")
                    .expect("valid alias")
            )
        );
        assert_ne!(
            stable_source_id(&canonical),
            stable_source_id("ssh://git@github.com/acme/skills")
        );
    }

    #[test]
    fn preserves_literal_dot_git_transport_paths() {
        let repository_url = "ssh://git@example.com/srv/git/skills.git";
        assert_eq!(
            transport_url(repository_url).expect("valid transport URL"),
            repository_url
        );
    }

    #[test]
    fn queries_and_clones_a_local_repository_default_branch() {
        let temporary = tempfile::tempdir().expect("temporary repository root");
        let remote = temporary.path().join("remote.git");
        let working = temporary.path().join("working");
        let clone = temporary.path().join("clone");

        run_test_git(temporary.path(), ["init", "--bare", path_text(&remote)]);
        run_test_git(temporary.path(), ["init", path_text(&working)]);
        run_test_git(
            &working,
            ["config", "user.email", "skill-manager@example.invalid"],
        );
        run_test_git(&working, ["config", "user.name", "Skill Manager Tests"]);
        let skill = working.join("skills").join("example");
        fs::create_dir_all(&skill).expect("skill directory");
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: example\ndescription: Example\n---\n",
        )
        .expect("skill");
        run_test_git(&working, ["add", "."]);
        run_test_git(&working, ["commit", "--quiet", "-m", "Add example"]);
        run_test_git(&working, ["branch", "-M", "trunk"]);
        run_test_git(&working, ["remote", "add", "origin", path_text(&remote)]);
        run_test_git(&working, ["push", "--quiet", "-u", "origin", "trunk"]);
        run_test_git(
            temporary.path(),
            [
                "--git-dir",
                path_text(&remote),
                "symbolic-ref",
                "HEAD",
                "refs/heads/trunk",
            ],
        );

        let repository_url = local_repository_url(&remote);
        let remote_head = query_remote_head(&repository_url).expect("remote HEAD");
        assert_eq!(remote_head.branch, "trunk");
        assert!(valid_git_object_id(&remote_head.commit));
        assert_eq!(
            super::remote_head(&repository_url).expect("remote HEAD commit"),
            remote_head.commit
        );

        let cloned_commit = clone_default_branch(&repository_url, &clone).expect("shallow clone");
        assert_eq!(cloned_commit, remote_head.commit);
        assert_eq!(
            cloned_head(&clone).expect("cloned HEAD"),
            remote_head.commit
        );
        assert!(clone.join("skills/example/SKILL.md").is_file());
    }

    fn path_text(path: &Path) -> &str {
        path.to_str().expect("UTF-8 test path")
    }

    fn run_test_git<const N: usize>(working_directory: &Path, arguments: [&str; N]) {
        let output = Command::new("git")
            .current_dir(working_directory)
            .args(arguments)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("run test Git");
        assert!(
            output.status.success(),
            "Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
