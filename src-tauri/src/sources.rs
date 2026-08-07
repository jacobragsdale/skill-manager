//! Source identity and Git-backed catalog acquisition.

use crate::application::run_blocking;
use crate::catalog::{catalog_contents, relative_path, validate_portable_path_component};
use crate::domain::{
    CatalogContents, CatalogError, CatalogMetadata, CatalogSkill, SourceDefinition, SourceStatus,
    SourcesConfig, BUILT_IN_SOURCE_ID, BUILT_IN_SOURCE_NAME, CATALOG_SOURCE,
};
use crate::ipc::SourceState;
use crate::manifest::{SourceManifest, MAX_MANIFEST_BYTES, SOURCE_MANIFEST_FILE};
use crate::{fs_retry, parallel};
use flate2::read::GzDecoder;
use reqwest::header::{HeaderValue, ACCEPT, ETAG, IF_NONE_MATCH};
use reqwest::StatusCode;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Cursor, Read, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CATALOG_REF_URL: &str =
    "https://api.github.com/repos/jacobragsdale/skillbook/git/ref/heads/main";
const SOURCES_CONFIG_FILE: &str = "sources.json";
const SOURCES_CONFIG_BACKUP_FILE: &str = "sources.json.previous";
pub(crate) const SOURCES_CONFIG_VERSION: u8 = 2;
const CATALOG_METADATA_FILE: &str = ".skill-manager-catalog.json";
pub(crate) const CATALOG_METADATA_VERSION: u8 = 2;
const MAX_ARCHIVE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_REF_RESPONSE_BYTES: u64 = 64 * 1024;
pub(crate) const MAX_EXTRACTED_BYTES: u64 = 50 * 1024 * 1024;
const MAX_CATALOG_FILES: usize = 2_000;

const GIT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GitSourceIdentity {
    pub(crate) canonical_url: String,
    pub(crate) source_key: String,
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
        if let Err(error) = fs_retry::remove_dir_all(&path) {
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
            let _ = fs_retry::remove_dir_all(&path);
        }
    }
}

pub(crate) fn source_identity(input: &str) -> Result<GitSourceIdentity, String> {
    let canonical_url = canonicalize_repository_url(input)?;
    Ok(GitSourceIdentity {
        source_key: stable_source_key(&canonical_url),
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

pub(crate) fn stable_source_key(canonical_url: &str) -> String {
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
        "Could not select the repository's catalog directories",
    )?;
    cloned_head(staging_path)
}

pub(crate) fn clone_manifest_source(
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

    let mut manifest_only = git_command();
    manifest_only.arg("-C");
    manifest_only.arg(staging_path);
    manifest_only.args([
        "sparse-checkout",
        "set",
        "--no-cone",
        "--",
        SOURCE_MANIFEST_FILE,
    ]);
    run_git(
        manifest_only,
        "Could not read the repository's source manifest",
    )?;
    let manifest_path = staging_path.join(SOURCE_MANIFEST_FILE);
    let manifest_bytes = fs::read(&manifest_path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            format!(
                "This repository does not publish the required top-level {SOURCE_MANIFEST_FILE}."
            )
        } else {
            format!("Could not read {SOURCE_MANIFEST_FILE}: {error}")
        }
    })?;
    let manifest = SourceManifest::from_slice(&manifest_bytes)?;

    let mut expand = git_command();
    expand.arg("-C");
    expand.arg(staging_path);
    expand.args(["sparse-checkout", "set", "--no-cone", "--"]);
    expand.args(manifest.referenced_repository_paths());
    run_git(
        expand,
        "Could not select the repository content referenced by skill-manager.json",
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
    let mut command = crate::process::command(Path::new("git"));
    command.args([
        // Git for Windows defaults to `core.autocrlf=true`, which would rewrite
        // every checked-out skill's line endings. That changes the skill digest,
        // so the same repository would produce different digests depending on
        // the machine — and a user who changed the setting would suddenly see
        // every installed skill as modified. Take the repository bytes verbatim.
        "-c",
        "core.autocrlf=false",
        "-c",
        "core.eol=lf",
        // Git Credential Manager answers from its cache when it can, but falls
        // back to a modal sign-in window. GIT_TERMINAL_PROMPT only covers the
        // terminal prompt, not the GUI one.
        "-c",
        "credential.interactive=false",
    ]);
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never");
    command
}

fn run_git(command: Command, operation: &str) -> Result<GitOutput, String> {
    let capture_directory = create_capture_directory()?;
    let output = crate::process::run(
        command,
        "git",
        GIT_TIMEOUT,
        capture_directory.path(),
        Arc::new(|_, _| {}),
    )
    .map_err(|error| {
        if error.contains("No such file") || error.contains("not found") {
            "System Git is required for custom sources but was not found on PATH.".to_string()
        } else {
            format!("{operation}: {error}")
        }
    })?;
    capture_directory.cleanup(operation);
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

#[derive(Debug)]
pub(crate) struct SourceCatalog {
    pub(crate) definition: SourceDefinition,
    pub(crate) state: SourceState,
    pub(crate) path: Option<PathBuf>,
    pub(crate) skills: BTreeMap<String, CatalogSkill>,
}

#[derive(Debug, Deserialize)]
struct GitHubReference {
    object: GitHubReferenceObject,
}

#[derive(Debug, Deserialize)]
struct GitHubReferenceObject {
    sha: String,
}

enum CommitCheck {
    NotModified,
    Current(CatalogMetadata),
}

/// Both variants carry the catalog contents that were read while preparing
/// them. Reading a catalog hashes every file of every skill in it, which is by
/// far the most expensive part of a refresh — on Windows each file open is
/// also an antivirus scan — so the result is threaded through to the caller
/// instead of being recomputed once the catalog is in place.
pub(crate) enum PreparedCatalog {
    Current {
        commit_sha: String,
        contents: CatalogContents,
    },
    Staged {
        commit_sha: String,
        path: PathBuf,
        contents: CatalogContents,
    },
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

pub(crate) fn sources_config_path(config_base: &Path) -> PathBuf {
    config_base.join(SOURCES_CONFIG_FILE)
}

pub(crate) fn sources_config_backup_path(config_base: &Path) -> PathBuf {
    config_base.join(SOURCES_CONFIG_BACKUP_FILE)
}

pub(crate) fn catalogs_root(cache_base: &Path) -> PathBuf {
    cache_base.join("catalogs")
}

pub(crate) fn source_cache_base(cache_base: &Path, source_id: &str) -> PathBuf {
    catalogs_root(cache_base).join(source_id)
}

pub(crate) fn catalog_dir(source_cache: &Path) -> PathBuf {
    source_cache.join("current")
}

pub(crate) fn legacy_catalog_dir(cache_base: &Path) -> PathBuf {
    cache_base.join("catalog")
}

pub(crate) fn catalog_metadata_path(catalog: &Path) -> PathBuf {
    catalog.join(CATALOG_METADATA_FILE)
}

pub(crate) fn read_sources_config(config_base: &Path) -> Result<Vec<SourceDefinition>, String> {
    recover_sources_config(config_base)?;
    let path = sources_config_path(config_base);
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let sources = vec![SourceDefinition::built_in()];
            write_sources_config(config_base, &sources)?;
            return Ok(sources);
        }
        Err(error) => {
            return Err(format!("Could not read {}: {error}", path.display()));
        }
    };
    let config = serde_json::from_slice::<SourcesConfig>(&contents)
        .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
    if !matches!(config.version, 1 | SOURCES_CONFIG_VERSION) {
        return Err(format!(
            "{} uses unsupported source configuration version {}.",
            path.display(),
            config.version
        ));
    }

    let migrated = config.version == 1;
    let mut sources = config.sources;
    if migrated {
        sources.insert(0, SourceDefinition::built_in());
    }
    let mut ids = BTreeSet::new();
    let mut urls = BTreeSet::new();
    for source in &sources {
        let valid_built_in = source.is_built_in()
            && source.name == BUILT_IN_SOURCE_NAME
            && source.url == CATALOG_SOURCE;
        let valid_custom = !source.is_built_in()
            && repository_url_key(&source.url) != repository_url_key(CATALOG_SOURCE)
            && validate_repository_url(&source.url).is_ok_and(|identity| {
                identity.source_key == source.id
                    && identity.display_name == source.name
                    && identity.canonical_url == source.url
            });
        if !valid_built_in && !valid_custom {
            return Err(format!("{} contains an invalid source.", path.display()));
        }
        if !ids.insert(source.id.clone())
            || !urls.insert(repository_url_key(&source.url).to_string())
        {
            return Err(format!(
                "{} contains duplicate source definitions.",
                path.display()
            ));
        }
    }
    if migrated {
        write_sources_config(config_base, &sources)?;
    }
    Ok(sources)
}

pub(crate) fn recover_sources_config(config_base: &Path) -> Result<(), String> {
    let path = sources_config_path(config_base);
    if path.exists() {
        return Ok(());
    }

    let backup = sources_config_backup_path(config_base);
    match fs_retry::rename(&backup, &path) {
        Ok(()) => sync_directory(config_base),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Could not recover {} from {}: {error}",
            path.display(),
            backup.display()
        )),
    }
}

