//! Git sparse acquisition, tree validation, and file copying.

use crate::catalog_v1::{relative_path, validate_portable_component};
use crate::locator::{self, git_identity_key};
#[cfg(test)]
use crate::locator::{Locator, LocatorKind};
use crate::manifest::{SourceManifest, SOURCE_MANIFEST_FILE};
use crate::parallel;

const REPOSITORY_MANIFEST_FILE: &str = "skill-manager-repository.json";
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const GIT_TIMEOUT: Duration = Duration::from_secs(120);
pub(crate) const MAX_SOURCE_BYTES: u64 = 50 * 1024 * 1024;
pub(crate) const MAX_SOURCE_FILES: usize = 2_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteHead {
    pub(crate) commit: String,
}

struct GitOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

pub(crate) fn repository_url_key(url: &str) -> &str {
    git_identity_key(url)
}

pub(crate) fn query_remote_head(repository_url: &str) -> Result<RemoteHead, String> {
    let repository_url = transport_url(repository_url)?;
    let mut command = git_command();
    command.args(["ls-remote", "--symref"]);
    command.arg(repository_url);
    command.arg("HEAD");
    let output = run_git(command, "Could not query the repository's default branch")?;
    parse_remote_head(&output.stdout)
}

pub(crate) fn clone_manifest_source(
    repository_url: &str,
    staging_path: &Path,
) -> Result<String, String> {
    sparse_clone(repository_url, staging_path)?;
    sparse_checkout(staging_path, &[SOURCE_MANIFEST_FILE])?;
    let manifest_bytes = read_required_git_file(
        staging_path,
        SOURCE_MANIFEST_FILE,
        REPOSITORY_MANIFEST_FILE,
        "This Git repository publishes a source repository catalog, not a source. Add it as a source repository.",
    )?;
    let manifest = SourceManifest::from_slice(&manifest_bytes)?;
    let paths = manifest.referenced_repository_paths();
    let checkout = paths.iter().map(String::as_str).collect::<Vec<_>>();
    sparse_checkout(staging_path, &checkout)?;
    cloned_head(staging_path)
}

pub(crate) fn clone_repository_manifest(
    repository_url: &str,
    staging_path: &Path,
) -> Result<String, String> {
    sparse_clone(repository_url, staging_path)?;
    sparse_checkout(staging_path, &[REPOSITORY_MANIFEST_FILE])?;
    let _ = read_required_git_file(
        staging_path,
        REPOSITORY_MANIFEST_FILE,
        SOURCE_MANIFEST_FILE,
        "This Git repository publishes a Skill Manager source, not a source repository catalog.",
    )?;
    cloned_head(staging_path)
}

fn cloned_head(repository_path: &Path) -> Result<String, String> {
    let mut command = git_command();
    command.arg("-C");
    command.arg(repository_path);
    command.args(["rev-parse", "--verify", "HEAD"]);
    let output = run_git(command, "Could not read the cloned repository commit")?;
    let commit = String::from_utf8(output.stdout)
        .map_err(|_| "Git returned a non-UTF-8 commit identifier.".to_string())?;
    let commit = commit.trim();
    if !valid_commit_sha(commit) {
        return Err("Git returned an invalid commit identifier.".to_string());
    }
    Ok(commit.to_string())
}

fn sparse_clone(repository_url: &str, staging_path: &Path) -> Result<(), String> {
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
    command.arg(repository_url);
    command.arg(staging_path);
    run_git(command, "Could not clone the repository")?;
    Ok(())
}

fn sparse_checkout(staging_path: &Path, paths: &[&str]) -> Result<(), String> {
    let mut command = git_command();
    command.arg("-C");
    command.arg(staging_path);
    command.args(["sparse-checkout", "set", "--no-cone", "--"]);
    command.args(paths);
    run_git(command, "Could not select the requested Git paths")?;
    Ok(())
}

fn read_required_git_file(
    staging_path: &Path,
    required: &str,
    alternative: &str,
    alternative_message: &str,
) -> Result<Vec<u8>, String> {
    match fs::read(staging_path.join(required)) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if git_file_exists(staging_path, alternative)? {
                Err(alternative_message.to_string())
            } else {
                Err(format!(
                    "This repository does not publish the required top-level {required}."
                ))
            }
        }
        Err(error) => Err(format!("Could not read {required}: {error}")),
    }
}

