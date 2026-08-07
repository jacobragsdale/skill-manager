//! Transactional generic item installation, lifecycle execution, and migration.

use crate::catalog_v1::{
    materialize_agent_skill, CatalogItem, ResolvedDestination, AGENT_SKILL_KIND,
};
use crate::domain::{InstallOwnership, BUILT_IN_SOURCE_ID};
use crate::fs_retry;
use crate::install::{install_ownership, marker_source_id, next_backup_path, path_entry_exists};
use crate::ledger::{
    self, InstallationLedger, InstallationRecord, LifecyclePhase, OwnedPath, OwnedPathKind,
};
use crate::manifest::{
    Architecture, CommandStep, DestinationAnchor, ManifestAction, OperatingSystem,
    PlatformSelector, Program,
};
use crate::process::{self, OutputCallback};
use crate::source_v1::{ConfiguredSource, SourceSnapshot};
use crate::sources::{copy_directory, temporary_path};
use serde::Serialize;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug)]
pub(crate) struct AnchorPaths {
    pub(crate) home: PathBuf,
    pub(crate) config: PathBuf,
    pub(crate) data: PathBuf,
    pub(crate) local_data: PathBuf,
    pub(crate) cache: PathBuf,
}

impl AnchorPaths {
    pub(crate) fn from_system() -> Result<Self, String> {
        if let Some(root) = crate::qa_paths::root()? {
            return Ok(Self {
                home: root.join("home"),
                config: root.join("config"),
                data: root.join("data"),
                local_data: root.join("local-data"),
                cache: root.join("cache"),
            });
        }
        Ok(Self {
            home: dirs::home_dir()
                .ok_or_else(|| "Could not find your home directory.".to_string())?,
            config: dirs::config_dir()
                .ok_or_else(|| "Could not find your configuration directory.".to_string())?,
            data: dirs::data_dir()
                .ok_or_else(|| "Could not find your data directory.".to_string())?,
            local_data: dirs::data_local_dir()
                .ok_or_else(|| "Could not find your local data directory.".to_string())?,
            cache: dirs::cache_dir()
                .ok_or_else(|| "Could not find your cache directory.".to_string())?,
        })
    }

    pub(crate) fn app_data(&self) -> PathBuf {
        self.data.join("skill-manager")
    }

    pub(crate) fn logs(&self) -> PathBuf {
        self.app_data().join("logs")
    }

    pub(crate) fn resolve(&self, destination: &ResolvedDestination) -> Result<PathBuf, String> {
        self.resolve_relative(destination.anchor, &destination.path)
    }

    pub(crate) fn resolve_owned(&self, owned: &OwnedPath) -> Result<PathBuf, String> {
        self.resolve_relative(owned.anchor, Path::new(&owned.path))
    }