pub(crate) fn repository_url_key(url: &str) -> &str {
    url.strip_suffix(".git").unwrap_or(url)
}

pub(crate) fn write_sources_config(
    config_base: &Path,
    sources: &[SourceDefinition],
) -> Result<(), String> {
    fs::create_dir_all(config_base)
        .map_err(|error| format!("Could not create {}: {error}", config_base.display()))?;
    recover_sources_config(config_base)?;
    let config = SourcesConfig {
        version: SOURCES_CONFIG_VERSION,
        sources: sources.to_vec(),
    };
    let mut contents = serde_json::to_vec_pretty(&config)
        .map_err(|error| format!("Could not create the source configuration: {error}"))?;
    contents.push(b'\n');
    let path = sources_config_path(config_base);
    let staging = temporary_path(config_base, "sources-writing");
    let mut staging_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .map_err(|error| format!("Could not create {}: {error}", staging.display()))?;
    staging_file
        .write_all(&contents)
        .and_then(|()| staging_file.sync_all())
        .map_err(|error| format!("Could not durably write {}: {error}", staging.display()))?;
    drop(staging_file);

    if path.exists() {
        let backup = sources_config_backup_path(config_base);
        if let Err(error) = fs_retry::remove_file(&backup) {
            if error.kind() != std::io::ErrorKind::NotFound {
                let _ = fs_retry::remove_file(&staging);
                return Err(format!(
                    "Could not remove stale source configuration backup {}: {error}",
                    backup.display()
                ));
            }
        }
        fs_retry::rename(&path, &backup).map_err(|error| {
            let _ = fs_retry::remove_file(&staging);
            format!(
                "Could not stage {} for replacement: {error}",
                path.display()
            )
        })?;
        sync_directory(config_base)?;
        if let Err(error) = fs_retry::rename(&staging, &path) {
            let restore = fs_retry::rename(&backup, &path);
            let _ = sync_directory(config_base);
            return match restore {
                Ok(()) => Err(format!("Could not activate {}: {error}", path.display())),
                Err(restore_error) => Err(format!(
                    "Could not activate {} ({error}) or restore it ({restore_error}).",
                    path.display()
                )),
            };
        }
        if let Err(error) = sync_directory(config_base) {
            eprintln!(
                "The sources updated, but the configuration directory could not be synchronized: {error}"
            );
        }
        if let Err(error) = fs_retry::remove_file(&backup) {
            eprintln!(
                "The sources updated, but {} could not be removed: {error}",
                backup.display()
            );
        } else if let Err(error) = sync_directory(config_base) {
            eprintln!(
                "The sources updated, but the configuration directory could not be synchronized after backup cleanup: {error}"
            );
        }
        Ok(())
    } else {
        if let Err(error) = fs_retry::rename(&staging, &path) {
            let _ = fs_retry::remove_file(&staging);
            return Err(format!("Could not activate {}: {error}", path.display()));
        }
        if let Err(error) = sync_directory(config_base) {
            eprintln!(
                "The sources updated, but the configuration directory could not be synchronized: {error}"
            );
        }
        Ok(())
    }
}

#[cfg(unix)]
pub(crate) fn sync_directory(path: &Path) -> Result<(), String> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("Could not durably update {}: {error}", path.display()))
}

