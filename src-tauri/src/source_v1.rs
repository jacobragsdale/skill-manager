//! Manifest-aware source and source-repository configuration and snapshots.

use crate::artifact::{
    download_artifact, extract_source_archive, head_artifact, require_repository_json,
    validators_match, ArtifactValidators, DownloadedBytes,
};
use crate::catalog_v1::{read_manifest_catalog, ManifestCatalog};
use crate::fs_retry;
use crate::locator::{Locator, LocatorKind};
use crate::repository::{
    report_manifest, RepositoryManifest, RepositoryValidationReport, REPOSITORY_MANIFEST_FILE,
};
use crate::sources::{
    clone_manifest_source, clone_repository_manifest, query_remote_head, repository_url_key,
    sync_directory, temporary_path, valid_commit_sha, validate_catalog_tree,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const SOURCES_VERSION: u8 = 5;
const LEGACY_SOURCES_VERSION: u8 = 4;
const CURRENT_POINTER_VERSION: u8 = 2;
const LEGACY_CURRENT_POINTER_VERSION: u8 = 1;
const SOURCES_FILE: &str = "sources.json";
const SOURCES_BACKUP_FILE: &str = "sources.json.previous";
const CURRENT_POINTER_FILE: &str = "current.json";
const CURRENT_POINTER_BACKUP_FILE: &str = "current.json.previous";
pub(crate) const BUILT_IN_SOURCE_KEY: &str = "source-41d130b3115ae73a";
pub(crate) const BUILT_IN_SOURCE_ID: &str = "skillbook";
pub(crate) const CATALOG_SOURCE: &str = "https://github.com/jacobragsdale/skillbook";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ConfiguredSource {
    pub(crate) source_key: String,
    pub(crate) source_id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) locator: Locator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) repository_key: Option<String>,
}

impl ConfiguredSource {
    pub(crate) fn built_in() -> Self {
        Self {
            source_key: BUILT_IN_SOURCE_KEY.to_string(),
            source_id: BUILT_IN_SOURCE_ID.to_string(),
            name: "Skillbook".to_string(),
            description: "Jacob's canonical library of portable Agent Skills.".to_string(),
            locator: Locator::Git {
                url: CATALOG_SOURCE.to_string(),
            },
            repository_key: None,
        }
    }

    pub(crate) fn url(&self) -> &str {
        self.locator.url()
    }