    fn resolve_relative(
        &self,
        anchor: DestinationAnchor,
        relative: &Path,
    ) -> Result<PathBuf, String> {
        if relative.as_os_str().is_empty() || relative.is_absolute() {
            return Err("Owned destinations must be non-empty relative paths.".to_string());
        }
        let root = match anchor {
            DestinationAnchor::Home => &self.home,
            DestinationAnchor::Config => &self.config,
            DestinationAnchor::Data => &self.data,
            DestinationAnchor::LocalData => &self.local_data,
            DestinationAnchor::Cache => &self.cache,
        };
        let resolved = root.join(relative);
        let state_roots = [
            self.config.join("skill-manager"),
            self.data.join("skill-manager"),
            self.local_data.join("skill-manager"),
            self.cache.join("skill-manager"),
        ];
        if state_roots
            .iter()
            .any(|state_root| resolved == *state_root || resolved.starts_with(state_root))
        {
            return Err(format!(
                "Destination {} is inside Skill Manager's own state.",
                resolved.display()
            ));
        }
        Ok(resolved)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ItemStatus {
    Available,
    Installed,
    UpdateAvailable,
    Removed,
    Modified,
    Conflict,
    SourceConflict,
    Incomplete,
    Unsupported,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExecutionLog {
    pub(crate) step_id: String,
    pub(crate) stdout_path: String,
    pub(crate) stderr_path: String,
    pub(crate) success: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationOutcome {
    pub(crate) incomplete: bool,
    pub(crate) logs: Vec<ExecutionLog>,
    pub(crate) backup_paths: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemovalPathWarning {
    pub(crate) path: String,
    pub(crate) modified: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemovalItemPlan {
    pub(crate) id: String,
    pub(crate) paths: Vec<RemovalPathWarning>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceRemovalPlan {
    pub(crate) source_id: String,
    pub(crate) executable_cleanup: bool,
    pub(crate) items: Vec<RemovalItemPlan>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MigrationResult {
    None,
    Migrated,
    Attention(String),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Operation {
    Install,
    Update,
    Uninstall,
    SourceAction,
    ItemAction,
}

impl Operation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update => "update",
            Self::Uninstall => "uninstall",
            Self::SourceAction => "source-action",
            Self::ItemAction => "item-action",
        }
    }
}

struct StagedMapping {
    staging: PathBuf,
    target: PathBuf,
    owned: OwnedPath,
}

struct MovedPath {
    target: PathBuf,
    backup: PathBuf,
    persistent: bool,
}

pub(crate) fn item_status(
    anchors: &AnchorPaths,
    ledger: &InstallationLedger,
    item: Option<&CatalogItem>,
    canonical_id: &str,
) -> ItemStatus {
    if item.is_some_and(|item| !platform_supported(item.platform.as_ref())) {
        return ItemStatus::Unsupported;
    }
    let Some(record) = ledger.items.get(canonical_id) else {
        return item.map_or(ItemStatus::Removed, |item| {
            if item.mappings.iter().any(|mapping| {
                anchors
                    .resolve(&mapping.destination)
                    .is_ok_and(|path| path_entry_exists(&path))
            }) {
                ItemStatus::Conflict
            } else {
                ItemStatus::Available
            }
        });
    };
    if !matches!(record.lifecycle_phase, LifecyclePhase::Complete) {
        return ItemStatus::Incomplete;
    }
    if item.is_some_and(|item| item.source_key != record.source_key) {
        return ItemStatus::SourceConflict;
    }
    if !record_paths_match(anchors, record) {
        return ItemStatus::Modified;
    }
    match item {
        None => ItemStatus::Removed,
        Some(item) if item.digest != record.item_digest => ItemStatus::UpdateAvailable,
        Some(_) => ItemStatus::Installed,
    }
}

pub(crate) fn install_item(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    executable_allowed: bool,
    on_output: OutputCallback,
) -> Result<OperationOutcome, String> {
    install_item_with_policy(
        anchors,
        source,
        snapshot,
        item,
        executable_allowed,
        false,
        on_output,
    )
}

pub(crate) fn replace_item(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    executable_allowed: bool,
    on_output: OutputCallback,
) -> Result<OperationOutcome, String> {
    install_item_with_policy(
        anchors,
        source,
        snapshot,
        item,
        executable_allowed,
        true,
        on_output,
    )
}

fn install_item_with_policy(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    executable_allowed: bool,
    replace_unmanaged: bool,
    on_output: OutputCallback,
) -> Result<OperationOutcome, String> {
    if !platform_supported(item.platform.as_ref()) {
        return Err(format!("{} is not supported on this platform.", item.id));
    }
    let mut ledger_state = ledger::read(&anchors.app_data())?;
    let existing = ledger_state.items.get(&item.id).cloned();
    if replace_unmanaged && existing.is_some() {
        return Err(format!(
            "{} is already managed; use the normal update operation.",
            item.id
        ));
    }
    if let Some(record) = &existing {
        if record.source_key != source.source_key {
            return Err(format!("{} is owned by a different source.", item.id));
        }
        if matches!(
            record.lifecycle_phase,
            LifecyclePhase::PostInstallIncomplete | LifecyclePhase::PostUpdateIncomplete
        ) && record_paths_match(anchors, record)
        {
            return retry_post_hook(
                anchors,
                source,
                snapshot,
                item,
                record,
                &mut ledger_state,
                executable_allowed,
                on_output,
            );
        }
        if !record_paths_match(anchors, record) {
            return Err(format!(
                "{} contains local changes and cannot be updated.",
                item.id
            ));
        }
        if record.item_digest == item.digest {
            return Err(format!("{} is already installed.", item.id));
        }
    }
    let operation = if existing.is_some() {
        Operation::Update
    } else {
        Operation::Install
    };
    let (pre_steps, post_steps, incomplete_phase) = match operation {
        Operation::Install => (
            item.hooks.pre_install.as_slice(),
            item.hooks.post_install.as_slice(),
            LifecyclePhase::PostInstallIncomplete,
        ),
        Operation::Update => (
            item.hooks.pre_update.as_slice(),
            item.hooks.post_update.as_slice(),
            LifecyclePhase::PostUpdateIncomplete,
        ),
        _ => unreachable!("installation operation"),
    };
    let mut logs = run_steps(
        anchors,
        source,
        snapshot,
        Some(item),
        pre_steps,
        operation,
        executable_allowed,
        Arc::clone(&on_output),
    )?;
    let staged = stage_item(anchors, snapshot, item)?;
    let new_record = InstallationRecord {
        source_key: source.source_key.clone(),
        source_url: source.url.clone(),
        source_id: source.source_id.clone(),
        local_id: item.local_id.clone(),
        commit: snapshot.commit.clone(),
        item_digest: item.digest.clone(),
        materialized_skill_name: item.materialized_skill_name.clone(),
        destination_roots: staged.iter().map(|mapping| mapping.owned.clone()).collect(),
        lifecycle_phase: LifecyclePhase::Complete,
        retained_snapshot: snapshot.path.display().to_string(),
    };
    let backup_paths = activate_transaction(
        anchors,
        &mut ledger_state,
        &item.id,
        existing.as_ref(),
        staged,
        new_record,
        replace_unmanaged,
    )?;

    match run_steps(
        anchors,
        source,
        snapshot,
        Some(item),
        post_steps,
        operation,
        executable_allowed,
        on_output,
    ) {
        Ok(post_logs) => {
            logs.extend(post_logs);
            Ok(OperationOutcome {
                incomplete: false,
                logs,
                backup_paths: backup_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
            })
        }
        Err(error) => {
            let mut ledger_state = ledger::read(&anchors.app_data())?;
            if let Some(record) = ledger_state.items.get_mut(&item.id) {
                record.lifecycle_phase = incomplete_phase;
            }
            ledger::write(&anchors.app_data(), &ledger_state)?;
            Err(format!(
                "{error} Files remain installed, and {} is marked Incomplete.",
                item.id
            ))
        }
    }
}

pub(crate) fn uninstall_item(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    force_modified: bool,
    executable_allowed: bool,
    on_output: OutputCallback,
) -> Result<OperationOutcome, String> {
    let mut ledger_state = ledger::read(&anchors.app_data())?;
    let record = ledger_state
        .items
        .get(&item.id)
        .cloned()
        .ok_or_else(|| format!("{} is not installed.", item.id))?;
    if record.source_key != source.source_key {
        return Err(format!("{} is owned by a different source.", item.id));
    }
    if record.lifecycle_phase == LifecyclePhase::PostUninstallIncomplete {
        let logs = run_steps(
            anchors,
            source,
            snapshot,
            Some(item),
            &item.hooks.post_uninstall,
            Operation::Uninstall,
            executable_allowed,
            on_output,
        )?;
        ledger_state.items.remove(&item.id);
        ledger::write(&anchors.app_data(), &ledger_state)?;
        return Ok(OperationOutcome {
            incomplete: false,
            logs,
            backup_paths: Vec::new(),
        });
    }
    if !force_modified && !record_paths_match(anchors, &record) {
        return Err(format!(
            "{} contains local changes and was not removed.",
            item.id
        ));
    }
    let mut logs = run_steps(
        anchors,
        source,
        snapshot,
        Some(item),
        &item.hooks.pre_uninstall,
        Operation::Uninstall,
        executable_allowed,
        Arc::clone(&on_output),
    )?;
    let moved = remove_owned_paths(anchors, &record)?;
    let mut pending_record = record.clone();
    pending_record.destination_roots.clear();
    pending_record.lifecycle_phase = LifecyclePhase::PostUninstallIncomplete;
    ledger_state.items.insert(item.id.clone(), pending_record);
    if let Err(error) = ledger::write(&anchors.app_data(), &ledger_state) {
        rollback_moved(&moved);
        return Err(error);
    }
    cleanup_moved(&moved)?;

    match run_steps(
        anchors,
        source,
        snapshot,
        Some(item),
        &item.hooks.post_uninstall,
        Operation::Uninstall,
        executable_allowed,
        on_output,
    ) {
        Ok(post_logs) => {
            logs.extend(post_logs);
            let mut ledger_state = ledger::read(&anchors.app_data())?;
            ledger_state.items.remove(&item.id);
            ledger::write(&anchors.app_data(), &ledger_state)?;
            Ok(OperationOutcome {
                incomplete: false,
                logs,
                backup_paths: Vec::new(),
            })
        }
        Err(error) => Err(format!(
            "{error} Files were removed, but {} remains Incomplete until its post-uninstall hook succeeds.",
            item.id
        )),
    }
}

pub(crate) fn run_item_action(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    action_id: &str,
    executable_allowed: bool,
    on_output: OutputCallback,
) -> Result<OperationOutcome, String> {
    let action = item
        .actions
        .iter()
        .find(|action| action.id == action_id)
        .ok_or_else(|| format!("Unknown item action: {}@{action_id}", item.id))?;
    run_action(
        anchors,
        source,
        snapshot,
        Some(item),
        action,
        Operation::ItemAction,
        executable_allowed,
        on_output,
    )
}

pub(crate) fn run_source_action(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    action_id: &str,
    executable_allowed: bool,
    on_output: OutputCallback,
) -> Result<OperationOutcome, String> {
    let action = snapshot
        .catalog
        .manifest
        .actions
        .iter()
        .find(|action| action.id == action_id)
        .ok_or_else(|| format!("Unknown source action: {}/@{action_id}", source.source_id))?;
    run_action(
        anchors,
        source,
        snapshot,
        None,
        action,
        Operation::SourceAction,
        executable_allowed,
        on_output,
    )
}

pub(crate) fn source_removal_plan(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
) -> Result<SourceRemovalPlan, String> {
    let ledger_state = ledger::read(&anchors.app_data())?;
    let mut items = Vec::new();
    for (id, record) in ledger_state
        .items
        .iter()
        .filter(|(_, record)| record.source_key == source.source_key)
    {
        let mut paths = Vec::new();
        for owned in &record.destination_roots {
            let path = anchors.resolve_owned(owned)?;
            let modified = if !path_entry_exists(&path) {
                false
            } else {
                match ledger::path_digest(&path, owned.kind) {
                    Ok(digest) => digest != owned.installed_digest,
                    Err(_) => true,
                }
            };
            paths.push(RemovalPathWarning {
                path: path.display().to_string(),
                modified,
            });
        }
        items.push(RemovalItemPlan {
            id: id.clone(),
            paths,
        });
    }
    Ok(SourceRemovalPlan {
        source_id: source.source_id.clone(),
        executable_cleanup: source.executable,
        items,
    })
}

pub(crate) fn migrate_legacy_agent_skill(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    unique_exact_match: bool,
    executable_allowed: bool,
) -> Result<MigrationResult, String> {
    if item.kind != AGENT_SKILL_KIND
        || ledger::read(&anchors.app_data())?
            .items
            .contains_key(&item.id)
    {
        return Ok(MigrationResult::None);
    }
    let Some(metadata) = &item.agent_skill else {
        return Ok(MigrationResult::None);
    };
    let legacy = anchors
        .home
        .join(".agents")
        .join("skills")
        .join(&metadata.local_name);
    if !path_entry_exists(&legacy) {
        return Ok(MigrationResult::None);
    }
    for mapping in &item.mappings {
        let destination = anchors.resolve(&mapping.destination)?;
        if path_entry_exists(&destination) {
            return Ok(MigrationResult::Attention(format!(
                "{} already exists; {} was not migrated.",
                destination.display(),
                legacy.display()
            )));
        }
    }

    let source_path = snapshot.path.join(
        item.mappings
            .first()
            .ok_or_else(|| format!("{} has no source mapping.", item.id))?
            .source
            .as_str(),
    );
    let original_digest = crate::digest::directory_digest(&source_path)?;
    let installed_digest = crate::digest::directory_digest(&legacy).ok();
    let symlink =
        fs::symlink_metadata(&legacy).is_ok_and(|metadata| metadata.file_type().is_symlink());
    let exact = installed_digest.as_deref() == Some(original_digest.as_str());
    let owned = match install_ownership(&legacy) {
        InstallOwnership::Legacy => source.is_built_in() && exact,
        InstallOwnership::Managed(marker) => {
            let owner = marker_source_id(&marker);
            let owner_matches = owner == Some(source.source_key.as_str())
                || (source.source_id == BUILT_IN_SOURCE_ID && owner == Some(BUILT_IN_SOURCE_ID));
            owner_matches && installed_digest.as_deref() == Some(marker.skill_digest.as_str())
        }
        InstallOwnership::Unmanaged => unique_exact_match && exact,
    };
    if !owned {
        return Ok(MigrationResult::Attention(format!(
            "{} is modified, ambiguous, or owned elsewhere; it requires manual migration.",
            legacy.display()
        )));
    }

    let backup = if symlink {
        let backup = next_backup_path(&anchors.home, &metadata.local_name)?;
        fs_retry::rename(&legacy, &backup)
            .map_err(|error| format!("Could not back up {}: {error}", legacy.display()))?;
        Some(backup)
    } else {
        None
    };
    let result = install_item(
        anchors,
        source,
        snapshot,
        item,
        executable_allowed,
        Arc::new(|_, _| {}),
    );
    if let Err(error) = result {
        if let Some(backup) = backup {
            let _ = fs_retry::rename(&backup, &legacy);
        }
        return Err(format!("Could not migrate {}: {error}", item.id));
    }
    if !symlink {
        fs_retry::remove_dir_all(&legacy).map_err(|error| {
            format!(
                "{} was installed under its namespace, but the old managed copy could not be removed: {error}",
                item.id
            )
        })?;
    }
    Ok(MigrationResult::Migrated)
}

#[allow(clippy::too_many_arguments)]
fn retry_post_hook(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    record: &InstallationRecord,
    ledger_state: &mut InstallationLedger,
    executable_allowed: bool,
    on_output: OutputCallback,
) -> Result<OperationOutcome, String> {
    let (steps, operation) = match record.lifecycle_phase {
        LifecyclePhase::PostInstallIncomplete => (&item.hooks.post_install, Operation::Install),
        LifecyclePhase::PostUpdateIncomplete => (&item.hooks.post_update, Operation::Update),
        _ => return Err(format!("{} is not awaiting a post-install hook.", item.id)),
    };
    let logs = run_steps(
        anchors,
        source,
        snapshot,
        Some(item),
        steps,
        operation,
        executable_allowed,
        on_output,
    )?;
    if let Some(record) = ledger_state.items.get_mut(&item.id) {
        record.lifecycle_phase = LifecyclePhase::Complete;
    }
    ledger::write(&anchors.app_data(), ledger_state)?;
    Ok(OperationOutcome {
        incomplete: false,
        logs,
        backup_paths: Vec::new(),
    })
}

fn stage_item(
    anchors: &AnchorPaths,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
) -> Result<Vec<StagedMapping>, String> {
    let mut staged = Vec::new();
    for mapping in &item.mappings {
        let source = snapshot.path.join(&mapping.source);
        let target = anchors.resolve(&mapping.destination)?;
        let parent = target
            .parent()
            .ok_or_else(|| format!("{} has no parent.", target.display()))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        let label = target
            .file_name()
            .and_then(OsStr::to_str)
            .map_or("item-installing".to_string(), |name| {
                format!("{name}-installing")
            });
        let staging = temporary_path(parent, &label);
        let result = if source.is_dir() {
            if let Some(effective_name) = item.materialized_skill_name.as_deref() {
                materialize_agent_skill(&source, &staging, effective_name)
                    .map(|digest| (OwnedPathKind::Directory, digest))
            } else {
                copy_directory(&source, &staging)?;
                ledger::path_digest(&staging, OwnedPathKind::Directory)
                    .map(|digest| (OwnedPathKind::Directory, digest))
            }
        } else if source.is_file() {
            fs::copy(&source, &staging).map_err(|error| {
                format!(
                    "Could not stage {} at {}: {error}",
                    source.display(),
                    staging.display()
                )
            })?;
            ledger::path_digest(&staging, OwnedPathKind::File)
                .map(|digest| (OwnedPathKind::File, digest))
        } else {
            Err(format!(
                "Source mapping {} no longer exists.",
                source.display()
            ))
        };
        match result {
            Ok((kind, digest)) => staged.push(StagedMapping {
                staging,
                target,
                owned: OwnedPath {
                    anchor: mapping.destination.anchor,
                    path: normalized_relative(&mapping.destination.path),
                    kind,
                    installed_digest: digest,
                },
            }),
            Err(error) => {
                cleanup_staged(&staged);
                if path_entry_exists(&staging) {
                    remove_path(&staging);
                }
                return Err(error);
            }
        }
    }
    Ok(staged)
}

fn activate_transaction(
    anchors: &AnchorPaths,
    ledger_state: &mut InstallationLedger,
    id: &str,
    previous: Option<&InstallationRecord>,
    staged: Vec<StagedMapping>,
    new_record: InstallationRecord,
    replace_unmanaged: bool,
) -> Result<Vec<PathBuf>, String> {
    let new_targets = staged
        .iter()
        .map(|mapping| mapping.target.clone())
        .collect::<BTreeSet<_>>();
    let mut old_paths = Vec::new();
    if let Some(previous) = previous {
        for owned in &previous.destination_roots {
            let path = anchors.resolve_owned(owned)?;
            if path_entry_exists(&path) && !new_targets.contains(&path) {
                old_paths.push(path);
            }
        }
    }
    if previous.is_none() {
        let exact_adoption = staged.iter().all(|mapping| {
            path_entry_exists(&mapping.target)
                && !fs::symlink_metadata(&mapping.target)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                && ledger::path_digest(&mapping.target, mapping.owned.kind)
                    .is_ok_and(|digest| digest == mapping.owned.installed_digest)
        });
        if exact_adoption {
            cleanup_staged(&staged);
            ledger_state.items.insert(id.to_string(), new_record);
            ledger::write(&anchors.app_data(), ledger_state)?;
            return Ok(Vec::new());
        }
    }
    let mut unmanaged_targets = BTreeSet::new();
    for mapping in &staged {
        if path_entry_exists(&mapping.target) {
            let owned = previous.is_some_and(|record| {
                record.destination_roots.iter().any(|owned| {
                    anchors
                        .resolve_owned(owned)
                        .is_ok_and(|path| path == mapping.target)
                        && ledger::path_digest(&mapping.target, owned.kind)
                            .is_ok_and(|digest| digest == owned.installed_digest)
                })
            });
            if !owned {
                if !replace_unmanaged {
                    cleanup_staged(&staged);
                    return Err(format!(
                        "{} already exists and is not an unmodified owned destination.",
                        mapping.target.display()
                    ));
                }
                unmanaged_targets.insert(mapping.target.clone());
            }
        }
    }

    let mut moved = Vec::new();
    let persistent_root = if unmanaged_targets.is_empty() {
        None
    } else {
        let label = id.replace('/', "-");
        let root = next_backup_path(&anchors.home, &label)?;
        fs::create_dir_all(&root)
            .map_err(|error| format!("Could not create {}: {error}", root.display()))?;
        Some(root)
    };
    for (index, target) in staged
        .iter()
        .map(|mapping| &mapping.target)
        .chain(old_paths.iter())
        .enumerate()
    {
        if path_entry_exists(target) {
            let persistent = unmanaged_targets.contains(target);
            let backup = if persistent {
                let filename = target
                    .file_name()
                    .and_then(OsStr::to_str)
                    .unwrap_or("destination");
                persistent_root
                    .as_ref()
                    .expect("unmanaged targets have a backup root")
                    .join(format!("{index}-{filename}"))
            } else {
                temporary_path(
                    target.parent().expect("destination parent"),
                    "item-previous",
                )
            };
            if let Err(error) = fs_retry::rename(target, &backup) {
                rollback_moved(&moved);
                cleanup_staged(&staged);
                return Err(format!("Could not prepare {}: {error}", target.display()));
            }
            moved.push(MovedPath {
                target: target.clone(),
                backup,
                persistent,
            });
        }
    }
    let mut activated = Vec::<PathBuf>::new();
    for mapping in &staged {
        if let Err(error) = fs_retry::rename(&mapping.staging, &mapping.target) {
            for target in activated.iter().rev() {
                remove_path(target);
            }
            rollback_moved(&moved);
            cleanup_staged(&staged);
            return Err(format!(
                "Could not activate {}: {error}",
                mapping.target.display()
            ));
        }
        activated.push(mapping.target.clone());
    }
    ledger_state.items.insert(id.to_string(), new_record);
    if let Err(error) = ledger::write(&anchors.app_data(), ledger_state) {
        for target in activated.iter().rev() {
            remove_path(target);
        }
        rollback_moved(&moved);
        return Err(error);
    }
    cleanup_moved(&moved)?;
    Ok(moved
        .into_iter()
        .filter(|moved| moved.persistent)
        .map(|moved| moved.backup)
        .collect())
}

fn remove_owned_paths(
    anchors: &AnchorPaths,
    record: &InstallationRecord,
) -> Result<Vec<MovedPath>, String> {
    let mut moved = Vec::new();
    for owned in &record.destination_roots {
        let target = anchors.resolve_owned(owned)?;
        if !path_entry_exists(&target) {
            continue;
        }
        let backup = temporary_path(
            target.parent().expect("destination parent"),
            "item-removing",
        );
        if let Err(error) = fs_retry::rename(&target, &backup) {
            rollback_moved(&moved);
            return Err(format!(
                "Could not prepare {} for removal: {error}",
                target.display()
            ));
        }
        moved.push(MovedPath {
            target,
            backup,
            persistent: false,
        });
    }
    Ok(moved)
}

#[allow(clippy::too_many_arguments)]
fn run_action(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: Option<&CatalogItem>,
    action: &ManifestAction,
    operation: Operation,
    executable_allowed: bool,
    on_output: OutputCallback,
) -> Result<OperationOutcome, String> {
    if !platform_supported(action.when.as_ref()) {
        return Err(format!(
            "Action {} is not supported on this platform.",
            action.id
        ));
    }
    let logs = run_steps(
        anchors,
        source,
        snapshot,
        item,
        &action.steps,
        operation,
        executable_allowed,
        on_output,
    )?;
    Ok(OperationOutcome {
        incomplete: false,
        logs,
        backup_paths: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
fn run_steps(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: Option<&CatalogItem>,
    steps: &[CommandStep],
    operation: Operation,
    executable_allowed: bool,
    on_output: OutputCallback,
) -> Result<Vec<ExecutionLog>, String> {
    if steps.is_empty() {
        return Ok(Vec::new());
    }
    if !executable_allowed {
        return Err(format!(
            "Executable trust is required before {} may run {} hooks or actions.",
            source.source_id,
            operation.as_str()
        ));
    }
    let mut logs = Vec::new();
    for step in steps {
        if !platform_supported(step.when.as_ref()) {
            continue;
        }
        let program = match &step.program {
            Program::Source(program) => {
                let path = snapshot.path.join(&program.source);
                let metadata = fs::symlink_metadata(&path).map_err(|error| {
                    format!("Could not inspect executable {}: {error}", path.display())
                })?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(format!(
                        "{} is not a regular source executable.",
                        path.display()
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o111 == 0 {
                        return Err(format!("{} is not executable.", path.display()));
                    }
                }
                path
            }
            Program::System(program) => PathBuf::from(&program.system),
        };
        let mut command = process::command(&program);
        command.args(&step.args).current_dir(&snapshot.path);
        set_command_environment(&mut command, anchors, source, snapshot, item, operation);
        let output = process::run(
            command,
            &format!("{}-{}", operation.as_str(), step.id),
            Duration::from_secs(u64::from(step.timeout_seconds)),
            &anchors.logs(),
            Arc::clone(&on_output),
        )?;
        let success = output.status.success();
        logs.push(ExecutionLog {
            step_id: step.id.clone(),
            stdout_path: output.stdout_log.display().to_string(),
            stderr_path: output.stderr_log.display().to_string(),
            success,
        });
        if !success {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let detail = if !stderr.trim().is_empty() {
                stderr.trim()
            } else {
                stdout.trim()
            };
            return Err(format!(
                "Command {} failed with {}{} Logs: {}, {}",
                step.id,
                output.status,
                if detail.is_empty() {
                    ".".to_string()
                } else {
                    format!(": {detail}.")
                },
                output.stdout_log.display(),
                output.stderr_log.display()
            ));
        }
    }
    Ok(logs)
}

fn set_command_environment(
    command: &mut std::process::Command,
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: Option<&CatalogItem>,
    operation: Operation,
) {
    let item_id = item.map_or_else(
        || format!("{}/@source", source.source_id),
        |item| item.id.clone(),
    );
    let local_id = item.map_or("", |item| item.local_id.as_str());
    let skill_name = item
        .and_then(|item| item.materialized_skill_name.as_deref())
        .unwrap_or("");
    let local_skill_name = item
        .and_then(|item| item.agent_skill.as_ref())
        .map_or("", |metadata| metadata.local_name.as_str());
    command
        .env("SKILL_MANAGER_SOURCE_ID", &source.source_id)
        .env("SOURCE_KEY", &source.source_key)
        .env("ITEM_ID", item_id)
        .env("LOCAL_ITEM_ID", local_id)
        .env("SKILL_NAME", skill_name)
        .env("LOCAL_SKILL_NAME", local_skill_name)
        .env("COMMIT", &snapshot.commit)
        .env("OPERATION", operation.as_str())
        .env("SKILL_MANAGER_SOURCE_SNAPSHOT", &snapshot.path)
        .env("SKILL_MANAGER_HOME", &anchors.home)
        .env("SKILL_MANAGER_CONFIG", &anchors.config)
        .env("SKILL_MANAGER_DATA", &anchors.data)
        .env("SKILL_MANAGER_LOCAL_DATA", &anchors.local_data)
        .env("SKILL_MANAGER_CACHE", &anchors.cache);
}

pub(crate) fn platform_supported(selector: Option<&PlatformSelector>) -> bool {
    let Some(selector) = selector else {
        return true;
    };
    let os_supported = selector.os.is_empty()
        || selector.os.iter().any(|os| {
            matches!(
                (os, std::env::consts::OS),
                (OperatingSystem::Macos, "macos")
                    | (OperatingSystem::Linux, "linux")
                    | (OperatingSystem::Windows, "windows")
            )
        });
    let arch_supported = selector.arch.is_empty()
        || selector.arch.iter().any(|arch| {
            matches!(
                (arch, std::env::consts::ARCH),
                (Architecture::X86_64, "x86_64") | (Architecture::Aarch64, "aarch64")
            )
        });
    os_supported && arch_supported
}

fn record_paths_match(anchors: &AnchorPaths, record: &InstallationRecord) -> bool {
    record.destination_roots.iter().all(|owned| {
        anchors.resolve_owned(owned).is_ok_and(|path| {
            !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink())
                && ledger::path_digest(&path, owned.kind)
                    .is_ok_and(|digest| digest == owned.installed_digest)
        })
    })
}

fn normalized_relative(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn cleanup_staged(staged: &[StagedMapping]) {
    for mapping in staged {
        if path_entry_exists(&mapping.staging) {
            remove_path(&mapping.staging);
        }
    }
}

fn rollback_moved(moved: &[MovedPath]) {
    for moved in moved.iter().rev() {
        if path_entry_exists(&moved.target) {
            remove_path(&moved.target);
        }
        let _ = fs_retry::rename(&moved.backup, &moved.target);
    }
}

fn cleanup_moved(moved: &[MovedPath]) -> Result<(), String> {
    for moved in moved {
        if !moved.persistent && path_entry_exists(&moved.backup) {
            remove_path_result(&moved.backup)?;
        }
    }
    Ok(())
}

fn remove_path(path: &Path) {
    let _ = remove_path_result(path);
}

fn remove_path_result(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_dir() {
        fs_retry::remove_dir_all(path)
            .map_err(|error| format!("Could not remove {}: {error}", path.display()))
    } else {
        fs_retry::remove_file(path)
            .map_err(|error| format!("Could not remove {}: {error}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_v1::read_manifest_catalog;
    use crate::manifest::SystemProgram;
    use crate::source_v1::BUILT_IN_SOURCE_KEY;
    use std::sync::Mutex;

    fn anchors(root: &Path) -> AnchorPaths {
        AnchorPaths {
            home: root.join("home"),
            config: root.join("config"),
            data: root.join("data"),
            local_data: root.join("local-data"),
            cache: root.join("cache"),
        }
    }

    fn snapshot(root: &Path) -> (ConfiguredSource, SourceSnapshot, CatalogItem) {
        let source_root = root.join("source");
        let skill = source_root.join("skills/review");
        fs::create_dir_all(&skill).expect("skill");
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: review\ndescription: Review code\nlicense: MIT\n---\nBody\n",
        )
        .expect("skill");
        fs::write(
            source_root.join("skill-manager.json"),
            r#"{
              "version": 1,
              "source": { "id": "skillbook", "name": "Skillbook", "description": "Skills" },
              "agentSkills": [{ "include": ["skills/*"], "destinations": [{ "anchor": "home", "path": ".agents/skills/${skill.name}" }] }]
            }"#,
        )
        .expect("manifest");
        let catalog = read_manifest_catalog(&source_root, BUILT_IN_SOURCE_KEY).expect("catalog");
        let source = ConfiguredSource::built_in();
        let item = catalog.items["review"].clone();
        let snapshot = SourceSnapshot {
            definition: source.clone(),
            commit: "a".repeat(40),
            path: source_root,
            catalog,
        };
        (source, snapshot, item)
    }

    fn system_step(id: &str, script: &str) -> CommandStep {
        #[cfg(unix)]
        let (system, args) = (
            "/bin/sh".to_string(),
            vec!["-c".to_string(), script.to_string()],
        );
        #[cfg(windows)]
        let (system, args) = (
            "cmd.exe".to_string(),
            vec![
                "/D".to_string(),
                "/S".to_string(),
                "/C".to_string(),
                script.to_string(),
            ],
        );
        CommandStep {
            id: id.to_string(),
            program: Program::System(SystemProgram { system }),
            args,
            timeout_seconds: 10,
            when: None,
        }
    }

    #[test]
    fn agent_skill_install_materializes_prefixed_directory_and_frontmatter() {
        let root = tempfile::tempdir().expect("root");
        let anchors = anchors(root.path());
        let (source, snapshot, item) = snapshot(root.path());
        install_item(
            &anchors,
            &source,
            &snapshot,
            &item,
            false,
            Arc::new(|_, _| {}),
        )
        .expect("install");
        let target = anchors.home.join(".agents/skills/skillbook-review");
        assert!(target.is_dir());
        assert!(fs::read_to_string(target.join("SKILL.md"))
            .expect("skill")
            .contains("name: skillbook-review"));
        let ledger = ledger::read(&anchors.app_data()).expect("ledger");
        assert_eq!(
            ledger.items["skillbook/review"]
                .materialized_skill_name
                .as_deref(),
            Some("skillbook-review")
        );
        assert_eq!(
            item_status(&anchors, &ledger, Some(&item), &item.id),
            ItemStatus::Installed
        );
    }

    #[test]
    fn exact_unmanaged_legacy_skill_migrates_to_the_namespaced_destination() {
        let root = tempfile::tempdir().expect("root");
        let anchors = anchors(root.path());
        let (source, snapshot, item) = snapshot(root.path());
        let legacy = anchors.home.join(".agents/skills/review");
        fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("legacy parent");
        copy_directory(&snapshot.path.join("skills/review"), &legacy).expect("legacy skill");

        assert!(matches!(
            migrate_legacy_agent_skill(&anchors, &source, &snapshot, &item, true, false)
                .expect("migration"),
            MigrationResult::Migrated
        ));
        let namespaced = anchors.home.join(".agents/skills/skillbook-review");
        assert!(!legacy.exists());
        assert!(namespaced.is_dir());
        assert!(fs::read_to_string(namespaced.join("SKILL.md"))
            .expect("materialized skill")
            .contains("name: skillbook-review"));
        assert!(ledger::read(&anchors.app_data())
            .expect("ledger")
            .items
            .contains_key("skillbook/review"));
    }

    #[test]
    fn modified_legacy_skill_requires_attention_and_remains_untouched() {
        let root = tempfile::tempdir().expect("root");
        let anchors = anchors(root.path());
        let (source, snapshot, item) = snapshot(root.path());
        let legacy = anchors.home.join(".agents/skills/review");
        fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("legacy parent");
        copy_directory(&snapshot.path.join("skills/review"), &legacy).expect("legacy skill");
        fs::write(legacy.join("local.txt"), "local change").expect("local change");

        let result = migrate_legacy_agent_skill(&anchors, &source, &snapshot, &item, true, false)
            .expect("migration result");
        assert!(
            matches!(result, MigrationResult::Attention(message) if message.contains("manual"))
        );
        assert!(legacy.join("local.txt").is_file());
        assert!(!anchors
            .home
            .join(".agents/skills/skillbook-review")
            .exists());
        assert!(ledger::read(&anchors.app_data())
            .expect("ledger")
            .items
            .is_empty());
    }

    #[test]
    fn legacy_migration_preserves_both_paths_when_the_destination_conflicts() {
        let root = tempfile::tempdir().expect("root");
        let anchors = anchors(root.path());
        let (source, snapshot, item) = snapshot(root.path());
        let legacy = anchors.home.join(".agents/skills/review");
        let namespaced = anchors.home.join(".agents/skills/skillbook-review");
        fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("legacy parent");
        copy_directory(&snapshot.path.join("skills/review"), &legacy).expect("legacy skill");
        fs::create_dir_all(&namespaced).expect("conflicting destination");
        fs::write(namespaced.join("keep.txt"), "different").expect("conflicting content");

        let result = migrate_legacy_agent_skill(&anchors, &source, &snapshot, &item, true, false)
            .expect("migration result");
        assert!(
            matches!(result, MigrationResult::Attention(message) if message.contains("already exists"))
        );
        assert!(legacy.join("SKILL.md").is_file());
        assert_eq!(
            fs::read_to_string(namespaced.join("keep.txt")).expect("conflicting content"),
            "different"
        );
        assert!(ledger::read(&anchors.app_data())
            .expect("ledger")
            .items
            .is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn unmanaged_replacement_keeps_a_backup_and_never_writes_through_a_symlink() {
        let root = tempfile::tempdir().expect("root");
        let anchors = anchors(root.path());
        let (source, snapshot, item) = snapshot(root.path());
        let target = anchors.home.join(".agents/skills/skillbook-review");
        let external = root.path().join("external");
        fs::create_dir_all(target.parent().expect("target parent")).expect("parent");
        fs::create_dir_all(&external).expect("external");
        fs::write(external.join("keep.txt"), "untouched").expect("external file");
        std::os::unix::fs::symlink(&external, &target).expect("symlink");

        assert_eq!(
            item_status(
                &anchors,
                &ledger::InstallationLedger::default(),
                Some(&item),
                &item.id,
            ),
            ItemStatus::Conflict
        );
        let outcome = replace_item(
            &anchors,
            &source,
            &snapshot,
            &item,
            false,
            Arc::new(|_, _| {}),
        )
        .expect("replace");
        assert_eq!(outcome.backup_paths.len(), 1);
        assert!(Path::new(&outcome.backup_paths[0]).is_symlink());
        assert_eq!(
            fs::read_to_string(external.join("keep.txt")).expect("external file"),
            "untouched"
        );
        assert!(target.is_dir());
        assert!(!fs::symlink_metadata(&target)
            .expect("target metadata")
            .file_type()
            .is_symlink());
    }

    #[test]
    fn update_moves_destinations_without_rerunning_install_hooks() {
        let root = tempfile::tempdir().expect("root");
        let anchors = anchors(root.path());
        let (source, snapshot, mut item) = snapshot(root.path());
        install_item(
            &anchors,
            &source,
            &snapshot,
            &item,
            false,
            Arc::new(|_, _| {}),
        )
        .expect("install");
        let old = anchors.home.join(".agents/skills/skillbook-review");
        item.digest = "b".repeat(64);
        item.mappings[0].destination.path = PathBuf::from(".agents/skills/skillbook-review-moved");
        item.materialized_skill_name = Some("skillbook-review-moved".to_string());
        install_item(
            &anchors,
            &source,
            &snapshot,
            &item,
            false,
            Arc::new(|_, _| {}),
        )
        .expect("update");
        assert!(!old.exists());
        assert!(anchors
            .home
            .join(".agents/skills/skillbook-review-moved")
            .is_dir());
    }

    #[test]
    fn trusted_hooks_receive_reserved_environment_from_the_pinned_snapshot() {
        let root = tempfile::tempdir().expect("root");
        let anchors = anchors(root.path());
        let (source, snapshot, mut item) = snapshot(root.path());
        #[cfg(unix)]
        let script = r#"mkdir -p "$SKILL_MANAGER_DATA" && printf '%s\n' "$SKILL_MANAGER_SOURCE_ID" "$SOURCE_KEY" "$ITEM_ID" "$LOCAL_ITEM_ID" "$SKILL_NAME" "$LOCAL_SKILL_NAME" "$COMMIT" "$OPERATION" "$SKILL_MANAGER_SOURCE_SNAPSHOT" "$SKILL_MANAGER_HOME" "$SKILL_MANAGER_CONFIG" "$SKILL_MANAGER_DATA" "$SKILL_MANAGER_LOCAL_DATA" "$SKILL_MANAGER_CACHE" "$PWD" > "$SKILL_MANAGER_DATA/hook-env.txt" && printf 'streamed-ok'"#;
        #[cfg(windows)]
        let script = r#"if not exist "%SKILL_MANAGER_DATA%" mkdir "%SKILL_MANAGER_DATA%" & > "%SKILL_MANAGER_DATA%\hook-env.txt" (echo %SKILL_MANAGER_SOURCE_ID%& echo %SOURCE_KEY%& echo %ITEM_ID%& echo %LOCAL_ITEM_ID%& echo %SKILL_NAME%& echo %LOCAL_SKILL_NAME%& echo %COMMIT%& echo %OPERATION%& echo %SKILL_MANAGER_SOURCE_SNAPSHOT%& echo %SKILL_MANAGER_HOME%& echo %SKILL_MANAGER_CONFIG%& echo %SKILL_MANAGER_DATA%& echo %SKILL_MANAGER_LOCAL_DATA%& echo %SKILL_MANAGER_CACHE%& echo %CD%) & <nul set /p "=streamed-ok" & exit /b 0"#;
        item.hooks.post_install = vec![system_step("verify-environment", script)];
        let streamed = Arc::new(Mutex::new(Vec::new()));
        let callback_capture = Arc::clone(&streamed);

        let outcome = install_item(
            &anchors,
            &source,
            &snapshot,
            &item,
            true,
            Arc::new(move |_, bytes| {
                callback_capture
                    .lock()
                    .expect("stream callback")
                    .extend_from_slice(bytes);
            }),
        )
        .expect("trusted install");

        assert_eq!(outcome.logs.len(), 1);
        assert!(outcome.logs[0].success);
        let evidence =
            fs::read_to_string(anchors.data.join("hook-env.txt")).expect("environment evidence");
        let mut actual = evidence
            .lines()
            .map(|line| line.trim_end_matches('\r').trim_start_matches('\u{feff}'))
            .collect::<Vec<_>>();
        let working_directory = actual.pop().expect("working directory");
        let expected = vec![
            "skillbook".to_string(),
            BUILT_IN_SOURCE_KEY.to_string(),
            "skillbook/review".to_string(),
            "review".to_string(),
            "skillbook-review".to_string(),
            "review".to_string(),
            "a".repeat(40),
            "install".to_string(),
            snapshot.path.display().to_string(),
            anchors.home.display().to_string(),
            anchors.config.display().to_string(),
            anchors.data.display().to_string(),
            anchors.local_data.display().to_string(),
            anchors.cache.display().to_string(),
        ];
        assert_eq!(actual, expected);
        assert_eq!(
            fs::canonicalize(working_directory).expect("working directory"),
            fs::canonicalize(&snapshot.path).expect("snapshot directory")
        );
        assert!(String::from_utf8(streamed.lock().expect("stream").clone())
            .expect("UTF-8 stream")
            .contains("streamed-ok"));
    }

    #[test]
    fn failed_post_install_hook_is_retryable_without_reactivating_files() {
        let root = tempfile::tempdir().expect("root");
        let anchors = anchors(root.path());
        let (source, snapshot, mut item) = snapshot(root.path());
        #[cfg(unix)]
        let failure_script = "exit 23";
        #[cfg(windows)]
        let failure_script = "exit /b 23";
        item.hooks.post_install = vec![system_step("post-install", failure_script)];

        let error = install_item(
            &anchors,
            &source,
            &snapshot,
            &item,
            true,
            Arc::new(|_, _| {}),
        )
        .expect_err("failed post-install");
        assert!(error.contains("marked Incomplete"));
        let target = anchors.home.join(".agents/skills/skillbook-review");
        assert!(target.is_dir());
        assert_eq!(
            ledger::read(&anchors.app_data()).expect("ledger").items[&item.id].lifecycle_phase,
            LifecyclePhase::PostInstallIncomplete
        );

        #[cfg(unix)]
        let success_script = "exit 0";
        #[cfg(windows)]
        let success_script = "exit /b 0";
        item.hooks.post_install = vec![system_step("post-install", success_script)];
        let outcome = install_item(
            &anchors,
            &source,
            &snapshot,
            &item,
            true,
            Arc::new(|_, _| {}),
        )
        .expect("retry post-install");
        assert!(!outcome.incomplete);
        assert_eq!(
            ledger::read(&anchors.app_data()).expect("ledger").items[&item.id].lifecycle_phase,
            LifecyclePhase::Complete
        );
    }

    #[test]
    fn modified_owned_paths_block_normal_uninstall_but_force_cleanup_removes_them() {
        let root = tempfile::tempdir().expect("root");
        let anchors = anchors(root.path());
        let (source, snapshot, item) = snapshot(root.path());
        install_item(
            &anchors,
            &source,
            &snapshot,
            &item,
            false,
            Arc::new(|_, _| {}),
        )
        .expect("install");
        let target = anchors.home.join(".agents/skills/skillbook-review");
        fs::write(target.join("local.txt"), "edit").expect("local edit");
        assert!(uninstall_item(
            &anchors,
            &source,
            &snapshot,
            &item,
            false,
            false,
            Arc::new(|_, _| {})
        )
        .expect_err("modified uninstall")
        .contains("local changes"));
        uninstall_item(
            &anchors,
            &source,
            &snapshot,
            &item,
            true,
            false,
            Arc::new(|_, _| {}),
        )
        .expect("forced cleanup");
        assert!(!target.exists());
    }

    #[test]
    fn app_state_destinations_are_always_rejected() {
        let root = tempfile::tempdir().expect("root");
        let anchors = anchors(root.path());
        let destination = ResolvedDestination {
            anchor: DestinationAnchor::Data,
            path: PathBuf::from("skill-manager/owned"),
        };
        assert!(anchors
            .resolve(&destination)
            .expect_err("state destination")
            .contains("own state"));
    }
}