#[cfg(not(unix))]
pub(crate) fn sync_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

pub(crate) fn migrate_legacy_catalog(cache_base: &Path) -> Result<(), String> {
    let legacy = legacy_catalog_dir(cache_base);
    if !legacy.is_dir() {
        return Ok(());
    }

    let built_in_cache = source_cache_base(cache_base, BUILT_IN_SOURCE_ID);
    let current = catalog_dir(&built_in_cache);
    if current.is_dir() {
        return Ok(());
    }

    fs::create_dir_all(&built_in_cache)
        .map_err(|error| format!("Could not create {}: {error}", built_in_cache.display()))?;
    fs_retry::rename(&legacy, &current).map_err(|error| {
        format!(
            "Could not migrate the existing skillbook cache from {} to {}: {error}",
            legacy.display(),
            current.display()
        )
    })
}

pub(crate) fn valid_commit_sha(commit_sha: &str) -> bool {
    matches!(commit_sha.len(), 40 | 64)
        && commit_sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn catalog_archive_url(commit_sha: &str) -> Result<String, String> {
    if commit_sha.len() != 40 || !valid_commit_sha(commit_sha) {
        return Err("GitHub returned an invalid skillbook commit SHA.".to_string());
    }

    Ok(format!("{CATALOG_SOURCE}/archive/{commit_sha}.tar.gz"))
}

pub(crate) fn metadata_matches_source(
    metadata: &CatalogMetadata,
    source: &SourceDefinition,
) -> bool {
    let legacy_built_in = metadata.version == 1
        && source.is_built_in()
        && metadata.source_id.is_none()
        && metadata.source == CATALOG_SOURCE;
    let current = metadata.version == CATALOG_METADATA_VERSION
        && metadata.source_id.as_deref() == Some(source.id.as_str())
        && metadata.source == source.url;
    legacy_built_in || current
}

pub(crate) fn read_catalog_metadata(
    catalog: &Path,
    source: &SourceDefinition,
) -> Option<CatalogMetadata> {
    let contents = fs::read(catalog_metadata_path(catalog)).ok()?;
    let metadata = serde_json::from_slice::<CatalogMetadata>(&contents).ok()?;

    if metadata_matches_source(&metadata, source)
        && valid_commit_sha(&metadata.commit_sha)
        && metadata
            .etag
            .as_ref()
            .is_none_or(|etag| etag.len() <= 1024 && HeaderValue::from_str(etag).is_ok())
    {
        Some(metadata)
    } else {
        None
    }
}

pub(crate) fn write_catalog_metadata(
    catalog: &Path,
    metadata: &CatalogMetadata,
) -> Result<(), String> {
    let mut contents = serde_json::to_vec_pretty(metadata)
        .map_err(|error| format!("Could not create the catalog metadata: {error}"))?;
    contents.push(b'\n');
    fs::write(catalog_metadata_path(catalog), contents)
        .map_err(|error| format!("Could not write the catalog metadata: {error}"))
}

pub(crate) fn archive_catalog_path(path: &Path) -> Result<Option<PathBuf>, String> {
    let mut components = Vec::new();

    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(format!(
                "Archive contains an unsafe path: {}",
                path.display()
            ));
        };
        validate_portable_path_component(part, path)?;
        components.push(part);
    }

    if components.len() < 2 || components[1] != OsStr::new("skills") {
        return Ok(None);
    }

    let mut relative = PathBuf::new();
    for component in &components[1..] {
        relative.push(component);
    }

    Ok(Some(relative))
}

pub(crate) fn extract_catalog_archive(
    bytes: &[u8],
    target: &Path,
) -> Result<CatalogContents, String> {
    fs::create_dir_all(target)
        .map_err(|error| format!("Could not create {}: {error}", target.display()))?;
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| format!("Could not read the skillbook archive: {error}"))?;
    let mut portable_paths = BTreeMap::<String, PathBuf>::new();
    let mut extracted_bytes = 0_u64;
    let mut extracted_files = 0_usize;

    for entry in entries {
        let mut entry =
            entry.map_err(|error| format!("Could not read the skillbook archive: {error}"))?;
        let archive_path = entry
            .path()
            .map_err(|error| format!("Archive contains an invalid path: {error}"))?
            .into_owned();
        let Some(relative) = archive_catalog_path(&archive_path)? else {
            continue;
        };
        let portable_key = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
            .collect::<Vec<_>>()
            .join("/");
        if let Some(existing) = portable_paths.get(&portable_key) {
            if existing != &relative {
                return Err(format!(
                    "Archive paths {} and {} collide on Windows",
                    existing.display(),
                    relative.display()
                ));
            }
        } else {
            portable_paths.insert(portable_key, relative.clone());
        }
        let destination = target.join(relative);
        let entry_type = entry.header().entry_type();

        if entry_type.is_dir() {
            fs::create_dir_all(&destination)
                .map_err(|error| format!("Could not create {}: {error}", destination.display()))?;
            continue;
        }
        if !entry_type.is_file() {
            return Err(format!(
                "Archive entry {} is not a regular file or directory",
                archive_path.display()
            ));
        }

        extracted_files += 1;
        if extracted_files > MAX_CATALOG_FILES {
            return Err(format!(
                "The skillbook archive contains more than {MAX_CATALOG_FILES} catalog files."
            ));
        }
        extracted_bytes = extracted_bytes
            .checked_add(entry.size())
            .ok_or_else(|| "The skillbook archive is too large.".to_string())?;
        if extracted_bytes > MAX_EXTRACTED_BYTES {
            return Err(format!(
                "The skillbook archive expands beyond {} MB.",
                MAX_EXTRACTED_BYTES / 1024 / 1024
            ));
        }

        let parent = destination
            .parent()
            .ok_or_else(|| format!("{} has no parent", destination.display()))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&destination)
            .map_err(|error| format!("Could not create {}: {error}", destination.display()))?;
        std::io::copy(&mut entry, &mut output)
            .map_err(|error| format!("Could not write {}: {error}", destination.display()))?;
        output
            .flush()
            .map_err(|error| format!("Could not write {}: {error}", destination.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = entry.header().mode().map_err(|error| {
                format!(
                    "Could not read {} permissions: {error}",
                    archive_path.display()
                )
            })?;
            fs::set_permissions(&destination, fs::Permissions::from_mode(mode & 0o777)).map_err(
                |error| {
                    format!(
                        "Could not set {} permissions: {error}",
                        destination.display()
                    )
                },
            )?;
        }
    }

    catalog_contents(target)
}