fn git_file_exists(staging_path: &Path, path: &str) -> Result<bool, String> {
    sparse_checkout(staging_path, &[path])?;
    Ok(staging_path.join(path).is_file())
}

fn transport_url(repository_url: &str) -> Result<String, String> {
    #[cfg(not(test))]
    {
        locator::canonicalize_git_url(repository_url)
    }
    #[cfg(test)]
    {
        locator::canonicalize_git_url(repository_url).or_else(|error| {
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
    if staging_path
        .read_dir()
        .map_err(|error| format!("Could not inspect the Git staging directory: {error}"))?
        .next()
        .is_some()
    {
        return Err("The Git staging directory is not empty.".to_string());
    }
    Ok(())
}

fn git_command() -> Command {
    let mut command = crate::process::command(Path::new("git"));
    command.args([
        "-c",
        "core.autocrlf=false",
        "-c",
        "core.eol=lf",
        "-c",
        "credential.interactive=false",
    ]);
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never");
    command
}

fn run_git(command: Command, operation: &str) -> Result<GitOutput, String> {
    let output = crate::process::run(command, "git", GIT_TIMEOUT).map_err(|error| {
        if error.contains("No such file") || error.contains("not found") {
            "System Git is required for sources but was not found on PATH.".to_string()
        } else {
            format!("{operation}: {error}")
        }
    })?;
    let output = GitOutput {
        status: output.status,
        stdout: output.stdout,
        stderr: output.stderr,
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

fn parse_remote_head(stdout: &[u8]) -> Result<RemoteHead, String> {
    let stdout = std::str::from_utf8(stdout)
        .map_err(|_| "Git returned non-UTF-8 remote reference data.".to_string())?;
    let commit = stdout.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        match (fields.next(), fields.next(), fields.next()) {
            (Some(object_id), Some("HEAD"), None) if valid_commit_sha(object_id) => {
                Some(object_id.to_string())
            }
            _ => None,
        }
    });
    commit
        .map(|commit| RemoteHead { commit })
        .ok_or_else(|| "The repository does not advertise a valid HEAD commit.".to_string())
}

pub(crate) fn cache_base_dir() -> Result<PathBuf, String> {
    if let Some(root) = crate::qa_paths::root()? {
        return Ok(root.join("cache/skill-manager"));
    }
    dirs::cache_dir()
        .map(|directory| directory.join("skill-manager"))
        .ok_or_else(|| "Could not find your cache directory.".to_string())
}

pub(crate) fn config_base_dir() -> Result<PathBuf, String> {
    if let Some(root) = crate::qa_paths::root()? {
        return Ok(root.join("config/skill-manager"));
    }
    dirs::config_dir()
        .map(|directory| directory.join("skill-manager"))
        .ok_or_else(|| "Could not find your configuration directory.".to_string())
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        fs::File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("Could not synchronize {}: {error}", path.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

pub(crate) fn valid_commit_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn temporary_path(parent: &Path, label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    parent.join(format!(".{label}-{}-{nonce}", std::process::id()))
}

pub(crate) fn validate_catalog_tree(root: &Path) -> Result<(), String> {
    fn visit(
        root: &Path,
        directory: &Path,
        seen_paths: &mut BTreeSet<String>,
        file_count: &mut usize,
        total_bytes: &mut u64,
    ) -> Result<(), String> {
        for entry in fs::read_dir(directory)
            .map_err(|error| format!("Could not read {}: {error}", directory.display()))?
        {
            let entry = entry
                .map_err(|error| format!("Could not read {}: {error}", directory.display()))?;
            let path = entry.path();
            validate_portable_component(&entry.file_name(), &path)?;
            let relative = relative_path(root, &path)?;
            if !seen_paths.insert(relative.to_lowercase()) {
                return Err(format!(
                    "Source paths collide on a case-insensitive filesystem: {relative}"
                ));
            }
            let file_type = entry
                .file_type()
                .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?;
            if file_type.is_dir() {
                visit(root, &path, seen_paths, file_count, total_bytes)?;
            } else if file_type.is_file() {
                *file_count = file_count
                    .checked_add(1)
                    .ok_or_else(|| "The source contains too many files.".to_string())?;
                if *file_count > MAX_SOURCE_FILES {
                    return Err(format!(
                        "The source contains more than {MAX_SOURCE_FILES} files."
                    ));
                }
                *total_bytes = total_bytes
                    .checked_add(
                        entry
                            .metadata()
                            .map_err(|error| {
                                format!("Could not inspect {}: {error}", path.display())
                            })?
                            .len(),
                    )
                    .ok_or_else(|| "The source is too large.".to_string())?;
                if *total_bytes > MAX_SOURCE_BYTES {
                    return Err("The source expands beyond 50 MB.".to_string());
                }
            } else {
                return Err(format!(
                    "Source entry is not a regular file or directory: {}",
                    path.display()
                ));
            }
        }
        Ok(())
    }
    let mut seen_paths = BTreeSet::new();
    let mut file_count = 0;
    let mut total_bytes = 0;
    visit(
        root,
        root,
        &mut seen_paths,
        &mut file_count,
        &mut total_bytes,
    )
}

fn plan_directory_copy(
    source: &Path,
    target: &Path,
    files: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), String> {
    fs::create_dir(target)
        .map_err(|error| format!("Could not create {}: {error}", target.display()))?;
    let mut entries = fs::read_dir(source)
        .map_err(|error| format!("Could not read {}: {error}", source.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Could not read {}: {error}", source.display()))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let source_path = entry.path();
        let destination = target.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Could not inspect {}: {error}", source_path.display()))?;
        if file_type.is_dir() {
            plan_directory_copy(&source_path, &destination, files)?;
        } else if file_type.is_file() {
            files.push((source_path, destination));
        } else {
            return Err(format!(
                "{} is not a regular file or directory",
                source_path.display()
            ));
        }
    }
    Ok(())
}

pub(crate) fn copy_directory(source: &Path, target: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    plan_directory_copy(source, target, &mut files)?;
    parallel::try_map(&files, |(source_path, destination)| {
        fs::copy(source_path, destination).map_err(|error| {
            format!(
                "Could not copy {} to {}: {error}",
                source_path.display(),
                destination.display()
            )
        })
    })
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(repository: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(repository)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn canonical_identity_ignores_default_ports_and_dot_git() {
        let one = Locator::parse(LocatorKind::Git, "HTTPS://GitHub.COM:443/acme/example.git")
            .expect("identity");
        let two =
            Locator::parse(LocatorKind::Git, "https://github.com/acme/example").expect("identity");
        assert_eq!(one.source_key(), two.source_key());
        assert_eq!(one.url(), "https://github.com/acme/example.git");
    }

    #[test]
    fn sparse_clone_fetches_only_referenced_content() {
        let repository = tempfile::tempdir().expect("repository");
        git(repository.path(), &["init", "--quiet", "-b", "main"]);
        git(
            repository.path(),
            &["config", "user.email", "tests@example.invalid"],
        );
        git(repository.path(), &["config", "user.name", "Tests"]);
        fs::create_dir(repository.path().join("included")).expect("included");
        fs::create_dir(repository.path().join("ignored")).expect("ignored");
        fs::write(
            repository.path().join("included/SKILL.md"),
            "---\nname: included\ndescription: Included skill.\n---\nBody\n",
        )
        .expect("skill");
        fs::write(repository.path().join("included/file.txt"), "included").expect("file");
        fs::write(repository.path().join("ignored/file.txt"), "ignored").expect("file");
        fs::write(
            repository.path().join(SOURCE_MANIFEST_FILE),
            r#"{
              "version": 2,
              "source": { "id": "acme", "name": "Acme", "description": "Test source" },
              "packages": [{
                "id": "included",
                "components": [{"kind": "skill", "path": "included"}]
              }]
            }"#,
        )
        .expect("manifest");
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "source"]);
        let root = tempfile::tempdir().expect("root");
        let staging = root.path().join("clone");
        clone_manifest_source(&repository.path().display().to_string(), &staging)
            .expect("sparse clone");
        assert!(staging.join("included/file.txt").is_file());
        assert!(!staging.join("ignored/file.txt").exists());
    }
}
