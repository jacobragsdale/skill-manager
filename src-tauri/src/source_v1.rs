//! Manifest-aware source configuration and immutable snapshot acquisition.

use crate::catalog_v1::{read_manifest_catalog, ManifestCatalog};
use crate::domain::{BUILT_IN_SOURCE_ID, CATALOG_SOURCE};
use crate::fs_retry;
use crate::sources::{
    clone_manifest_source, repository_url_key, source_identity, sync_directory, temporary_path,
    valid_commit_sha, validate_catalog_tree,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const SOURCES_VERSION: u8 = 3;
const CURRENT_POINTER_VERSION: u8 = 1;
const SOURCES_FILE: &str = "sources.json";
const SOURCES_BACKUP_FILE: &str = "sources.json.previous";
pub(crate) const BUILT_IN_SOURCE_KEY: &str = "source-41d130b3115ae73a";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ConfiguredSource {
    pub(crate) source_key: String,
    pub(crate) source_id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) url: String,
    pub(crate) executable: bool,
}

impl ConfiguredSource {
    pub(crate) fn built_in() -> Self {
        Self {
            source_key: BUILT_IN_SOURCE_KEY.to_string(),
            source_id: BUILT_IN_SOURCE_ID.to_string(),
            name: "Skillbook".to_string(),
            description: "Jacob's canonical library of portable Agent Skills.".to_string(),
            url: CATALOG_SOURCE.to_string(),
            executable: false,
        }
    }