pub(crate) fn download_manifest_source_archive(commit_sha: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("skill-manager")
        .build()
        .map_err(|error| format!("Could not initialize the GitHub client: {error}"))?;
    let mut response = client
        .get(catalog_archive_url(commit_sha)?)
        .send()
        .map_err(|error| format!("Could not download skillbook: {error}"))?
        .error_for_status()
        .map_err(|error| format!("GitHub rejected the skillbook download: {error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARCHIVE_BYTES)
    {
        return Err(format!(
            "The skillbook archive exceeds {} MB.",
            MAX_ARCHIVE_BYTES / 1024 / 1024
        ));
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(MAX_ARCHIVE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read the skillbook download: {error}"))?;
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        return Err(format!(
            "The skillbook archive exceeds {} MB.",
            MAX_ARCHIVE_BYTES / 1024 / 1024
        ));
    }
    Ok(bytes)
}

pub(crate) fn built_in_remote_head() -> Result<RemoteHead, String> {
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .timeout(Duration::from_secs(30))
        .user_agent(format!("skill-manager/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("Could not configure the GitHub client: {error}"))?;
    let mut response = client
        .get(CATALOG_REF_URL)
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .map_err(|error| format!("Could not check the skillbook commit: {error}"))?
        .error_for_status()
        .map_err(|error| format!("GitHub rejected the skillbook commit check: {error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_REF_RESPONSE_BYTES)
    {
        return Err("The GitHub reference response is unexpectedly large.".to_string());
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(MAX_REF_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read the skillbook reference response: {error}"))?;
    if bytes.len() as u64 > MAX_REF_RESPONSE_BYTES {
        return Err("The GitHub reference response is unexpectedly large.".to_string());
    }
    let reference = serde_json::from_slice::<GitHubReference>(&bytes)
        .map_err(|error| format!("GitHub returned invalid skillbook commit metadata: {error}"))?;
    if !valid_commit_sha(&reference.object.sha) {
        return Err("GitHub returned an invalid skillbook commit SHA.".to_string());
    }
    Ok(RemoteHead {
        branch: "main".to_string(),
        commit: reference.object.sha,
    })
}

/// Reads a GitHub source archive twice: the first pass discovers and validates
/// the root manifest, and the second extracts only content the manifest can use.
pub(crate) fn extract_manifest_source_archive(
    bytes: &[u8],
    target: &Path,
) -> Result<SourceManifest, String> {
    fs::create_dir_all(target)
        .map_err(|error| format!("Could not create {}: {error}", target.display()))?;
    let manifest_bytes = archive_manifest(bytes)?;
    let manifest = SourceManifest::from_slice(&manifest_bytes)?;
    fs::write(target.join(SOURCE_MANIFEST_FILE), &manifest_bytes)
        .map_err(|error| format!("Could not write the source manifest: {error}"))?;

    let references = manifest.referenced_repository_paths();
    let roots = references
        .iter()
        .map(|reference| archive_reference_root(reference))
        .collect::<Vec<_>>();
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| format!("Could not read the skillbook archive: {error}"))?;
    let mut portable_paths = BTreeMap::<String, PathBuf>::new();
    let mut extracted_bytes = manifest_bytes.len() as u64;
    let mut extracted_files = 1_usize;

    for entry in entries {
        let mut entry =
            entry.map_err(|error| format!("Could not read the skillbook archive: {error}"))?;
        let archive_path = entry
            .path()
            .map_err(|error| format!("Archive contains an invalid path: {error}"))?
            .into_owned();
        let Some(relative) = archive_source_path(&archive_path)? else {
            continue;
        };
        if relative == Path::new(SOURCE_MANIFEST_FILE)
            || !roots
                .iter()
                .any(|root| archive_reference_matches(&relative, root))
        {
            continue;
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            continue;
        }
        if !entry_type.is_file() {
            return Err(format!(
                "Referenced archive entry {} is not a regular file or directory.",
                archive_path.display()
            ));
        }
        let portable_key = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
            .collect::<Vec<_>>()
            .join("/");
        if let Some(existing) = portable_paths.insert(portable_key, relative.clone()) {
            if existing != relative {
                return Err(format!(
                    "Archive paths {} and {} collide on a case-insensitive filesystem.",
                    existing.display(),
                    relative.display()
                ));
            }
        }
        extracted_files = extracted_files
            .checked_add(1)
            .ok_or_else(|| "The source contains too many files.".to_string())?;
        if extracted_files > MAX_CATALOG_FILES {
            return Err(format!(
                "The source archive contains more than {MAX_CATALOG_FILES} referenced files."
            ));
        }
        extracted_bytes = extracted_bytes
            .checked_add(entry.size())
            .ok_or_else(|| "The source archive is too large.".to_string())?;
        if extracted_bytes > MAX_EXTRACTED_BYTES {
            return Err(format!(
                "The source archive expands beyond {} MB.",
                MAX_EXTRACTED_BYTES / 1024 / 1024
            ));
        }
        let destination = target.join(&relative);
        let parent = destination
            .parent()
            .ok_or_else(|| format!("{} has no parent.", destination.display()))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&destination)
            .map_err(|error| format!("Could not create {}: {error}", destination.display()))?;
        std::io::copy(&mut entry, &mut output)
            .and_then(|_| output.flush())
            .map_err(|error| format!("Could not write {}: {error}", destination.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = entry.header().mode().map_err(|error| {
                format!(
                    "Could not read {} permissions: {error}",
                    archive_path.display()
                )
            })?;
            fs::set_permissions(&destination, fs::Permissions::from_mode(mode & 0o777)).map_err(
                |error| {
                    format!(
                        "Could not set {} permissions: {error}",
                        destination.display()
                    )
                },
            )?;
        }
    }
    validate_catalog_tree(target)?;
    Ok(manifest)
}

fn archive_manifest(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| format!("Could not read the skillbook archive: {error}"))?;
    let mut manifest = None;
    for entry in entries {
        let mut entry =
            entry.map_err(|error| format!("Could not read the skillbook archive: {error}"))?;
        let archive_path = entry
            .path()
            .map_err(|error| format!("Archive contains an invalid path: {error}"))?
            .into_owned();
        if archive_source_path(&archive_path)?.as_deref() != Some(Path::new(SOURCE_MANIFEST_FILE)) {
            continue;
        }
        if manifest.is_some() || !entry.header().entry_type().is_file() {
            return Err(
                "The source archive must contain one regular root skill-manager.json file."
                    .to_string(),
            );
        }
        let mut contents = Vec::new();
        entry
            .by_ref()
            .take(MAX_MANIFEST_BYTES as u64 + 1)
            .read_to_end(&mut contents)
            .map_err(|error| format!("Could not read skill-manager.json: {error}"))?;
        if contents.len() > MAX_MANIFEST_BYTES {
            return Err("skill-manager.json is larger than the 1 MB limit.".to_string());
        }
        manifest = Some(contents);
    }
    manifest.ok_or_else(|| {
        "This source archive does not publish the required root skill-manager.json.".to_string()
    })
}

fn archive_source_path(path: &Path) -> Result<Option<PathBuf>, String> {
    let mut components = path.components();
    let Some(Component::Normal(root)) = components.next() else {
        return Err(format!(
            "Archive contains an unsafe path: {}",
            path.display()
        ));
    };
    validate_portable_path_component(root, path)?;
    let mut relative = PathBuf::new();
    for component in components {
        let Component::Normal(part) = component else {
            return Err(format!(
                "Archive contains an unsafe path: {}",
                path.display()
            ));
        };
        validate_portable_path_component(part, path)?;
        relative.push(part);
    }
    Ok((!relative.as_os_str().is_empty()).then_some(relative))
}

fn archive_reference_root(reference: &str) -> PathBuf {
    let wildcard = reference.find(['*', '?', '[']);
    match wildcard {
        None => PathBuf::from(reference),
        Some(index) => {
            let literal = &reference[..index];
            literal
                .rfind('/')
                .map_or_else(PathBuf::new, |slash| PathBuf::from(&literal[..slash]))
        }
    }
}

fn archive_reference_matches(path: &Path, root: &Path) -> bool {
    root.as_os_str().is_empty() || path == root || path.starts_with(root)
}

pub(crate) fn temporary_path(parent: &Path, label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    parent.join(format!(".{label}-{}-{nonce}", std::process::id()))
}

pub(crate) fn activate_catalog(staging: &Path, cache_base: &Path) -> Result<(), String> {
    let current = catalog_dir(cache_base);
    if !current.exists() {
        return fs_retry::rename(staging, &current)
            .map_err(|error| format!("Could not activate the skillbook catalog: {error}"));
    }

    let backup = temporary_path(cache_base, "catalog-previous");
    fs_retry::rename(&current, &backup).map_err(|error| {
        format!("Could not prepare the existing catalog for replacement: {error}")
    })?;

    if let Err(error) = fs_retry::rename(staging, &current) {
        let restore = fs_retry::rename(&backup, &current);
        return match restore {
            Ok(()) => Err(format!("Could not activate the new skillbook catalog: {error}")),
            Err(restore_error) => Err(format!(
                "Could not activate the new skillbook catalog ({error}) or restore the previous catalog ({restore_error})."
            )),
        };
    }

    fs_retry::remove_dir_all(&backup).map_err(|error| {
        format!("The catalog updated, but its previous cache could not be removed: {error}")
    })
}

pub(crate) fn stage_catalog(
    cache_base: &Path,
    bytes: &[u8],
    metadata: &CatalogMetadata,
) -> Result<(PathBuf, CatalogContents), String> {
    fs::create_dir_all(cache_base)
        .map_err(|error| format!("Could not create {}: {error}", cache_base.display()))?;
    let staging = temporary_path(cache_base, "catalog-downloading");
    let result = extract_catalog_archive(bytes, &staging).and_then(|catalog| {
        write_catalog_metadata(&staging, metadata)?;
        Ok((staging.clone(), catalog))
    });

    if result.is_err() && staging.exists() {
        let _ = fs_retry::remove_dir_all(&staging);
    }

    result
}

pub(crate) fn source_definitions(config_base: &Path) -> Vec<SourceDefinition> {
    match read_sources_config(config_base) {
        Ok(sources) => sources,
        Err(error) => {
            eprintln!("Ignoring invalid source configuration: {error}");
            Vec::new()
        }
    }
}

pub(crate) fn source_state(
    source: &SourceDefinition,
    status: SourceStatus,
    refresh_failed: bool,
    message: Option<String>,
    commit: Option<String>,
    checked_at_epoch_seconds: u64,
    catalog_errors: Vec<CatalogError>,
) -> SourceState {
    SourceState {
        id: source.id.clone(),
        name: source.name.clone(),
        url: source.url.clone(),
        built_in: source.is_built_in(),
        status,
        refresh_failed,
        message,
        commit,
        checked_at_epoch_seconds,
        catalog_errors,
    }
}

pub(crate) fn source_catalog_from_disk(
    source: SourceDefinition,
    cache_base: &Path,
    requested_status: SourceStatus,
    message: Option<String>,
    commit: Option<String>,
    checked_at_epoch_seconds: u64,
) -> SourceCatalog {
    let refresh_failed = message.is_some();
    let source_cache = source_cache_base(cache_base, &source.id);
    let catalog = catalog_dir(&source_cache);
    if !catalog.is_dir() {
        return SourceCatalog {
            state: source_state(
                &source,
                SourceStatus::Error,
                refresh_failed,
                message.or_else(|| Some("No validated catalog is available yet.".to_string())),
                commit,
                checked_at_epoch_seconds,
                Vec::new(),
            ),
            definition: source,
            path: None,
            skills: BTreeMap::new(),
        };
    }

    let Some(metadata) = read_catalog_metadata(&catalog, &source) else {
        return SourceCatalog {
            state: source_state(
                &source,
                SourceStatus::Error,
                refresh_failed,
                Some(match message {
                    Some(message) => {
                        format!("{message} Cached catalog metadata is invalid or missing.")
                    }
                    None => "Cached catalog metadata is invalid or missing.".to_string(),
                }),
                commit,
                checked_at_epoch_seconds,
                Vec::new(),
            ),
            definition: source,
            path: None,
            skills: BTreeMap::new(),
        };
    };

    match catalog_contents(&catalog) {
        Ok(contents) => {
            let stored_commit = commit.or(Some(metadata.commit_sha));
            SourceCatalog {
                state: source_state(
                    &source,
                    requested_status,
                    refresh_failed,
                    message,
                    stored_commit,
                    checked_at_epoch_seconds,
                    contents.errors.clone(),
                ),
                definition: source,
                path: Some(catalog),
                skills: contents.skills,
            }
        }
        Err(error) => SourceCatalog {
            state: source_state(
                &source,
                SourceStatus::Error,
                refresh_failed,
                Some(match message {
                    Some(message) => format!("{message} Cached catalog is invalid: {error}"),
                    None => format!("Cached catalog is invalid: {error}"),
                }),
                commit,
                checked_at_epoch_seconds,
                Vec::new(),
            ),
            definition: source,
            path: None,
            skills: BTreeMap::new(),
        },
    }
}

/// Builds the source view from contents that were already read and validated
/// while the catalog was prepared, so a refresh hashes each catalog once.
pub(crate) fn source_catalog_from_contents(
    source: SourceDefinition,
    cache_base: &Path,
    contents: CatalogContents,
    commit_sha: String,
    checked_at_epoch_seconds: u64,
) -> SourceCatalog {
    let catalog = catalog_dir(&source_cache_base(cache_base, &source.id));
    SourceCatalog {
        state: source_state(
            &source,
            SourceStatus::Fresh,
            false,
            None,
            Some(commit_sha),
            checked_at_epoch_seconds,
            contents.errors,
        ),
        definition: source,
        path: Some(catalog),
        skills: contents.skills,
    }
}

pub(crate) fn finalize_prepared_source(
    source: SourceDefinition,
    cache_base: &Path,
    prepared: Result<PreparedCatalog, String>,
    checked_at_epoch_seconds: u64,
) -> SourceCatalog {
    let source_cache = source_cache_base(cache_base, &source.id);
    match prepared {
        Ok(PreparedCatalog::Current {
            commit_sha,
            contents,
        }) => source_catalog_from_contents(
            source,
            cache_base,
            contents,
            commit_sha,
            checked_at_epoch_seconds,
        ),
        Ok(PreparedCatalog::Staged {
            commit_sha,
            path,
            contents,
        }) => {
            let activation = activate_catalog(&path, &source_cache);
            if path.exists() {
                let _ = fs_retry::remove_dir_all(&path);
            }
            match activation {
                Ok(()) => source_catalog_from_contents(
                    source,
                    cache_base,
                    contents,
                    commit_sha,
                    checked_at_epoch_seconds,
                ),
                Err(error) => source_catalog_from_disk(
                    source,
                    cache_base,
                    SourceStatus::Cached,
                    Some(format!(
                        "Could not activate the refreshed catalog. Using the last validated copy. {error}"
                    )),
                    None,
                    checked_at_epoch_seconds,
                ),
            }
        }
        Err(error) => source_catalog_from_disk(
            source,
            cache_base,
            SourceStatus::Cached,
            Some(format!(
                "Could not refresh this source. Using the last validated copy when available. {error}"
            )),
            None,
            checked_at_epoch_seconds,
        ),
    }
}

pub(crate) fn configured_source(
    config_base: &Path,
    source_id: &str,
) -> Result<SourceDefinition, String> {
    source_definitions(config_base)
        .into_iter()
        .find(|source| source.id == source_id)
        .ok_or_else(|| format!("Unknown skill source: {source_id}"))
}

#[cfg(test)]
pub(crate) fn refresh_catalog(
    cache_base: &Path,
    bytes: &[u8],
    metadata: &CatalogMetadata,
) -> Result<(), String> {
    let (staging, _) = stage_catalog(cache_base, bytes, metadata)?;
    let result = activate_catalog(&staging, cache_base);

    if staging.exists() {
        let _ = fs_retry::remove_dir_all(&staging);
    }

    result
}

pub(crate) fn github_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .https_only(true)
        .timeout(Duration::from_secs(30))
        .user_agent(format!("skill-manager/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("Could not configure the GitHub client: {error}"))
}

async fn check_current_commit(
    client: &reqwest::Client,
    current_metadata: Option<&CatalogMetadata>,
) -> Result<CommitCheck, String> {
    let mut request = client
        .get(CATALOG_REF_URL)
        .header(ACCEPT, "application/vnd.github+json");
    if let Some(etag) = current_metadata.and_then(|metadata| metadata.etag.as_ref()) {
        request = request.header(IF_NONE_MATCH, etag);
    }

    let response = request
        .send()
        .await
        .map_err(|error| format!("Could not check the skillbook commit: {error}"))?;
    if response.status() == StatusCode::NOT_MODIFIED {
        return Ok(CommitCheck::NotModified);
    }
    let response = response
        .error_for_status()
        .map_err(|error| format!("GitHub rejected the skillbook commit check: {error}"))?;

    if response
        .content_length()
        .is_some_and(|length| length > MAX_REF_RESPONSE_BYTES)
    {
        return Err("The GitHub reference response is unexpectedly large.".to_string());
    }

    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("Could not read the skillbook reference response: {error}"))?;
    if bytes.len() as u64 > MAX_REF_RESPONSE_BYTES {
        return Err("The GitHub reference response is unexpectedly large.".to_string());
    }
    let reference = serde_json::from_slice::<GitHubReference>(&bytes)
        .map_err(|error| format!("GitHub returned invalid skillbook commit metadata: {error}"))?;
    if !valid_commit_sha(&reference.object.sha) {
        return Err("GitHub returned an invalid skillbook commit SHA.".to_string());
    }

    Ok(CommitCheck::Current(CatalogMetadata {
        version: CATALOG_METADATA_VERSION,
        source_id: Some(BUILT_IN_SOURCE_ID.to_string()),
        source: CATALOG_SOURCE.to_string(),
        commit_sha: reference.object.sha,
        etag,
    }))
}

pub(crate) async fn download_catalog(
    client: &reqwest::Client,
    commit_sha: &str,
) -> Result<Vec<u8>, String> {
    let response = client
        .get(catalog_archive_url(commit_sha)?)
        .send()
        .await
        .map_err(|error| format!("Could not download skillbook: {error}"))?
        .error_for_status()
        .map_err(|error| format!("GitHub rejected the skillbook download: {error}"))?;

    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARCHIVE_BYTES)
    {
        return Err(format!(
            "The skillbook archive exceeds {} MB.",
            MAX_ARCHIVE_BYTES / 1024 / 1024
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("Could not read the skillbook download: {error}"))?;
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        return Err(format!(
            "The skillbook archive exceeds {} MB.",
            MAX_ARCHIVE_BYTES / 1024 / 1024
        ));
    }

    Ok(bytes.to_vec())
}

pub(crate) async fn prepare_catalog_from_github(
    client: &reqwest::Client,
    current_metadata: Option<CatalogMetadata>,
    cache_base: PathBuf,
) -> Result<PreparedCatalog, String> {
    let current_catalog = catalog_dir(&cache_base);
    let cached_contents = if current_metadata.is_some() {
        run_blocking("Cached catalog validation", move || {
            Ok(catalog_contents(&current_catalog).ok())
        })
        .await?
    } else {
        None
    };
    let check_metadata = cached_contents
        .is_some()
        .then_some(current_metadata.as_ref())
        .flatten();

    match check_current_commit(client, check_metadata).await? {
        CommitCheck::NotModified => cached_contents
            .zip(current_metadata)
            .map(|(contents, metadata)| PreparedCatalog::Current {
                commit_sha: metadata.commit_sha,
                contents,
            })
            .ok_or_else(|| "GitHub reported an unchanged catalog without a cached commit.".into()),
        CommitCheck::Current(metadata) => {
            let unchanged = current_metadata
                .as_ref()
                .is_some_and(|current| current.commit_sha == metadata.commit_sha);
            match cached_contents.filter(|_| unchanged) {
                Some(contents) => Ok(PreparedCatalog::Current {
                    commit_sha: metadata.commit_sha,
                    contents,
                }),
                None => {
                    let bytes = download_catalog(client, &metadata.commit_sha).await?;
                    let commit_sha = metadata.commit_sha.clone();
                    let (path, contents) = run_blocking("Catalog extraction", move || {
                        stage_catalog(&cache_base, &bytes, &metadata)
                    })
                    .await?;
                    Ok(PreparedCatalog::Staged {
                        commit_sha,
                        path,
                        contents,
                    })
                }
            }
        }
    }
}

pub(crate) fn stage_catalog_from_git(
    source: &SourceDefinition,
    source_cache: &Path,
    commit_sha: &str,
) -> Result<(PathBuf, CatalogContents), String> {
    fs::create_dir_all(source_cache)
        .map_err(|error| format!("Could not create {}: {error}", source_cache.display()))?;
    let checkout = temporary_path(source_cache, "git-checkout");
    let staging = temporary_path(source_cache, "catalog-downloading");
    let result = (|| {
        let cloned_commit = clone_default_branch(&source.url, &checkout)?;
        if !valid_commit_sha(&cloned_commit) {
            return Err(format!("{} returned an invalid Git commit.", source.name));
        }
        fs::create_dir(&staging)
            .map_err(|error| format!("Could not create {}: {error}", staging.display()))?;
        let source_directory = checkout.join("skills");
        if source_directory.is_dir() {
            copy_validated_catalog_directory(&source_directory, &staging.join("skills"))?;
        }
        validate_catalog_tree(&staging)?;
        let catalog = catalog_contents(&staging).map_err(|error| {
            format!(
                "This Git repository is not properly formatted as a Skill Manager source: {error}"
            )
        })?;
        write_catalog_metadata(
            &staging,
            &CatalogMetadata {
                version: CATALOG_METADATA_VERSION,
                source_id: Some(source.id.clone()),
                source: source.url.clone(),
                commit_sha: if cloned_commit == commit_sha {
                    commit_sha.to_string()
                } else {
                    cloned_commit
                },
                etag: None,
            },
        )?;
        Ok((staging.clone(), catalog))
    })();

    if checkout.exists() {
        let _ = fs_retry::remove_dir_all(&checkout);
    }
    if result.is_err() && staging.exists() {
        let _ = fs_retry::remove_dir_all(&staging);
    }
    result
}

pub(crate) fn prepare_catalog_from_git(
    source: &SourceDefinition,
    current_metadata: Option<CatalogMetadata>,
    source_cache: &Path,
) -> Result<PreparedCatalog, String> {
    let commit_sha = remote_head(&source.url)?;
    if !valid_commit_sha(&commit_sha) {
        return Err(format!("{} returned an invalid Git commit.", source.name));
    }
    if current_metadata
        .as_ref()
        .is_some_and(|current| current.commit_sha == commit_sha)
    {
        if let Ok(contents) = catalog_contents(&catalog_dir(source_cache)) {
            return Ok(PreparedCatalog::Current {
                commit_sha,
                contents,
            });
        }
    }

    let (path, contents) = stage_catalog_from_git(source, source_cache, &commit_sha)?;
    let actual_commit = read_catalog_metadata(&path, source)
        .map(|metadata| metadata.commit_sha)
        .unwrap_or(commit_sha);
    Ok(PreparedCatalog::Staged {
        commit_sha: actual_commit,
        path,
        contents,
    })
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
            validate_portable_path_component(&entry.file_name(), &path)?;
            let relative = relative_path(root, &path)?;
            let portable_key = relative.replace('\\', "/").to_lowercase();
            if !seen_paths.insert(portable_key) {
                return Err(format!(
                    "Catalog paths collide on a case-insensitive filesystem: {relative}"
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
                    .ok_or_else(|| "The catalog contains too many files.".to_string())?;
                if *file_count > MAX_CATALOG_FILES {
                    return Err(format!(
                        "The catalog contains more than {MAX_CATALOG_FILES} skill files."
                    ));
                }
                let length = entry
                    .metadata()
                    .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?
                    .len();
                *total_bytes = total_bytes
                    .checked_add(length)
                    .ok_or_else(|| "The catalog is too large.".to_string())?;
                if *total_bytes > MAX_EXTRACTED_BYTES {
                    return Err(format!(
                        "The catalog expands beyond {} MB.",
                        MAX_EXTRACTED_BYTES / 1024 / 1024
                    ));
                }
            } else {
                return Err(format!(
                    "Catalog entry is not a regular file or directory: {}",
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

pub(crate) fn plan_directory_copy(
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

/// Installing a skill writes every file it contains, and on Windows every one
/// of those writes is scanned before it lands. The directories are created
/// first, in order, so that the files can then be copied together.
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
    .map(|_copied| ())
}

pub(crate) fn copy_validated_catalog_directory(source: &Path, target: &Path) -> Result<(), String> {
    fn copy_entries(
        source_root: &Path,
        source: &Path,
        target: &Path,
        seen_paths: &mut BTreeSet<String>,
        file_count: &mut usize,
        total_bytes: &mut u64,
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
            validate_portable_path_component(&entry.file_name(), &source_path)?;
            let relative = relative_path(source_root, &source_path)?;
            let portable_key = relative.replace('\\', "/").to_lowercase();
            if !seen_paths.insert(portable_key) {
                return Err(format!(
                    "Catalog paths collide on a case-insensitive filesystem: {relative}"
                ));
            }

            let file_type = entry
                .file_type()
                .map_err(|error| format!("Could not inspect {}: {error}", source_path.display()))?;
            if file_type.is_dir() {
                copy_entries(
                    source_root,
                    &source_path,
                    &destination,
                    seen_paths,
                    file_count,
                    total_bytes,
                )?;
            } else if file_type.is_file() {
                *file_count = file_count
                    .checked_add(1)
                    .ok_or_else(|| "The catalog contains too many files.".to_string())?;
                if *file_count > MAX_CATALOG_FILES {
                    return Err(format!(
                        "The catalog contains more than {MAX_CATALOG_FILES} skill files."
                    ));
                }
                let length = entry
                    .metadata()
                    .map_err(|error| {
                        format!("Could not inspect {}: {error}", source_path.display())
                    })?
                    .len();
                *total_bytes = total_bytes
                    .checked_add(length)
                    .ok_or_else(|| "The catalog is too large.".to_string())?;
                if *total_bytes > MAX_EXTRACTED_BYTES {
                    return Err(format!(
                        "The catalog expands beyond {} MB.",
                        MAX_EXTRACTED_BYTES / 1024 / 1024
                    ));
                }

                fs::copy(&source_path, &destination).map_err(|error| {
                    format!(
                        "Could not copy {} to {}: {error}",
                        source_path.display(),
                        destination.display()
                    )
                })?;
            } else {
                return Err(format!(
                    "{} is not a regular file or directory",
                    source_path.display()
                ));
            }
        }
        Ok(())
    }

    let mut seen_paths = BTreeSet::new();
    let mut file_count = 0;
    let mut total_bytes = 0;
    copy_entries(
        source,
        source,
        target,
        &mut seen_paths,
        &mut file_count,
        &mut total_bytes,
    )
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
            https.source_key,
            validate_repository_url("https://github.com/acme/skills")
                .expect("same repository without .git")
                .source_key
        );

        let ssh = source_identity("ssh://git@GitHub.COM:22/acme/private-skills.git")
            .expect("valid SSH repository");
        assert_eq!(
            ssh.canonical_url,
            "ssh://git@github.com/acme/private-skills.git"
        );
        assert_eq!(ssh.display_name, "private-skills");
        assert!(ssh.source_key.starts_with("source-"));
        assert_eq!(ssh.source_key.len(), 23);
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
    fn source_keys_are_stable_and_url_specific() {
        let canonical =
            canonicalize_repository_url("https://github.com/acme/skills").expect("valid URL");
        assert_eq!(stable_source_key(&canonical), stable_source_key(&canonical));
        assert_eq!(
            stable_source_key(&canonical),
            stable_source_key(
                &canonicalize_repository_url("https://github.com/acme/skills.git")
                    .expect("valid alias")
            )
        );
        assert_ne!(
            stable_source_key(&canonical),
            stable_source_key("ssh://git@github.com/acme/skills")
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

    #[test]
    fn manifest_archive_is_discovered_before_referenced_content_is_extracted() {
        use flate2::{write::GzEncoder, Compression};

        let manifest = br#"{
          "version": 1,
          "source": { "id": "skillbook", "name": "Skillbook", "description": "Skills" },
          "agentSkills": [{ "include": ["skills/*"], "destinations": [{ "anchor": "home", "path": ".agents/skills/${skill.name}" }] }],
          "actions": [{ "id": "doctor", "name": "Doctor", "description": "Check", "steps": [{ "id": "doctor", "program": { "source": "scripts/doctor.sh" } }] }]
        }"#;
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut archive = tar::Builder::new(encoder);
        for (path, contents, mode) in [
            (
                "skillbook-commit/skill-manager.json",
                manifest.as_slice(),
                0o644,
            ),
            (
                "skillbook-commit/skills/review/SKILL.md",
                b"---\nname: review\ndescription: Review\n---\n".as_slice(),
                0o644,
            ),
            (
                "skillbook-commit/scripts/doctor.sh",
                b"#!/bin/sh\nexit 0\n".as_slice(),
                0o755,
            ),
            (
                "skillbook-commit/private/secret.txt",
                b"not referenced".as_slice(),
                0o644,
            ),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            archive
                .append_data(&mut header, path, contents)
                .expect("archive entry");
        }
        let encoder = archive.into_inner().expect("archive");
        let bytes = encoder.finish().expect("gzip");
        let target = tempfile::tempdir().expect("target");
        let parsed = extract_manifest_source_archive(&bytes, target.path()).expect("extract");

        assert_eq!(parsed.source.id, "skillbook");
        assert!(target.path().join("skill-manager.json").is_file());
        assert!(target.path().join("skills/review/SKILL.md").is_file());
        assert!(target.path().join("scripts/doctor.sh").is_file());
        assert!(!target.path().join("private/secret.txt").exists());
    }

    fn path_text(path: &Path) -> &str {
        path.to_str().expect("UTF-8 test path")
    }

    fn run_test_git<const N: usize>(working_directory: &Path, arguments: [&str; N]) {
        let output = git_command()
            .current_dir(working_directory)
            .args(arguments)
            .output()
            .expect("run test Git");
        assert!(
            output.status.success(),
            "Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