    pub(crate) fn is_built_in(&self) -> bool {
        self.source_key == BUILT_IN_SOURCE_KEY
            && self.source_id == BUILT_IN_SOURCE_ID
            && matches!(
                &self.locator,
                Locator::Git { url } if repository_url_key(url) == repository_url_key(CATALOG_SOURCE)
            )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ConfiguredRepository {
    pub(crate) repository_key: String,
    pub(crate) repository_id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) locator: Locator,
}

impl ConfiguredRepository {
    pub(crate) fn url(&self) -> &str {
        self.locator.url()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SourcesConfig {
    pub(crate) repositories: Vec<ConfiguredRepository>,
    pub(crate) sources: Vec<ConfiguredSource>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SourcesFile {
    version: u8,
    #[serde(default)]
    repositories: Vec<ConfiguredRepository>,
    sources: Vec<ConfiguredSource>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LegacyConfiguredSource {
    source_key: String,
    source_id: String,
    name: String,
    description: String,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LegacySourcesFile {
    version: u8,
    sources: Vec<LegacyConfiguredSource>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CurrentPointer {
    version: u8,
    revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_modified: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LegacyCurrentPointer {
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
    pub(crate) validators: ArtifactValidators,
}

#[derive(Clone, Debug)]
pub(crate) struct SourceSnapshot {
    pub(crate) definition: ConfiguredSource,
    pub(crate) commit: String,
    pub(crate) path: PathBuf,
    pub(crate) catalog: ManifestCatalog,
}

#[derive(Clone, Debug)]
pub(crate) struct RepositoryCandidate {
    pub(crate) definition: ConfiguredRepository,
    pub(crate) revision: String,
    pub(crate) path: PathBuf,
    pub(crate) manifest: RepositoryManifest,
    pub(crate) staged: bool,
    pub(crate) validators: ArtifactValidators,
}

#[derive(Clone, Debug)]
pub(crate) struct RepositorySnapshot {
    pub(crate) definition: ConfiguredRepository,
    pub(crate) revision: String,
    pub(crate) path: PathBuf,
    pub(crate) manifest: RepositoryManifest,
}

pub(crate) fn sources_path(config_base: &Path) -> PathBuf {
    config_base.join(SOURCES_FILE)
}

pub(crate) fn read_sources_config(config_base: &Path) -> Result<SourcesConfig, String> {
    recover_sources_file(config_base)?;
    let path = sources_path(config_base);
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let config = SourcesConfig {
                repositories: Vec::new(),
                sources: vec![ConfiguredSource::built_in()],
            };
            write_sources_config(config_base, &config)?;
            return Ok(config);
        }
        Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
    };
    let config = parse_sources_file(&path, &contents)?;
    validate_sources_config(&config)?;
    Ok(config)
}

pub(crate) fn read_sources(config_base: &Path) -> Result<Vec<ConfiguredSource>, String> {
    Ok(read_sources_config(config_base)?.sources)
}

pub(crate) fn write_sources_config(
    config_base: &Path,
    config: &SourcesConfig,
) -> Result<(), String> {
    validate_sources_config(config)?;
    fs::create_dir_all(config_base)
        .map_err(|error| format!("Could not create {}: {error}", config_base.display()))?;
    recover_sources_file(config_base)?;
    let file = SourcesFile {
        version: SOURCES_VERSION,
        repositories: config.repositories.clone(),
        sources: config.sources.clone(),
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

#[allow(dead_code)]
pub(crate) fn write_sources(
    config_base: &Path,
    sources: &[ConfiguredSource],
) -> Result<(), String> {
    let mut config = read_sources_config(config_base).unwrap_or_default();
    config.sources = sources.to_vec();
    write_sources_config(config_base, &config)
}

pub(crate) fn configured_source(
    config_base: &Path,
    source_id: &str,
) -> Result<ConfiguredSource, String> {
    read_sources(config_base)?
        .into_iter()
        .find(|source| source.source_id == source_id)
        .ok_or_else(|| format!("Unknown source: {source_id}"))
}

pub(crate) fn source_cache_root(cache_base: &Path, source_key: &str) -> PathBuf {
    cache_base.join("sources").join(source_key)
}

pub(crate) fn repository_cache_root(cache_base: &Path, repository_key: &str) -> PathBuf {
    cache_base.join("repositories").join(repository_key)
}

pub(crate) fn revision_path(cache_base: &Path, source_key: &str, commit: &str) -> PathBuf {
    source_cache_root(cache_base, source_key)
        .join("revisions")
        .join(commit)
}

pub(crate) fn repository_revision_path(
    cache_base: &Path,
    repository_key: &str,
    revision: &str,
) -> PathBuf {
    repository_cache_root(cache_base, repository_key)
        .join("revisions")
        .join(revision)
}

pub(crate) fn load_current(
    cache_base: &Path,
    definition: &ConfiguredSource,
) -> Result<Option<SourceSnapshot>, String> {
    let source_root = source_cache_root(cache_base, &definition.source_key);
    let Some(pointer) = read_current_pointer(&source_root)? else {
        return Ok(None);
    };
    let path = revision_path(cache_base, &definition.source_key, &pointer.revision);
    let catalog = read_manifest_catalog(&path, &definition.source_key)?;
    if catalog.manifest.source().id != definition.source_id {
        return Err(format!(
            "Source {} changed its manifest id from {} to {}. The last validated revision remains active.",
            definition.url(),
            definition.source_id,
            catalog.manifest.source().id
        ));
    }
    let normalized = configured_from_catalog(
        definition.source_key.clone(),
        definition.locator.clone(),
        definition.repository_key.clone(),
        &catalog,
    );
    Ok(Some(SourceSnapshot {
        definition: normalized,
        commit: pointer.revision,
        path,
        catalog,
    }))
}

pub(crate) fn load_current_repository(
    cache_base: &Path,
    definition: &ConfiguredRepository,
) -> Result<Option<RepositorySnapshot>, String> {
    let root = repository_cache_root(cache_base, &definition.repository_key);
    let Some(pointer) = read_current_pointer(&root)? else {
        return Ok(None);
    };
    let path = repository_revision_path(cache_base, &definition.repository_key, &pointer.revision);
    let manifest = RepositoryManifest::from_path(&path)?;
    if manifest.repository.id != definition.repository_id {
        return Err(format!(
            "Source repository {} changed its id from {} to {}. The last validated revision remains active.",
            definition.url(),
            definition.repository_id,
            manifest.repository.id
        ));
    }
    Ok(Some(repository_snapshot(
        configured_from_repository_manifest(
            definition.repository_key.clone(),
            definition.locator.clone(),
            &manifest,
        ),
        pointer.revision,
        path,
        manifest,
    )?))
}

pub(crate) fn prepare_new_source(
    locator: &Locator,
    cache_base: &Path,
    repository_key: Option<String>,
    expected_source_id: Option<&str>,
) -> Result<SourceCandidate, String> {
    let mut candidate = prepare_candidate(locator, &locator.source_key(), cache_base)?;
    candidate.definition.repository_key = repository_key;
    if let Some(expected) = expected_source_id {
        if candidate.definition.source_id != expected {
            discard_candidate(&candidate);
            return Err(format!(
                "The catalog listed source.id {expected}, but the fetched source publishes {}.",
                candidate.definition.source_id
            ));
        }
    }
    Ok(candidate)
}

pub(crate) fn prepare_refresh(
    source: &ConfiguredSource,
    cache_base: &Path,
) -> Result<SourceCandidate, String> {
    let mut candidate = prepare_candidate(&source.locator, &source.source_key, cache_base)?;
    candidate.definition.repository_key = source.repository_key.clone();
    Ok(candidate)
}

pub(crate) fn prepare_new_repository(
    locator: &Locator,
    cache_base: &Path,
) -> Result<RepositoryCandidate, String> {
    prepare_repository_candidate(locator, &locator.repository_key(), cache_base)
}

pub(crate) fn prepare_repository_refresh(
    repository: &ConfiguredRepository,
    cache_base: &Path,
) -> Result<RepositoryCandidate, String> {
    prepare_repository_candidate(&repository.locator, &repository.repository_key, cache_base)
}

fn prepare_candidate(
    locator: &Locator,
    source_key: &str,
    cache_base: &Path,
) -> Result<SourceCandidate, String> {
    let source_root = source_cache_root(cache_base, source_key);
    fs::create_dir_all(&source_root)
        .map_err(|error| format!("Could not create {}: {error}", source_root.display()))?;
    match locator {
        Locator::Git { url } => {
            prepare_git_source(source_key, locator, url, cache_base, &source_root)
        }
        Locator::Artifact { url } => {
            prepare_artifact_source(source_key, locator, url, cache_base, &source_root)
        }
    }
}

fn prepare_git_source(
    source_key: &str,
    locator: &Locator,
    url: &str,
    cache_base: &Path,
    source_root: &Path,
) -> Result<SourceCandidate, String> {
    let remote = query_remote_head(url)?;
    if let Some(current) =
        reuse_source_revision(cache_base, source_key, locator, source_root, &remote.commit)?
    {
        return Ok(current);
    }
    stage_git_source(source_key, locator, url, source_root)
}

fn prepare_artifact_source(
    source_key: &str,
    locator: &Locator,
    url: &str,
    cache_base: &Path,
    source_root: &Path,
) -> Result<SourceCandidate, String> {
    let stored = read_current_pointer(source_root)?;
    if let Some(pointer) = &stored {
        if let Ok(remote) = head_artifact(url) {
            if validators_match(&pointer.validators(), &remote) {
                if let Some(current) = reuse_source_revision(
                    cache_base,
                    source_key,
                    locator,
                    source_root,
                    &pointer.revision,
                )? {
                    return Ok(current);
                }
            }
        }
    }
    let downloaded = download_artifact(url)?;
    if stored
        .as_ref()
        .is_some_and(|pointer| pointer.revision == downloaded.digest)
    {
        if let Some(mut current) = reuse_source_revision(
            cache_base,
            source_key,
            locator,
            source_root,
            &downloaded.digest,
        )? {
            current.validators = downloaded.validators;
            return Ok(current);
        }
    }
    stage_artifact_source(source_key, locator, downloaded, source_root)
}

fn reuse_source_revision(
    cache_base: &Path,
    source_key: &str,
    locator: &Locator,
    source_root: &Path,
    revision: &str,
) -> Result<Option<SourceCandidate>, String> {
    if read_current_pointer(source_root)?.is_none_or(|pointer| pointer.revision != revision) {
        return Ok(None);
    }
    let path = revision_path(cache_base, source_key, revision);
    let Ok(catalog) = read_manifest_catalog(&path, source_key) else {
        return Ok(None);
    };
    Ok(Some(SourceCandidate {
        definition: configured_from_catalog(
            source_key.to_string(),
            locator.clone(),
            None,
            &catalog,
        ),
        commit: revision.to_string(),
        path,
        catalog,
        staged: false,
        validators: ArtifactValidators::default(),
    }))
}

fn stage_git_source(
    source_key: &str,
    locator: &Locator,
    url: &str,
    source_root: &Path,
) -> Result<SourceCandidate, String> {
    let staging = temporary_path(source_root, "source-preparing");
    let result = (|| {
        let commit = clone_manifest_source(url, &staging)?;
        if !valid_commit_sha(&commit) {
            return Err("Git returned an invalid source commit.".to_string());
        }
        strip_git_metadata(&staging)?;
        validate_catalog_tree(&staging)?;
        let catalog = read_manifest_catalog(&staging, source_key).map_err(|error| {
            format!("This Git repository is not a valid Skill Manager source: {error}")
        })?;
        let definition =
            configured_from_catalog(source_key.to_string(), locator.clone(), None, &catalog);
        Ok(SourceCandidate {
            definition,
            commit,
            path: staging.clone(),
            catalog,
            staged: true,
            validators: ArtifactValidators::default(),
        })
    })();
    if result.is_err() && staging.exists() {
        let _ = fs_retry::remove_dir_all(&staging);
    }
    result
}

fn stage_artifact_source(
    source_key: &str,
    locator: &Locator,
    downloaded: DownloadedBytes,
    source_root: &Path,
) -> Result<SourceCandidate, String> {
    let staging = temporary_path(source_root, "source-preparing");
    let result = (|| {
        extract_source_archive(&downloaded.bytes, &staging)?;
        let catalog = read_manifest_catalog(&staging, source_key).map_err(|error| {
            format!("This artifact is not a valid Skill Manager source: {error}")
        })?;
        let definition =
            configured_from_catalog(source_key.to_string(), locator.clone(), None, &catalog);
        Ok(SourceCandidate {
            definition,
            commit: downloaded.digest.clone(),
            path: staging.clone(),
            catalog,
            staged: true,
            validators: downloaded.validators,
        })
    })();
    if result.is_err() && staging.exists() {
        let _ = fs_retry::remove_dir_all(&staging);
    }
    result
}

fn prepare_repository_candidate(
    locator: &Locator,
    repository_key: &str,
    cache_base: &Path,
) -> Result<RepositoryCandidate, String> {
    let root = repository_cache_root(cache_base, repository_key);
    fs::create_dir_all(&root)
        .map_err(|error| format!("Could not create {}: {error}", root.display()))?;
    match locator {
        Locator::Git { url } => {
            if let Some(current) =
                reuse_git_repository(cache_base, locator, repository_key, &root, url)?
            {
                return Ok(current);
            }
            stage_git_repository(locator, url, &root)
        }
        Locator::Artifact { url } => {
            prepare_artifact_repository(locator, url, cache_base, repository_key, &root)
        }
    }
}

fn reuse_git_repository(
    cache_base: &Path,
    locator: &Locator,
    repository_key: &str,
    root: &Path,
    url: &str,
) -> Result<Option<RepositoryCandidate>, String> {
    let remote = query_remote_head(url)?;
    let Some(pointer) = read_current_pointer(root)? else {
        return Ok(None);
    };
    if pointer.revision != remote.commit {
        return Ok(None);
    }
    load_repository_candidate(
        cache_base,
        locator,
        repository_key,
        &pointer.revision,
        false,
    )
}

fn prepare_artifact_repository(
    locator: &Locator,
    url: &str,
    cache_base: &Path,
    repository_key: &str,
    root: &Path,
) -> Result<RepositoryCandidate, String> {
    let stored = read_current_pointer(root)?;
    if let Some(pointer) = &stored {
        if let Ok(remote) = head_artifact(url) {
            if validators_match(&pointer.validators(), &remote) {
                if let Some(current) = load_repository_candidate(
                    cache_base,
                    locator,
                    repository_key,
                    &pointer.revision,
                    false,
                )? {
                    return Ok(current);
                }
            }
        }
    }
    let downloaded = download_artifact(url)?;
    require_repository_json(&downloaded.bytes)?;
    if stored
        .as_ref()
        .is_some_and(|pointer| pointer.revision == downloaded.digest)
    {
        if let Some(mut current) = load_repository_candidate(
            cache_base,
            locator,
            repository_key,
            &downloaded.digest,
            false,
        )? {
            current.validators = downloaded.validators;
            return Ok(current);
        }
    }
    stage_artifact_repository(locator, downloaded, root)
}

fn load_repository_candidate(
    cache_base: &Path,
    locator: &Locator,
    repository_key: &str,
    revision: &str,
    staged: bool,
) -> Result<Option<RepositoryCandidate>, String> {
    let path = repository_revision_path(cache_base, repository_key, revision);
    let Ok(manifest) = RepositoryManifest::from_path(&path) else {
        return Ok(None);
    };
    Ok(Some(RepositoryCandidate {
        definition: configured_from_repository_manifest(
            repository_key.to_string(),
            locator.clone(),
            &manifest,
        ),
        revision: revision.to_string(),
        path,
        manifest,
        staged,
        validators: ArtifactValidators::default(),
    }))
}

fn stage_git_repository(
    locator: &Locator,
    url: &str,
    root: &Path,
) -> Result<RepositoryCandidate, String> {
    let staging = temporary_path(root, "repository-preparing");
    let result = (|| {
        let revision = clone_repository_manifest(url, &staging)?;
        strip_git_metadata(&staging)?;
        let manifest = RepositoryManifest::from_path(&staging)?;
        Ok(RepositoryCandidate {
            definition: configured_from_repository_manifest(
                locator.repository_key(),
                locator.clone(),
                &manifest,
            ),
            revision,
            path: staging.clone(),
            manifest,
            staged: true,
            validators: ArtifactValidators::default(),
        })
    })();
    if result.is_err() && staging.exists() {
        let _ = fs_retry::remove_dir_all(&staging);
    }
    result
}

fn stage_artifact_repository(
    locator: &Locator,
    downloaded: DownloadedBytes,
    root: &Path,
) -> Result<RepositoryCandidate, String> {
    let staging = temporary_path(root, "repository-preparing");
    let result = (|| {
        fs::create_dir_all(&staging)
            .map_err(|error| format!("Could not create {}: {error}", staging.display()))?;
        fs::write(staging.join(REPOSITORY_MANIFEST_FILE), &downloaded.bytes).map_err(|error| {
            format!(
                "Could not write {}: {error}",
                staging.join(REPOSITORY_MANIFEST_FILE).display()
            )
        })?;
        let manifest = RepositoryManifest::from_path(&staging)?;
        Ok(RepositoryCandidate {
            definition: configured_from_repository_manifest(
                locator.repository_key(),
                locator.clone(),
                &manifest,
            ),
            revision: downloaded.digest.clone(),
            path: staging.clone(),
            manifest,
            staged: true,
            validators: downloaded.validators,
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
    retain_revision(
        &revision,
        candidate.staged,
        &candidate.path,
        &candidate.commit,
    )?;
    write_current_pointer(&source_root, &candidate.commit, &candidate.validators)?;
    let catalog = read_manifest_catalog(&revision, &candidate.definition.source_key)?;
    Ok(SourceSnapshot {
        definition: candidate.definition,
        commit: candidate.commit,
        path: revision,
        catalog,
    })
}

pub(crate) fn activate_repository(
    cache_base: &Path,
    candidate: RepositoryCandidate,
) -> Result<RepositorySnapshot, String> {
    let root = repository_cache_root(cache_base, &candidate.definition.repository_key);
    let revision = repository_revision_path(
        cache_base,
        &candidate.definition.repository_key,
        &candidate.revision,
    );
    retain_revision(
        &revision,
        candidate.staged,
        &candidate.path,
        &candidate.revision,
    )?;
    write_current_pointer(&root, &candidate.revision, &candidate.validators)?;
    repository_snapshot(
        candidate.definition,
        candidate.revision,
        revision,
        candidate.manifest,
    )
}

pub(crate) fn discard_candidate(candidate: &SourceCandidate) {
    if candidate.staged && candidate.path.exists() {
        let _ = fs_retry::remove_dir_all(&candidate.path);
    }
}

pub(crate) fn discard_repository(candidate: &RepositoryCandidate) {
    if candidate.staged && candidate.path.exists() {
        let _ = fs_retry::remove_dir_all(&candidate.path);
    }
}

pub(crate) fn remove_source_cache(cache_base: &Path, source_key: &str) -> Result<(), String> {
    remove_cache_root(&source_cache_root(cache_base, source_key))
}

pub(crate) fn remove_repository_cache(
    cache_base: &Path,
    repository_key: &str,
) -> Result<(), String> {
    remove_cache_root(&repository_cache_root(cache_base, repository_key))
}

fn remove_cache_root(root: &Path) -> Result<(), String> {
    match fs_retry::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not remove {}: {error}", root.display())),
    }
}

fn retain_revision(
    revision: &Path,
    staged: bool,
    staged_path: &Path,
    token: &str,
) -> Result<(), String> {
    fs::create_dir_all(revision.parent().expect("revision parent"))
        .map_err(|error| format!("Could not create {}: {error}", revision.display()))?;
    if !staged {
        return Ok(());
    }
    if revision.exists() {
        fs_retry::remove_dir_all(staged_path)
            .map_err(|error| format!("Could not remove duplicate prepared snapshot: {error}"))?;
    } else {
        fs_retry::rename(staged_path, revision)
            .map_err(|error| format!("Could not retain revision {token}: {error}"))?;
    }
    Ok(())
}

fn strip_git_metadata(path: &Path) -> Result<(), String> {
    let git_directory = path.join(".git");
    if git_directory.exists() {
        fs_retry::remove_dir_all(&git_directory)
            .map_err(|error| format!("Could not remove Git metadata: {error}"))?;
    }
    Ok(())
}

fn configured_from_catalog(
    source_key: String,
    locator: Locator,
    repository_key: Option<String>,
    catalog: &ManifestCatalog,
) -> ConfiguredSource {
    ConfiguredSource {
        source_key,
        source_id: catalog.manifest.source().id.clone(),
        name: catalog.manifest.source().name.clone(),
        description: catalog.manifest.source().description.clone(),
        locator,
        repository_key,
    }
}

fn configured_from_repository_manifest(
    repository_key: String,
    locator: Locator,
    manifest: &RepositoryManifest,
) -> ConfiguredRepository {
    ConfiguredRepository {
        repository_key,
        repository_id: manifest.repository.id.clone(),
        name: manifest.repository.name.clone(),
        description: manifest.repository.description.clone(),
        locator,
    }
}

fn repository_snapshot(
    definition: ConfiguredRepository,
    revision: String,
    path: PathBuf,
    manifest: RepositoryManifest,
) -> Result<RepositorySnapshot, String> {
    let _ = manifest.canonical_sources()?;
    Ok(RepositorySnapshot {
        definition,
        revision,
        path,
        manifest,
    })
}

fn parse_sources_file(path: &Path, contents: &[u8]) -> Result<SourcesConfig, String> {
    let value = serde_json::from_slice::<serde_json::Value>(contents)
        .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("{} has no valid version.", path.display()))?;
    match version {
        version if version == u64::from(SOURCES_VERSION) => {
            let file = serde_json::from_value::<SourcesFile>(value)
                .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
            Ok(SourcesConfig {
                repositories: file.repositories,
                sources: file.sources,
            })
        }
        version if version == u64::from(LEGACY_SOURCES_VERSION) => {
            let file = serde_json::from_value::<LegacySourcesFile>(value)
                .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
            Ok(SourcesConfig {
                repositories: Vec::new(),
                sources: file
                    .sources
                    .into_iter()
                    .map(|source| ConfiguredSource {
                        source_key: source.source_key,
                        source_id: source.source_id,
                        name: source.name,
                        description: source.description,
                        locator: Locator::Git { url: source.url },
                        repository_key: None,
                    })
                    .collect(),
            })
        }
        _ => Err(format!(
            "{} uses an unsupported source configuration version; reset the development app data.",
            path.display()
        )),
    }
}

fn validate_sources_config(config: &SourcesConfig) -> Result<(), String> {
    validate_repositories(&config.repositories)?;
    validate_sources(&config.sources, &config.repositories)?;
    Ok(())
}

fn validate_repositories(repositories: &[ConfiguredRepository]) -> Result<(), String> {
    let mut keys = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut locators = BTreeSet::new();
    for repository in repositories {
        let locator = Locator::parse(repository.locator.kind(), repository.locator.url())?;
        if locator.repository_key() != repository.repository_key
            || locator.url() != repository.locator.url()
            || locator.kind() != repository.locator.kind()
        {
            return Err(format!(
                "Source repository {} does not match its locator-derived repositoryKey.",
                repository.repository_id
            ));
        }
        validate_repository_id(&repository.repository_id)?;
        if repository.name.is_empty() || repository.description.is_empty() {
            return Err(format!(
                "Source repository {} has incomplete metadata.",
                repository.repository_id
            ));
        }
        if !keys.insert(repository.repository_key.as_str())
            || !ids.insert(repository.repository_id.as_str())
            || !locators.insert((locator.kind(), locator.identity_key().to_string()))
        {
            return Err(
                "Source repository configuration contains a duplicate locator, repositoryKey, or repositoryId."
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn validate_sources(
    sources: &[ConfiguredSource],
    repositories: &[ConfiguredRepository],
) -> Result<(), String> {
    let repository_keys = repositories
        .iter()
        .map(|repository| repository.repository_key.as_str())
        .collect::<BTreeSet<_>>();
    let mut keys = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut locators = BTreeSet::new();
    for source in sources {
        let locator = Locator::parse(source.locator.kind(), source.locator.url())?;
        if locator.source_key() != source.source_key
            || locator.url() != source.locator.url()
            || locator.kind() != source.locator.kind()
        {
            return Err(format!(
                "Source {} does not match its locator-derived sourceKey.",
                source.source_id
            ));
        }
        if !valid_manifest_source_id(&source.source_id) {
            return Err(format!("Invalid configured sourceId: {}", source.source_id));
        }
        if source.name.is_empty() || source.description.is_empty() {
            return Err(format!(
                "Source {} has incomplete manifest metadata.",
                source.source_id
            ));
        }
        if let Some(repository_key) = &source.repository_key {
            if !repository_keys.contains(repository_key.as_str()) {
                // Provenance is display-only; a removed catalog must not invalidate the source.
            }
        }
        if !keys.insert(source.source_key.as_str())
            || !ids.insert(source.source_id.as_str())
            || !locators.insert((locator.kind(), locator.identity_key().to_string()))
        {
            return Err(
                "Source configuration contains a duplicate locator, sourceKey, or sourceId."
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn validate_repository_id(value: &str) -> Result<(), String> {
    if validate_repository_id_shape(value) {
        Ok(())
    } else {
        Err(format!("Invalid configured repositoryId: {value}"))
    }
}

fn validate_repository_id_shape(value: &str) -> bool {
    (2..=32).contains(&value.len())
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && !value.ends_with('-')
        && !value.contains("--")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
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

impl CurrentPointer {
    fn validators(&self) -> ArtifactValidators {
        ArtifactValidators {
            etag: self.etag.clone(),
            last_modified: self.last_modified.clone(),
        }
    }
}

fn read_current_pointer(root: &Path) -> Result<Option<CurrentPointer>, String> {
    recover_current_pointer(root)?;
    let path = root.join(CURRENT_POINTER_FILE);
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
    };
    let value = serde_json::from_slice::<serde_json::Value>(&contents)
        .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            format!(
                "{} contains an invalid source revision pointer.",
                path.display()
            )
        })?;
    let pointer = if version == u64::from(CURRENT_POINTER_VERSION) {
        serde_json::from_value::<CurrentPointer>(value)
            .map_err(|error| format!("Could not parse {}: {error}", path.display()))?
    } else if version == u64::from(LEGACY_CURRENT_POINTER_VERSION) {
        let legacy = serde_json::from_value::<LegacyCurrentPointer>(value)
            .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
        CurrentPointer {
            version: CURRENT_POINTER_VERSION,
            revision: legacy.commit,
            etag: None,
            last_modified: None,
        }
    } else {
        return Err(format!(
            "{} contains an invalid source revision pointer.",
            path.display()
        ));
    };
    if !valid_commit_sha(&pointer.revision) {
        return Err(format!(
            "{} contains an invalid source revision pointer.",
            path.display()
        ));
    }
    Ok(Some(pointer))
}

fn write_current_pointer(
    root: &Path,
    revision: &str,
    validators: &ArtifactValidators,
) -> Result<(), String> {
    if !valid_commit_sha(revision) {
        return Err("Cannot activate an invalid source revision.".to_string());
    }
    let pointer = CurrentPointer {
        version: CURRENT_POINTER_VERSION,
        revision: revision.to_string(),
        etag: validators.etag.clone(),
        last_modified: validators.last_modified.clone(),
    };
    let mut contents = serde_json::to_vec_pretty(&pointer)
        .map_err(|error| format!("Could not serialize the source pointer: {error}"))?;
    contents.push(b'\n');
    fs::create_dir_all(root)
        .map_err(|error| format!("Could not create {}: {error}", root.display()))?;
    atomic_write_with_backup(
        root,
        &root.join(CURRENT_POINTER_FILE),
        &root.join(CURRENT_POINTER_BACKUP_FILE),
        "current-writing",
        &contents,
    )
}

fn recover_current_pointer(root: &Path) -> Result<(), String> {
    let path = root.join(CURRENT_POINTER_FILE);
    if path.exists() {
        return Ok(());
    }
    let backup = root.join(CURRENT_POINTER_BACKUP_FILE);
    match fs_retry::rename(&backup, &path) {
        Ok(()) => sync_directory(root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not recover {}: {error}", path.display())),
    }
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceValidationError {
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceValidationReport {
    pub source_id: String,
    pub valid_installs: usize,
    pub errors: Vec<SourceValidationError>,
}

pub fn validate_source(input: &str) -> Result<SourceValidationReport, String> {
    if input.starts_with("https://") || input.starts_with("ssh://") {
        validate_remote_source(&Locator::parse(LocatorKind::Git, input)?)
    } else {
        Ok(report_catalog(&read_manifest_catalog(
            Path::new(input),
            "validation",
        )?))
    }
}

pub fn validate_source_locator(
    kind: LocatorKind,
    url: &str,
) -> Result<SourceValidationReport, String> {
    validate_remote_source(&Locator::parse(kind, url)?)
}

pub fn validate_source_repository_locator(
    kind: LocatorKind,
    url: &str,
) -> Result<RepositoryValidationReport, String> {
    validate_remote_repository(&Locator::parse(kind, url)?)
}

pub(crate) fn validate_remote_repository(
    locator: &Locator,
) -> Result<RepositoryValidationReport, String> {
    let cache = temporary_path(&std::env::temp_dir(), "skill-manager-repository-validation");
    fs::create_dir(&cache)
        .map_err(|error| format!("Could not create {}: {error}", cache.display()))?;
    let result = prepare_new_repository(locator, &cache);
    let report = match result {
        Ok(candidate) => {
            let report = report_manifest(&candidate.manifest);
            discard_repository(&candidate);
            report
        }
        Err(error) => {
            let _ = fs_retry::remove_dir_all(&cache);
            return Err(error);
        }
    };
    let _ = fs_retry::remove_dir_all(&cache);
    report
}

fn validate_remote_source(locator: &Locator) -> Result<SourceValidationReport, String> {
    let cache = temporary_path(&std::env::temp_dir(), "skill-manager-validation");
    fs::create_dir(&cache)
        .map_err(|error| format!("Could not create {}: {error}", cache.display()))?;
    let result = prepare_new_source(locator, &cache, None, None);
    let report = match result {
        Ok(candidate) => {
            let report = report_catalog(&candidate.catalog);
            discard_candidate(&candidate);
            report
        }
        Err(error) => {
            let _ = fs_retry::remove_dir_all(&cache);
            return Err(error);
        }
    };
    let _ = fs_retry::remove_dir_all(&cache);
    Ok(report)
}

fn report_catalog(catalog: &ManifestCatalog) -> SourceValidationReport {
    SourceValidationReport {
        source_id: catalog.manifest.source().id.clone(),
        valid_installs: catalog.items.len(),
        errors: catalog
            .errors
            .iter()
            .map(|error| SourceValidationError {
                path: error.path.clone(),
                message: error.message.clone(),
            })
            .collect(),
    }
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
                  "version": 2,
                  "source": {{ "id": "{source_id}", "name": "Test", "description": "Test source" }},
                  "packages": [{{
                    "id": "review",
                    "components": [{{"kind": "skill", "path": "skills/review"}}]
                  }}]
                }}"#
            ),
        )
        .expect("manifest");
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "source"]);
        repository
    }

    #[test]
    fn validate_source_reports_a_local_catalog() {
        let repository = repository("acme");
        let report = validate_source(repository.path().to_str().expect("utf-8")).expect("report");
        assert_eq!(report.source_id, "acme");
        assert_eq!(report.valid_installs, 1);
        assert!(report.errors.is_empty());
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
        crate::sources::copy_directory(repository.path(), &copied).expect("copy source");
        fs_retry::remove_dir_all(&copied.join(".git")).expect("remove Git metadata");
        let catalog = read_manifest_catalog(&copied, source_key).expect("catalog");
        let definition = configured_from_catalog(
            source_key.to_string(),
            Locator::Git {
                url: "https://example.com/test".to_string(),
            },
            None,
            &catalog,
        );
        let candidate = SourceCandidate {
            definition,
            commit: commit.clone(),
            path: copied,
            catalog,
            staged: true,
            validators: ArtifactValidators::default(),
        };
        let snapshot = activate_candidate(cache.path(), candidate).expect("activate");
        assert_eq!(snapshot.commit, commit);
        assert_eq!(
            snapshot.path,
            revision_path(cache.path(), source_key, &commit)
        );
        assert!(snapshot.path.join("skill-manager.json").is_file());
        assert_eq!(
            read_current_pointer(&source_root)
                .expect("pointer")
                .map(|pointer| pointer.revision),
            Some(commit)
        );
    }

    #[test]
    fn duplicate_manifest_namespaces_are_rejected_for_different_urls() {
        let sources = [
            configured("https://example.com/one", "acme", "One"),
            configured("https://example.com/two", "acme", "Two"),
        ];
        assert!(validate_sources(&sources, &[])
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
        crate::sources::copy_directory(repository.path(), &copied).expect("copy");
        fs_retry::remove_dir_all(&copied.join(".git")).expect("remove Git metadata");
        write_current_pointer(
            &source_cache_root(cache.path(), &configured.source_key),
            &commit,
            &ArtifactValidators::default(),
        )
        .expect("pointer");
        assert!(load_current(cache.path(), &configured)
            .expect_err("changed namespace")
            .contains("changed its manifest id"));
    }

    #[test]
    fn interrupted_current_pointer_replacement_recovers_the_previous_revision() {
        let cache = tempfile::tempdir().expect("cache");
        let source_root = source_cache_root(cache.path(), "source-test");
        let commit = "a".repeat(40);
        write_current_pointer(&source_root, &commit, &ArtifactValidators::default())
            .expect("pointer");
        fs_retry::rename(
            &source_root.join(CURRENT_POINTER_FILE),
            &source_root.join(CURRENT_POINTER_BACKUP_FILE),
        )
        .expect("simulate interrupted replacement");

        assert_eq!(
            read_current_pointer(&source_root)
                .expect("recovered pointer")
                .map(|pointer| pointer.revision),
            Some(commit)
        );
        assert!(source_root.join(CURRENT_POINTER_FILE).is_file());
    }

    #[test]
    fn v4_sources_file_migrates_git_urls_to_locators() {
        let config = tempfile::tempdir().expect("config");
        fs::write(
            sources_path(config.path()),
            r#"{
              "version": 4,
              "sources": [{
                "sourceKey": "source-41d130b3115ae73a",
                "sourceId": "skillbook",
                "name": "Skillbook",
                "description": "Jacob's canonical library of portable Agent Skills.",
                "url": "https://github.com/jacobragsdale/skillbook"
              }]
            }"#,
        )
        .expect("v4 file");
        let parsed = read_sources_config(config.path()).expect("migrate");
        assert!(parsed.repositories.is_empty());
        assert_eq!(parsed.sources[0].locator.kind(), LocatorKind::Git);
        assert_eq!(
            parsed.sources[0].url(),
            "https://github.com/jacobragsdale/skillbook"
        );
        write_sources_config(config.path(), &parsed).expect("write v5");
        let written = fs::read_to_string(sources_path(config.path())).expect("read");
        assert!(written.contains("\"version\": 5"));
        assert!(written.contains("\"kind\": \"git\""));
    }

    #[test]
    fn removing_a_repository_leaves_opted_in_sources() {
        let config = tempfile::tempdir().expect("config");
        let cache = tempfile::tempdir().expect("cache");
        let locator = Locator::parse(LocatorKind::Git, "https://github.com/acme/catalog.git")
            .expect("locator");
        let source = configured("https://github.com/acme/review.git", "review", "Review");
        let mut source = source;
        source.repository_key = Some(locator.repository_key());
        let repositories = vec![ConfiguredRepository {
            repository_key: locator.repository_key(),
            repository_id: "acme".to_string(),
            name: "Acme".to_string(),
            description: "Catalog".to_string(),
            locator,
        }];
        write_sources_config(
            config.path(),
            &SourcesConfig {
                repositories,
                sources: vec![source.clone()],
            },
        )
        .expect("write");
        let mut remaining = read_sources_config(config.path()).expect("read");
        remaining.repositories.clear();
        write_sources_config(config.path(), &remaining).expect("remove repo");
        remove_repository_cache(cache.path(), source.repository_key.as_deref().expect("key"))
            .expect("cache");
        let after = read_sources_config(config.path()).expect("after");
        assert!(after.repositories.is_empty());
        assert_eq!(after.sources.len(), 1);
        assert_eq!(after.sources[0].source_id, "review");
    }

    fn configured(url: &str, source_id: &str, name: &str) -> ConfiguredSource {
        let locator = Locator::parse(LocatorKind::Git, url).expect("identity");
        ConfiguredSource {
            source_key: locator.source_key(),
            source_id: source_id.to_string(),
            name: name.to_string(),
            description: format!("{name} source"),
            locator,
            repository_key: None,
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