    pub(crate) fn is_built_in(&self) -> bool {
        self.source_key == BUILT_IN_SOURCE_KEY
            && self.source_id == BUILT_IN_SOURCE_ID
            && repository_url_key(&self.url) == repository_url_key(CATALOG_SOURCE)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SourcesFile {
    version: u8,
    sources: Vec<ConfiguredSource>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LegacySource {
    id: String,
    name: String,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LegacySourcesFile {
    version: u8,
    sources: Vec<LegacySource>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CurrentPointer {
    version: u8,
    commit: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SourceCandidate {
    pub(crate) definition: ConfiguredSource,
    pub(crate) commit: String,
    pub(crate) path: PathBuf,
    pub(crate) catalog: ManifestCatalog,
    pub(crate) staged: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct SourceSnapshot {
    pub(crate) definition: ConfiguredSource,
    pub(crate) commit: String,
    pub(crate) path: PathBuf,
    pub(crate) catalog: ManifestCatalog,
}

pub(crate) fn sources_path(config_base: &Path) -> PathBuf {
    config_base.join(SOURCES_FILE)
}

pub(crate) fn read_sources(
    config_base: &Path,
    cache_base: &Path,
) -> Result<Vec<ConfiguredSource>, String> {
    recover_sources_file(config_base)?;
    let path = sources_path(config_base);
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let sources = vec![ConfiguredSource::built_in()];
            write_sources(config_base, &sources)?;
            return Ok(sources);
        }
        Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
    };
    let version = serde_json::from_slice::<serde_json::Value>(&contents)
        .ok()
        .and_then(|value| value.get("version").and_then(serde_json::Value::as_u64))
        .ok_or_else(|| {
            format!(
                "{} does not contain a source configuration version.",
                path.display()
            )
        })?;
    let sources = if version == u64::from(SOURCES_VERSION) {
        let file = serde_json::from_slice::<SourcesFile>(&contents)
            .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
        file.sources
    } else if matches!(version, 1 | 2) {
        let legacy = serde_json::from_slice::<LegacySourcesFile>(&contents)
            .map_err(|error| format!("Could not parse legacy {}: {error}", path.display()))?;
        migrate_legacy_sources(legacy, cache_base)?
    } else {
        return Err(format!(
            "{} uses unsupported source configuration version {version}.",
            path.display()
        ));
    };
    validate_sources(&sources)?;
    Ok(sources)
}

pub(crate) fn write_sources(
    config_base: &Path,
    sources: &[ConfiguredSource],
) -> Result<(), String> {
    validate_sources(sources)?;
    fs::create_dir_all(config_base)
        .map_err(|error| format!("Could not create {}: {error}", config_base.display()))?;
    recover_sources_file(config_base)?;
    let file = SourcesFile {
        version: SOURCES_VERSION,
        sources: sources.to_vec(),
    };
    let mut contents = serde_json::to_vec_pretty(&file)
        .map_err(|error| format!("Could not serialize source configuration: {error}"))?;
    contents.push(b'\n');
    atomic_write_with_backup(
        config_base,
        &sources_path(config_base),
        &config_base.join(SOURCES_BACKUP_FILE),
        "sources-writing",
        &contents,
    )
}

pub(crate) fn configured_source(
    config_base: &Path,
    cache_base: &Path,
    source_id: &str,
) -> Result<ConfiguredSource, String> {
    read_sources(config_base, cache_base)?
        .into_iter()
        .find(|source| source.source_id == source_id)
        .ok_or_else(|| format!("Unknown source: {source_id}"))
}

pub(crate) fn source_cache_root(cache_base: &Path, source_key: &str) -> PathBuf {
    cache_base.join("sources").join(source_key)
}

pub(crate) fn revision_path(cache_base: &Path, source_key: &str, commit: &str) -> PathBuf {
    source_cache_root(cache_base, source_key)
        .join("revisions")
        .join(commit)
}

pub(crate) fn load_current(
    cache_base: &Path,
    definition: &ConfiguredSource,
) -> Result<Option<SourceSnapshot>, String> {
    let source_root = source_cache_root(cache_base, &definition.source_key);
    let Some(commit) = read_current_pointer(&source_root)? else {
        return Ok(None);
    };
    let path = revision_path(cache_base, &definition.source_key, &commit);
    let catalog = read_manifest_catalog(&path, &definition.source_key)?;
    if !pending_source_id(&definition.source_id)
        && catalog.manifest.source.id != definition.source_id
    {
        return Err(format!(
            "Source {} changed its manifest id from {} to {}. The last validated revision remains active.",
            definition.url, definition.source_id, catalog.manifest.source.id
        ));
    }
    let normalized = configured_from_catalog(
        definition.source_key.clone(),
        definition.url.clone(),
        &catalog,
    );
    Ok(Some(SourceSnapshot {
        definition: normalized,
        commit,
        path,
        catalog,
    }))
}

pub(crate) fn prepare_new_source(url: &str, cache_base: &Path) -> Result<SourceCandidate, String> {
    let identity = source_identity(url)?;
    prepare_candidate(&identity.source_key, &identity.canonical_url, cache_base)
}

pub(crate) fn prepare_refresh(
    source: &ConfiguredSource,
    cache_base: &Path,
) -> Result<SourceCandidate, String> {
    prepare_candidate(&source.source_key, &source.url, cache_base)
}

fn prepare_candidate(
    source_key: &str,
    canonical_url: &str,
    cache_base: &Path,
) -> Result<SourceCandidate, String> {
    let source_root = source_cache_root(cache_base, source_key);
    fs::create_dir_all(&source_root)
        .map_err(|error| format!("Could not create {}: {error}", source_root.display()))?;
    let remote = crate::sources::query_remote_head(canonical_url)?;
    if let Some(commit) = read_current_pointer(&source_root)? {
        if commit == remote.commit {
            let path = revision_path(cache_base, source_key, &commit);
            if let Ok(catalog) = read_manifest_catalog(&path, source_key) {
                let definition = configured_from_catalog(
                    source_key.to_string(),
                    canonical_url.to_string(),
                    &catalog,
                );
                return Ok(SourceCandidate {
                    definition,
                    commit,
                    path,
                    catalog,
                    staged: false,
                });
            }
        }
    }

    let staging = temporary_path(&source_root, "source-preparing");
    let result = (|| {
        let commit = if source_key == BUILT_IN_SOURCE_KEY {
            let bytes = crate::sources::download_manifest_source_archive(&remote.commit)?;
            crate::sources::extract_manifest_source_archive(&bytes, &staging)?;
            remote.commit.clone()
        } else {
            clone_manifest_source(canonical_url, &staging)?
        };
        if !valid_commit_sha(&commit) {
            return Err("Git returned an invalid source commit.".to_string());
        }
        let git_directory = staging.join(".git");
        if git_directory.exists() {
            fs_retry::remove_dir_all(&git_directory)
                .map_err(|error| format!("Could not remove source Git metadata: {error}"))?;
        }
        validate_catalog_tree(&staging)?;
        let catalog = read_manifest_catalog(&staging, source_key).map_err(|error| {
            format!("This Git repository is not a valid Skill Manager source: {error}")
        })?;
        let definition =
            configured_from_catalog(source_key.to_string(), canonical_url.to_string(), &catalog);
        Ok(SourceCandidate {
            definition,
            commit,
            path: staging.clone(),
            catalog,
            staged: true,
        })
    })();
    if result.is_err() && staging.exists() {
        let _ = fs_retry::remove_dir_all(&staging);
    }
    result
}

pub(crate) fn activate_candidate(
    cache_base: &Path,
    candidate: SourceCandidate,
) -> Result<SourceSnapshot, String> {
    let source_root = source_cache_root(cache_base, &candidate.definition.source_key);
    let revision = revision_path(
        cache_base,
        &candidate.definition.source_key,
        &candidate.commit,
    );
    fs::create_dir_all(revision.parent().expect("revision parent"))
        .map_err(|error| format!("Could not create {}: {error}", revision.display()))?;
    if candidate.staged {
        if revision.exists() {
            fs_retry::remove_dir_all(&candidate.path)
                .map_err(|error| format!("Could not remove duplicate prepared source: {error}"))?;
        } else {
            fs_retry::rename(&candidate.path, &revision).map_err(|error| {
                format!(
                    "Could not retain source revision {}: {error}",
                    candidate.commit
                )
            })?;
        }
    }
    write_current_pointer(&source_root, &candidate.commit)?;
    let catalog = read_manifest_catalog(&revision, &candidate.definition.source_key)?;
    Ok(SourceSnapshot {
        definition: candidate.definition,
        commit: candidate.commit,
        path: revision,
        catalog,
    })
}

pub(crate) fn discard_candidate(candidate: &SourceCandidate) {
    if candidate.staged && candidate.path.exists() {
        let _ = fs_retry::remove_dir_all(&candidate.path);
    }
}

pub(crate) fn remove_source_cache(cache_base: &Path, source_key: &str) -> Result<(), String> {
    let root = source_cache_root(cache_base, source_key);
    match fs_retry::remove_dir_all(&root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not remove {}: {error}", root.display())),
    }
}

fn configured_from_catalog(
    source_key: String,
    url: String,
    catalog: &ManifestCatalog,
) -> ConfiguredSource {
    ConfiguredSource {
        source_key,
        source_id: catalog.manifest.source.id.clone(),
        name: catalog.manifest.source.name.clone(),
        description: catalog.manifest.source.description.clone(),
        url,
        executable: catalog.manifest.has_executable_behavior(),
    }
}

fn validate_sources(sources: &[ConfiguredSource]) -> Result<(), String> {
    let mut keys = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut urls = BTreeSet::new();
    for source in sources {
        let identity = source_identity(&source.url)?;
        if identity.source_key != source.source_key || identity.canonical_url != source.url {
            return Err(format!(
                "Source {} does not match its URL-derived sourceKey.",
                source.source_id
            ));
        }
        if !valid_manifest_source_id(&source.source_id) && !pending_source_id(&source.source_id) {
            return Err(format!("Invalid configured sourceId: {}", source.source_id));
        }
        if source.name.is_empty() || source.description.is_empty() {
            return Err(format!(
                "Source {} has incomplete manifest metadata.",
                source.source_id
            ));
        }
        if !keys.insert(source.source_key.as_str())
            || !ids.insert(source.source_id.as_str())
            || !urls.insert(repository_url_key(&source.url))
        {
            return Err(
                "Source configuration contains a duplicate URL, sourceKey, or sourceId."
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn migrate_legacy_sources(
    legacy: LegacySourcesFile,
    cache_base: &Path,
) -> Result<Vec<ConfiguredSource>, String> {
    let mut legacy_sources = legacy.sources;
    if legacy.version == 1 {
        legacy_sources.insert(
            0,
            LegacySource {
                id: BUILT_IN_SOURCE_ID.to_string(),
                name: "skillbook".to_string(),
                url: CATALOG_SOURCE.to_string(),
            },
        );
    }
    legacy_sources
        .into_iter()
        .map(|legacy| {
            let identity = source_identity(&legacy.url)?;
            if repository_url_key(&legacy.url) == repository_url_key(CATALOG_SOURCE) {
                return Ok(ConfiguredSource::built_in());
            }
            if legacy.id != identity.source_key {
                return Err("Legacy source identity does not match its URL.".to_string());
            }
            let provisional = ConfiguredSource {
                source_key: identity.source_key.clone(),
                source_id: pending_id(&identity.source_key),
                name: legacy.name,
                description: "Manifest metadata will be loaded on the next source refresh."
                    .to_string(),
                url: identity.canonical_url,
                executable: false,
            };
            load_current(cache_base, &provisional)
                .map(|snapshot| snapshot.map_or(provisional, |snapshot| snapshot.definition))
        })
        .collect()
}

fn pending_id(source_key: &str) -> String {
    format!(
        "pending-{}",
        source_key
            .rsplit('-')
            .next()
            .unwrap_or("source")
            .chars()
            .take(8)
            .collect::<String>()
    )
}

fn pending_source_id(source_id: &str) -> bool {
    source_id.starts_with("pending-")
}

fn valid_manifest_source_id(value: &str) -> bool {
    (2..=16).contains(&value.len())
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && !value.ends_with('-')
        && !value.contains("--")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn read_current_pointer(source_root: &Path) -> Result<Option<String>, String> {
    let path = source_root.join("current.json");
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
    };
    let pointer = serde_json::from_slice::<CurrentPointer>(&contents)
        .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
    if pointer.version != CURRENT_POINTER_VERSION || !valid_commit_sha(&pointer.commit) {
        return Err(format!(
            "{} contains an invalid source revision pointer.",
            path.display()
        ));
    }
    Ok(Some(pointer.commit))
}

fn write_current_pointer(source_root: &Path, commit: &str) -> Result<(), String> {
    if !valid_commit_sha(commit) {
        return Err("Cannot activate an invalid source commit.".to_string());
    }
    let pointer = CurrentPointer {
        version: CURRENT_POINTER_VERSION,
        commit: commit.to_string(),
    };
    let mut contents = serde_json::to_vec_pretty(&pointer)
        .map_err(|error| format!("Could not serialize the source pointer: {error}"))?;
    contents.push(b'\n');
    fs::create_dir_all(source_root)
        .map_err(|error| format!("Could not create {}: {error}", source_root.display()))?;
    let path = source_root.join("current.json");
    let staging = temporary_path(source_root, "current-writing");
    write_new_file(&staging, &contents)?;
    if path.exists() {
        fs_retry::remove_file(&path)
            .map_err(|error| format!("Could not replace {}: {error}", path.display()))?;
    }
    fs_retry::rename(&staging, &path)
        .map_err(|error| format!("Could not activate {}: {error}", path.display()))?;
    sync_directory(source_root)
}

fn recover_sources_file(config_base: &Path) -> Result<(), String> {
    let path = sources_path(config_base);
    if path.exists() {
        return Ok(());
    }
    let backup = config_base.join(SOURCES_BACKUP_FILE);
    match fs_retry::rename(&backup, &path) {
        Ok(()) => sync_directory(config_base),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not recover {}: {error}", path.display())),
    }
}

fn atomic_write_with_backup(
    parent: &Path,
    path: &Path,
    backup: &Path,
    label: &str,
    contents: &[u8],
) -> Result<(), String> {
    let staging = temporary_path(parent, label);
    write_new_file(&staging, contents)?;
    if path.exists() {
        if backup.exists() {
            fs_retry::remove_file(backup)
                .map_err(|error| format!("Could not remove {}: {error}", backup.display()))?;
        }
        fs_retry::rename(path, backup)
            .map_err(|error| format!("Could not stage {}: {error}", path.display()))?;
        if let Err(error) = fs_retry::rename(&staging, path) {
            let restore = fs_retry::rename(backup, path);
            return match restore {
                Ok(()) => Err(format!("Could not activate {}: {error}", path.display())),
                Err(restore_error) => Err(format!(
                    "Could not activate {} ({error}) or restore it ({restore_error}).",
                    path.display()
                )),
            };
        }
        let _ = fs_retry::remove_file(backup);
    } else {
        fs_retry::rename(&staging, path)
            .map_err(|error| format!("Could not activate {}: {error}", path.display()))?;
    }
    sync_directory(parent)
}

fn write_new_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("Could not create {}: {error}", path.display()))?;
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("Could not write {}: {error}", path.display()))
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

    fn repository(source_id: &str) -> tempfile::TempDir {
        let repository = tempfile::tempdir().expect("repository");
        git(repository.path(), &["init", "--quiet", "-b", "main"]);
        git(
            repository.path(),
            &["config", "user.email", "tests@example.invalid"],
        );
        git(repository.path(), &["config", "user.name", "Tests"]);
        let skill = repository.path().join("skills/review");
        fs::create_dir_all(&skill).expect("skill");
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: review\ndescription: Reviews code\n---\nBody\n",
        )
        .expect("skill");
        fs::write(
            repository.path().join("skill-manager.json"),
            format!(
                r#"{{
                  "version": 1,
                  "source": {{ "id": "{source_id}", "name": "Test", "description": "Test source" }},
                  "agentSkills": [{{ "include": ["skills/*"], "destinations": [{{ "anchor": "home", "path": ".agents/skills/${{skill.name}}" }}] }}]
                }}"#
            ),
        )
        .expect("manifest");
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "source"]);
        repository
    }

    #[test]
    fn candidate_activation_uses_immutable_commit_directories() {
        let repository = repository("acme");
        let cache = tempfile::tempdir().expect("cache");
        let source_key = "source-test";
        let source_root = source_cache_root(cache.path(), source_key);
        fs::create_dir_all(&source_root).expect("source root");
        let commit = git_output(repository.path(), &["rev-parse", "HEAD"]);
        let copied = temporary_path(&source_root, "source-preparing");
        crate::sources::copy_validated_catalog_directory(repository.path(), &copied)
            .expect("copy source");
        fs_retry::remove_dir_all(&copied.join(".git")).expect("remove Git metadata");
        let catalog = read_manifest_catalog(&copied, source_key).expect("catalog");
        let definition = configured_from_catalog(
            source_key.to_string(),
            "https://example.com/test".to_string(),
            &catalog,
        );
        let candidate = SourceCandidate {
            definition,
            commit: commit.clone(),
            path: copied,
            catalog,
            staged: true,
        };
        let snapshot = activate_candidate(cache.path(), candidate).expect("activate");
        assert_eq!(snapshot.commit, commit);
        assert_eq!(
            snapshot.path,
            revision_path(cache.path(), source_key, &commit)
        );
        assert!(snapshot.path.join("skill-manager.json").is_file());
        assert_eq!(
            read_current_pointer(&source_root).expect("pointer"),
            Some(commit)
        );
    }

    #[test]
    fn duplicate_manifest_namespaces_are_rejected_for_different_urls() {
        let sources = [
            configured("https://example.com/one", "acme", "One"),
            configured("https://example.com/two", "acme", "Two"),
        ];
        assert!(validate_sources(&sources)
            .expect_err("duplicate namespace")
            .contains("duplicate"));
    }

    #[test]
    fn source_id_changes_do_not_replace_the_current_revision() {
        let repository = repository("acme");
        let cache = tempfile::tempdir().expect("cache");
        let configured = configured("https://example.com/acme", "different", "Different");
        let commit = git_output(repository.path(), &["rev-parse", "HEAD"]);
        let copied = revision_path(cache.path(), &configured.source_key, &commit);
        fs::create_dir_all(copied.parent().expect("parent")).expect("revision parent");
        crate::sources::copy_validated_catalog_directory(repository.path(), &copied).expect("copy");
        fs_retry::remove_dir_all(&copied.join(".git")).expect("remove Git metadata");
        write_current_pointer(
            &source_cache_root(cache.path(), &configured.source_key),
            &commit,
        )
        .expect("pointer");
        assert!(load_current(cache.path(), &configured)
            .expect_err("changed namespace")
            .contains("changed its manifest id"));
    }

    fn configured(url: &str, source_id: &str, name: &str) -> ConfiguredSource {
        let identity = source_identity(url).expect("identity");
        ConfiguredSource {
            source_key: identity.source_key,
            source_id: source_id.to_string(),
            name: name.to_string(),
            description: format!("{name} source"),
            url: identity.canonical_url,
            executable: false,
        }
    }

    fn git_output(repository: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(repository)
            .args(args)
            .output()
            .expect("git output");
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .expect("UTF-8")
            .trim()
            .to_string()
    }
}
