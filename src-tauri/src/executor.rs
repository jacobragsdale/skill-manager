//! The only filesystem writer for planned package resources.

use crate::agent_profiles::TargetId;
use crate::catalog::{materialize_agent_skill, CatalogComponentKind, CatalogItem};
use crate::fs_retry;
use crate::install::OperationOutcome;
use crate::ledger::{
    self, BindingRecord, InstallationLedger, InstallationRecord, LegacyPathRoots, OwnedPath,
    OwnedPathKind, OwnedResource, OwnedStructuredEntry, OwnedTextBlock, ResourceRecord,
};
use crate::managed_documents;
use crate::paths::SystemPaths;
use crate::planner;
use crate::resource::{DesiredResource, OperationPlan, PathMaterialization, StructuredFormat};
use crate::source::{ConfiguredSource, SourceSnapshot};
use crate::sources::{copy_directory, sync_directory, temporary_path};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const JOURNAL_FILE: &str = "resource-transaction.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TransactionJournal {
    version: u8,
    transaction_id: String,
    mutations: Vec<JournalMutation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct JournalMutation {
    target: String,
    staging: Option<String>,
    backup: Option<String>,
    persistent_backup: bool,
    target_existed: bool,
    original_digest: Option<String>,
}

#[derive(Clone, Debug)]
struct DocumentWork {
    path: PathBuf,
    original: Vec<u8>,
    updated: Vec<u8>,
    persistent_backup: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TargetCleanupPreview {
    pub(crate) target_id: TargetId,
    pub(crate) binding_count: usize,
    pub(crate) resources_removed: Vec<String>,
    pub(crate) resources_retained: Vec<String>,
}

pub(crate) fn recover(paths: &SystemPaths) -> Result<(), String> {
    let journal_path = paths.app_data().join(JOURNAL_FILE);
    let contents = match fs::read(&journal_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "Could not read {}: {error}",
                journal_path.display()
            ))
        }
    };
    let journal = serde_json::from_slice::<TransactionJournal>(&contents)
        .map_err(|error| format!("Could not parse {}: {error}", journal_path.display()))?;
    if journal.version != 1 {
        return Err(format!(
            "{} uses an unsupported transaction journal version.",
            journal_path.display()
        ));
    }
    let committed = read_ledger_raw(paths)?.last_transaction_id.as_deref()
        == Some(journal.transaction_id.as_str());
    if committed {
        cleanup_committed(&journal)?;
    } else {
        rollback(&journal)?;
    }
    fs_retry::remove_file(&journal_path)
        .map_err(|error| format!("Could not remove {}: {error}", journal_path.display()))?;
    sync_directory(&paths.app_data())
}

pub(crate) fn read_ledger(paths: &SystemPaths) -> Result<InstallationLedger, String> {
    recover(paths)?;
    read_ledger_raw(paths)
}

#[cfg(test)]
pub(crate) fn install(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    replace_unmanaged: bool,
    trust_approved: bool,
) -> Result<OperationOutcome, String> {
    install_components(
        paths,
        source,
        snapshot,
        item,
        replace_unmanaged,
        trust_approved,
        None,
    )
}

pub(crate) fn install_components(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    replace_unmanaged: bool,
    trust_approved: bool,
    component_ids: Option<&[String]>,
) -> Result<OperationOutcome, String> {
    recover(paths)?;
    let ledger_state = read_ledger_raw(paths)?;
    let existing = ledger_state.items.get(&item.id).cloned();
    let operate_on = resolve_operate_on(item, component_ids)?;
    let plan = planner::plan_install_components(paths, snapshot, item, Some(&operate_on))?;
    let preview = planner::preview(item, &plan);
    if preview.requires_approval && !trust_approved {
        return Err(format!(
            "{} contains an MCP server and requires explicit Tier 3 approval.",
            item.id
        ));
    }

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
        if !components_match(paths, &ledger_state, &item.id, &operate_on) {
            return Err(format!(
                "{} contains local changes and cannot be updated.",
                item.id
            ));
        }
        let already_selected = operate_on.iter().all(|component_id| {
            planner::selected_component_ids(record, item).contains(component_id)
        });
        if record.item_digest == item.digest
            && already_selected
            && plan_matches_ledger(&ledger_state, &plan)?
        {
            return Err(format!("{} is already installed.", item.id));
        }
    }
    planner::preflight_installed_conflicts(&ledger_state, source, item, &plan)?;
    validate_plan_paths(&plan)?;

    let mut next = ledger_state.clone();
    let removed = if existing.is_some() {
        detach_components(&mut next, &item.id, &operate_on)
    } else {
        Vec::new()
    };
    let replaced_identities = removed
        .iter()
        .map(|resource| resource.identity.clone())
        .collect::<BTreeSet<_>>();
    preflight_new_resources(&next, &plan, &replaced_identities, replace_unmanaged)?;

    for binding in plan.bindings.values() {
        next.bindings.insert(
            binding.id.clone(),
            BindingRecord {
                id: binding.id.clone(),
                installation_id: binding.installation_id.clone(),
                component_id: binding.component_id.clone(),
                target_id: binding.target_id.clone(),
                dialect_id: binding.dialect_id.clone(),
                scope: binding.scope.clone(),
                capability: binding.capability.clone(),
                resource_ids: binding.resource_ids.clone(),
            },
        );
    }

    let transaction_id = transaction_id(&item.id);
    let (journal, installed, backup_paths) = stage_changes(&StageRequest {
        paths,
        transaction_id: &transaction_id,
        plan: &plan,
        removed: &removed,
        remaining_ledger: &next,
        replace_unmanaged,
        force_modified: false,
    })?;
    for mut record in installed {
        if let Some(existing_resource) = next.resource_by_identity_mut(&record.identity) {
            if existing_resource.desired_digest != record.desired_digest {
                cleanup_staging(&journal);
                return Err(format!(
                    "{} has conflicting desired content.",
                    record.identity
                ));
            }
            for consumer in record.consumer_binding_ids.drain(..) {
                if !existing_resource.consumer_binding_ids.contains(&consumer) {
                    existing_resource.consumer_binding_ids.push(consumer);
                }
            }
            existing_resource.consumer_binding_ids.sort();
        } else {
            next.resources.insert(record.id.clone(), record);
        }
    }
    update_document_digests_from_journal(&mut next, &journal)?;
    let selected_component_ids =
        merged_selected_component_ids(item, existing.as_ref(), component_ids, &operate_on);
    let mut binding_ids = next
        .items
        .get(&item.id)
        .map(|record| record.binding_ids.clone())
        .unwrap_or_default();
    binding_ids.extend(plan.bindings.keys().cloned());
    binding_ids.sort();
    binding_ids.dedup();
    let destination = compatibility_destination(&next, &plan, paths)?;
    next.items.insert(
        item.id.clone(),
        InstallationRecord {
            source_key: source.source_key.clone(),
            source_url: source.url().to_string(),
            source_id: source.source_id.clone(),
            local_id: item.local_id.clone(),
            commit: snapshot.commit.clone(),
            item_digest: item.digest.clone(),
            name: item.name.clone(),
            description: item.description.clone(),
            disable_model_invocation: item.disable_model_invocation,
            source: item.source.clone(),
            destination,
            manifest_version: item.manifest_version,
            component_kind: "package".to_string(),
            binding_ids,
            selected_component_ids,
            conflicts_with: item.conflicts_with.clone(),
        },
    );
    next.last_transaction_id = Some(transaction_id);
    commit(paths, &journal, &next)?;
    Ok(OperationOutcome {
        backup_paths: backup_paths
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
    })
}

pub(crate) struct BatchInstall<'a> {
    pub(crate) source: &'a ConfiguredSource,
    pub(crate) snapshot: &'a SourceSnapshot,
    pub(crate) item: &'a CatalogItem,
    pub(crate) replace_unmanaged: bool,
}

pub(crate) fn install_batch(
    paths: &SystemPaths,
    requests: &[BatchInstall<'_>],
    trust_approved: bool,
) -> Result<OperationOutcome, String> {
    if requests.is_empty() {
        return Ok(OperationOutcome::default());
    }
    recover(paths)?;
    let original = read_ledger_raw(paths)?;
    let mut next = original.clone();
    let batch_ids = requests
        .iter()
        .map(|request| request.item.id.clone())
        .collect::<BTreeSet<_>>();
    let mut combined = OperationPlan::default();
    let mut item_plans = BTreeMap::new();
    let mut removed = Vec::new();
    for request in requests {
        let item = request.item;
        let selected = original
            .items
            .get(&item.id)
            .map(|record| planner::selected_component_ids(record, item));
        let plan =
            planner::plan_install_components(paths, request.snapshot, item, selected.as_deref())?;
        if planner::preview(item, &plan).requires_approval && !trust_approved {
            return Err(format!(
                "{} contains an MCP server and requires explicit Tier 3 approval.",
                item.id
            ));
        }
        if item
            .conflicts_with
            .iter()
            .any(|conflict| batch_ids.contains(conflict))
        {
            return Err(format!(
                "{} conflicts with another package in this batch.",
                item.id
            ));
        }
        if let Some(existing) = original.items.get(&item.id) {
            if request.replace_unmanaged {
                return Err(format!(
                    "{} is already managed; use the normal update operation.",
                    item.id
                ));
            }
            if existing.source_key != request.source.source_key {
                return Err(format!("{} is owned by a different source.", item.id));
            }
            if !installation_matches(paths, &original, &item.id) {
                return Err(format!("{} contains local changes.", item.id));
            }
        }
        planner::preflight_installed_conflicts(&original, request.source, item, &plan)?;
        validate_plan_paths(&plan)?;
        removed.extend(detach_installation(&mut next, &item.id));
        merge_plan(&mut combined, &plan)?;
        item_plans.insert(item.id.clone(), plan);
    }
    validate_plan_paths(&combined)?;
    let replaced_identities = removed
        .iter()
        .map(|resource| resource.identity.clone())
        .collect::<BTreeSet<_>>();
    preflight_new_resources(
        &next,
        &combined,
        &replaced_identities,
        requests.iter().any(|request| request.replace_unmanaged),
    )?;
    for binding in combined.bindings.values() {
        next.bindings.insert(
            binding.id.clone(),
            BindingRecord {
                id: binding.id.clone(),
                installation_id: binding.installation_id.clone(),
                component_id: binding.component_id.clone(),
                target_id: binding.target_id.clone(),
                dialect_id: binding.dialect_id.clone(),
                scope: binding.scope.clone(),
                capability: binding.capability.clone(),
                resource_ids: binding.resource_ids.clone(),
            },
        );
    }
    let transaction_id = transaction_id("batch-install");
    let (journal, installed, backup_paths) = stage_changes(&StageRequest {
        paths,
        transaction_id: &transaction_id,
        plan: &combined,
        removed: &removed,
        remaining_ledger: &next,
        replace_unmanaged: requests.iter().any(|request| request.replace_unmanaged),
        force_modified: false,
    })?;
    for mut resource in installed {
        if let Some(existing) = next.resource_by_identity_mut(&resource.identity) {
            for consumer in resource.consumer_binding_ids.drain(..) {
                if !existing.consumer_binding_ids.contains(&consumer) {
                    existing.consumer_binding_ids.push(consumer);
                }
            }
            existing.consumer_binding_ids.sort();
        } else {
            next.resources.insert(resource.id.clone(), resource);
        }
    }
    update_document_digests_from_journal(&mut next, &journal)?;
    for request in requests {
        let item = request.item;
        let plan = &item_plans[&item.id];
        let mut record =
            installation_record(paths, &next, plan, request.source, request.snapshot, item)?;
        record.selected_component_ids = original
            .items
            .get(&item.id)
            .map(|existing| existing.selected_component_ids.clone())
            .unwrap_or_default();
        next.items.insert(item.id.clone(), record);
    }
    next.last_transaction_id = Some(transaction_id);
    commit(paths, &journal, &next)?;
    Ok(OperationOutcome {
        backup_paths: backup_paths
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
    })
}

pub(crate) fn uninstall_batch(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    installation_ids: &[String],
    force_modified: bool,
) -> Result<OperationOutcome, String> {
    if installation_ids.is_empty() {
        return Ok(OperationOutcome::default());
    }
    recover(paths)?;
    let original = read_ledger_raw(paths)?;
    for installation_id in installation_ids {
        let record = original
            .items
            .get(installation_id)
            .ok_or_else(|| format!("{installation_id} is not installed."))?;
        if record.source_key != source.source_key {
            return Err(format!("{installation_id} is owned by a different source."));
        }
        if !force_modified && !installation_matches(paths, &original, installation_id) {
            return Err(format!("{installation_id} contains local changes."));
        }
    }
    let mut next = original;
    let mut removed = Vec::new();
    for installation_id in installation_ids {
        removed.extend(detach_installation(&mut next, installation_id));
    }
    let transaction_id = transaction_id("batch-uninstall");
    let (journal, _, backup_paths) = stage_changes(&StageRequest {
        paths,
        transaction_id: &transaction_id,
        plan: &OperationPlan::default(),
        removed: &removed,
        remaining_ledger: &next,
        replace_unmanaged: false,
        force_modified,
    })?;
    update_document_digests_from_journal(&mut next, &journal)?;
    next.last_transaction_id = Some(transaction_id);
    commit(paths, &journal, &next)?;
    Ok(OperationOutcome {
        backup_paths: backup_paths
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
    })
}

pub(crate) fn reset_source(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    snapshot: Option<&SourceSnapshot>,
) -> Result<OperationOutcome, String> {
    recover(paths)?;
    let original = read_ledger_raw(paths)?;
    let catalog_ids = snapshot
        .map(|snapshot| {
            snapshot
                .catalog
                .items
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let installation_ids = crate::install::source_reset_ids(&original, source, &catalog_ids);
    let mut next = original;
    let mut removed = Vec::new();
    for installation_id in &installation_ids {
        removed.extend(detach_installation(&mut next, installation_id));
    }
    let mut backup_paths = Vec::new();
    if !removed.is_empty() {
        match commit_forced_removal(paths, &mut next, &removed) {
            Ok(backups) => backup_paths.extend(backups),
            Err(_) => {
                persist_reset_ledger(paths, &mut next)?;
                backup_paths.extend(best_effort_remove(paths, &removed));
            }
        }
    }
    backup_paths.extend(remove_leftover_source_skills(
        paths, source, snapshot, &next,
    ));
    backup_paths.sort();
    backup_paths.dedup();
    Ok(OperationOutcome { backup_paths })
}

fn commit_forced_removal(
    paths: &SystemPaths,
    next: &mut InstallationLedger,
    removed: &[ResourceRecord],
) -> Result<Vec<String>, String> {
    let transaction_id = transaction_id("source-reset");
    let (journal, _, backup_paths) = stage_changes(&StageRequest {
        paths,
        transaction_id: &transaction_id,
        plan: &OperationPlan::default(),
        removed,
        remaining_ledger: next,
        replace_unmanaged: false,
        force_modified: true,
    })?;
    update_document_digests_from_journal(next, &journal)?;
    next.last_transaction_id = Some(transaction_id);
    commit(paths, &journal, next)?;
    Ok(backup_paths
        .into_iter()
        .map(|path| path.display().to_string())
        .collect())
}

fn persist_reset_ledger(paths: &SystemPaths, next: &mut InstallationLedger) -> Result<(), String> {
    next.last_transaction_id = Some(transaction_id("source-reset"));
    crate::ledger::write(&paths.app_data(), next)
}

fn best_effort_remove(paths: &SystemPaths, removed: &[ResourceRecord]) -> Vec<String> {
    let mut backup_paths = Vec::new();
    for resource in removed {
        match &resource.owned {
            OwnedResource::Path(owned) => {
                if let Some(backup) = best_effort_remove_path(paths, Path::new(&owned.path)) {
                    backup_paths.push(backup);
                }
            }
            OwnedResource::StructuredEntry(owned) => {
                let _ = strip_structured_entry(Path::new(&owned.document_path), owned);
            }
            OwnedResource::TextBlock(owned) => {
                let _ = strip_text_block(Path::new(&owned.document_path), owned);
            }
        }
    }
    backup_paths
}

fn best_effort_remove_path(paths: &SystemPaths, target: &Path) -> Option<String> {
    if !path_entry_exists(target) {
        return None;
    }
    if let Ok(backup) = mutation_backup(paths, "source-reset", target, true) {
        if fs_retry::rename(target, &backup).is_ok() {
            return Some(backup.display().to_string());
        }
    }
    let _ = remove_any(target);
    None
}

fn strip_structured_entry(path: &Path, owned: &OwnedStructuredEntry) -> Result<(), String> {
    let original = managed_documents::read_or_empty(path, owned.format)?;
    let updated = managed_documents::remove_entries(
        &original,
        owned.format,
        std::slice::from_ref(&owned.key_path),
    )?;
    if updated != original {
        fs::write(path, updated)
            .map_err(|error| format!("Could not update {}: {error}", path.display()))?;
    }
    Ok(())
}

fn strip_text_block(path: &Path, owned: &OwnedTextBlock) -> Result<(), String> {
    let original = fs::read(path).unwrap_or_default();
    let updated =
        managed_documents::remove_text_blocks(&original, std::slice::from_ref(&owned.marker_id))?;
    if updated != original {
        fs::write(path, updated)
            .map_err(|error| format!("Could not update {}: {error}", path.display()))?;
    }
    Ok(())
}

fn remove_leftover_source_skills(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    snapshot: Option<&SourceSnapshot>,
    remaining: &InstallationLedger,
) -> Vec<String> {
    let owned_paths = remaining
        .resources
        .values()
        .filter_map(|resource| match &resource.owned {
            OwnedResource::Path(owned) => Some(normalize_path(Path::new(&owned.path))),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut names = BTreeSet::new();
    if let Some(snapshot) = snapshot {
        for item in snapshot.catalog.items.values() {
            for component in &item.components {
                if component.kind == CatalogComponentKind::Skill {
                    names.insert(component.effective_name.clone());
                }
            }
        }
    }
    let prefix = format!("{}-", source.source_id);
    let mut backup_paths = Vec::new();
    for root in [
        paths.home.join(".agents/skills"),
        paths.home.join(".claude/skills"),
    ] {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with(&prefix) && !names.contains(name) {
                continue;
            }
            let target = entry.path();
            if owned_paths.contains(&normalize_path(&target)) {
                continue;
            }
            if let Some(backup) = best_effort_remove_path(paths, &target) {
                backup_paths.push(backup);
            }
        }
    }
    backup_paths
}

#[cfg(test)]
pub(crate) fn uninstall(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    installation_id: &str,
    force_modified: bool,
) -> Result<OperationOutcome, String> {
    uninstall_components(paths, source, installation_id, None, force_modified)
}

pub(crate) fn uninstall_components(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    installation_id: &str,
    component_ids: Option<&[String]>,
    force_modified: bool,
) -> Result<OperationOutcome, String> {
    recover(paths)?;
    let ledger_state = read_ledger_raw(paths)?;
    let record = ledger_state
        .items
        .get(installation_id)
        .ok_or_else(|| format!("{installation_id} is not installed."))?;
    if record.source_key != source.source_key {
        return Err(format!("{installation_id} is owned by a different source."));
    }
    let operate_on = match component_ids {
        None | Some([]) => None,
        Some(ids) => {
            let installed = binding_component_ids(&ledger_state, record);
            if ids.iter().all(|id| installed.contains(id))
                && installed.iter().all(|id| ids.contains(id))
            {
                None
            } else {
                Some(ids.to_vec())
            }
        }
    };
    if let Some(ids) = &operate_on {
        if !force_modified && !components_match(paths, &ledger_state, installation_id, ids) {
            return Err(format!(
                "{installation_id} contains local changes and cannot be uninstalled."
            ));
        }
    } else if !force_modified && !installation_matches(paths, &ledger_state, installation_id) {
        return Err(format!(
            "{installation_id} contains local changes and cannot be uninstalled."
        ));
    }
    let mut next = ledger_state.clone();
    let removed = match &operate_on {
        Some(ids) => detach_components(&mut next, installation_id, ids),
        None => detach_installation(&mut next, installation_id),
    };
    let transaction_id = transaction_id(installation_id);
    let (journal, _, backup_paths) = stage_changes(&StageRequest {
        paths,
        transaction_id: &transaction_id,
        plan: &OperationPlan::default(),
        removed: &removed,
        remaining_ledger: &next,
        replace_unmanaged: false,
        force_modified,
    })?;
    update_document_digests_from_journal(&mut next, &journal)?;
    next.last_transaction_id = Some(transaction_id);
    commit(paths, &journal, &next)?;
    Ok(OperationOutcome {
        backup_paths: backup_paths
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
    })
}

pub(crate) fn preview_target_cleanup(
    paths: &SystemPaths,
    target_id: TargetId,
) -> Result<TargetCleanupPreview, String> {
    let ledger = read_ledger(paths)?;
    let binding_ids = ledger
        .bindings
        .values()
        .filter(|binding| binding.target_id == target_id.as_str())
        .map(|binding| binding.id.clone())
        .collect::<BTreeSet<_>>();
    let mut removed = Vec::new();
    let mut retained = Vec::new();
    for resource in ledger.resources.values() {
        if !resource
            .consumer_binding_ids
            .iter()
            .any(|binding| binding_ids.contains(binding))
        {
            continue;
        }
        if resource
            .consumer_binding_ids
            .iter()
            .all(|binding| binding_ids.contains(binding))
        {
            removed.push(resource.identity.clone());
        } else {
            retained.push(resource.identity.clone());
        }
    }
    removed.sort();
    retained.sort();
    Ok(TargetCleanupPreview {
        target_id,
        binding_count: binding_ids.len(),
        resources_removed: removed,
        resources_retained: retained,
    })
}

pub(crate) fn disable_target(
    paths: &SystemPaths,
    target_id: TargetId,
    force_modified: bool,
) -> Result<OperationOutcome, String> {
    recover(paths)?;
    let mut next = read_ledger_raw(paths)?;
    let mut binding_ids = next
        .bindings
        .values()
        .filter(|binding| binding.target_id == target_id.as_str())
        .map(|binding| binding.id.clone())
        .collect::<BTreeSet<_>>();
    let affected_installations = binding_ids
        .iter()
        .filter_map(|binding_id| next.bindings.get(binding_id))
        .map(|binding| binding.installation_id.clone())
        .collect::<BTreeSet<_>>();
    for installation_id in &affected_installations {
        let has_other_target = next.bindings.values().any(|binding| {
            binding.installation_id == *installation_id
                && binding.target_id != target_id.as_str()
                && binding.target_id != "skill-manager"
        });
        if !has_other_target {
            binding_ids.extend(
                next.bindings
                    .values()
                    .filter(|binding| {
                        binding.installation_id == *installation_id
                            && binding.target_id == "skill-manager"
                    })
                    .map(|binding| binding.id.clone()),
            );
        }
    }
    if binding_ids.is_empty() {
        return Ok(OperationOutcome::default());
    }
    for binding_id in &binding_ids {
        next.bindings.remove(binding_id);
    }
    for item in next.items.values_mut() {
        item.binding_ids
            .retain(|binding_id| !binding_ids.contains(binding_id));
    }
    let empty_installations = next
        .items
        .iter()
        .filter(|(_, item)| item.binding_ids.is_empty() && item.manifest_version == 2)
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for installation_id in empty_installations {
        next.items.remove(&installation_id);
    }
    let mut orphan_ids = Vec::new();
    for (resource_id, resource) in &mut next.resources {
        resource
            .consumer_binding_ids
            .retain(|binding| !binding_ids.contains(binding));
        if resource.consumer_binding_ids.is_empty() {
            orphan_ids.push(resource_id.clone());
        }
    }
    let removed = orphan_ids
        .into_iter()
        .filter_map(|resource_id| next.resources.remove(&resource_id))
        .collect::<Vec<_>>();
    let transaction_id = transaction_id(target_id.as_str());
    let (journal, _, backup_paths) = stage_changes(&StageRequest {
        paths,
        transaction_id: &transaction_id,
        plan: &OperationPlan::default(),
        removed: &removed,
        remaining_ledger: &next,
        replace_unmanaged: false,
        force_modified,
    })?;
    update_document_digests_from_journal(&mut next, &journal)?;
    next.last_transaction_id = Some(transaction_id);
    commit(paths, &journal, &next)?;
    Ok(OperationOutcome {
        backup_paths: backup_paths
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
    })
}

pub(crate) fn installation_matches(
    paths: &SystemPaths,
    ledger: &InstallationLedger,
    installation_id: &str,
) -> bool {
    let Some(record) = ledger.items.get(installation_id) else {
        return false;
    };
    record.binding_ids.iter().all(|binding_id| {
        ledger.bindings.get(binding_id).is_some_and(|binding| {
            binding.resource_ids.iter().all(|resource_id| {
                ledger
                    .resources
                    .get(resource_id)
                    .is_some_and(|resource| resource_matches(paths, resource).unwrap_or(false))
            })
        })
    })
}

pub(crate) fn resource_matches(
    paths: &SystemPaths,
    resource: &ResourceRecord,
) -> Result<bool, String> {
    match &resource.owned {
        OwnedResource::Path(owned) => {
            let path = validate_absolute_owned_path(paths, Path::new(&owned.path))?;
            if !path_entry_exists(&path) {
                return Ok(false);
            }
            ledger::path_digest(&path, owned.kind).map(|digest| digest == owned.installed_digest)
        }
        OwnedResource::StructuredEntry(owned) => {
            let path = validate_absolute_owned_path(paths, Path::new(&owned.document_path))?;
            let contents = managed_documents::read_or_empty(&path, owned.format)?;
            let value = managed_documents::entry_value(&contents, owned.format, &owned.key_path)?;
            value
                .as_ref()
                .map(managed_documents::value_digest)
                .transpose()
                .map(|digest| digest.as_deref() == Some(owned.value_digest.as_str()))
        }
        OwnedResource::TextBlock(owned) => {
            let path = validate_absolute_owned_path(paths, Path::new(&owned.document_path))?;
            let contents = fs::read(&path).unwrap_or_default();
            managed_documents::text_block_body(&contents, &owned.marker_id).map(|body| {
                body.as_deref()
                    .map(|body| ledger::bytes_digest(body.as_bytes()))
                    == Some(owned.body_digest.clone())
            })
        }
    }
}

fn read_ledger_raw(paths: &SystemPaths) -> Result<InstallationLedger, String> {
    ledger::read(
        &paths.app_data(),
        LegacyPathRoots {
            home: &paths.home,
            config: &paths.config,
            data: &paths.data,
            local_data: &paths.local_data,
            cache: &paths.cache,
        },
    )
}

pub(crate) fn plan_satisfied(
    ledger: &InstallationLedger,
    plan: &OperationPlan,
) -> Result<bool, String> {
    if plan
        .bindings
        .keys()
        .any(|binding_id| !ledger.bindings.contains_key(binding_id))
    {
        return Ok(false);
    }
    for planned in plan.resources.values() {
        let Some(existing) = ledger.resource_by_identity(&planned.desired.identity()) else {
            return Ok(false);
        };
        if existing.desired_digest != planned.desired.desired_digest()? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn plan_matches_ledger(ledger: &InstallationLedger, plan: &OperationPlan) -> Result<bool, String> {
    plan_satisfied(ledger, plan)
}

fn merge_plan(combined: &mut OperationPlan, plan: &OperationPlan) -> Result<(), String> {
    for binding in plan.bindings.values() {
        combined.add_binding(binding.clone())?;
    }
    for planned in plan.resources.values() {
        if let Some(existing) = combined.resources.get_mut(&planned.id) {
            if existing.desired.identity() != planned.desired.identity()
                || existing.desired.desired_digest()? != planned.desired.desired_digest()?
            {
                return Err(format!(
                    "Conflicting batch content targets {}.",
                    planned.desired.identity()
                ));
            }
            for consumer in &planned.consumer_binding_ids {
                if !existing.consumer_binding_ids.contains(consumer) {
                    existing.consumer_binding_ids.push(consumer.clone());
                }
            }
            existing.consumer_binding_ids.sort();
        } else {
            combined
                .resources
                .insert(planned.id.clone(), planned.clone());
        }
    }
    combined.compatibility.extend(plan.compatibility.clone());
    combined.warnings.extend(plan.warnings.clone());
    combined.warnings.sort();
    combined.warnings.dedup();
    Ok(())
}

fn installation_record(
    paths: &SystemPaths,
    ledger: &InstallationLedger,
    plan: &OperationPlan,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
) -> Result<InstallationRecord, String> {
    Ok(InstallationRecord {
        source_key: source.source_key.clone(),
        source_url: source.url().to_string(),
        source_id: source.source_id.clone(),
        local_id: item.local_id.clone(),
        commit: snapshot.commit.clone(),
        item_digest: item.digest.clone(),
        name: item.name.clone(),
        description: item.description.clone(),
        disable_model_invocation: item.disable_model_invocation,
        source: item.source.clone(),
        destination: compatibility_destination(ledger, plan, paths)?,
        manifest_version: item.manifest_version,
        component_kind: "package".to_string(),
        binding_ids: plan.bindings.keys().cloned().collect(),
        selected_component_ids: Vec::new(),
        conflicts_with: item.conflicts_with.clone(),
    })
}

fn resolve_operate_on(
    item: &CatalogItem,
    component_ids: Option<&[String]>,
) -> Result<Vec<String>, String> {
    match component_ids {
        None | Some([]) => Ok(planner::package_component_ids(item)),
        Some(ids) => {
            for component_id in ids {
                planner::validate_component_id(item, component_id)?;
            }
            Ok(ids.to_vec())
        }
    }
}

fn merged_selected_component_ids(
    item: &CatalogItem,
    existing: Option<&InstallationRecord>,
    requested: Option<&[String]>,
    operate_on: &[String],
) -> Vec<String> {
    if requested.is_none() {
        return Vec::new();
    }
    let all = planner::package_component_ids(item);
    let mut selected = match existing {
        Some(record) => planner::selected_component_ids(record, item),
        None => Vec::new(),
    };
    for component_id in operate_on {
        if !selected.contains(component_id) {
            selected.push(component_id.clone());
        }
    }
    if selected.len() == all.len() && all.iter().all(|id| selected.contains(id)) {
        Vec::new()
    } else {
        selected
    }
}

fn binding_component_ids(ledger: &InstallationLedger, record: &InstallationRecord) -> Vec<String> {
    record
        .binding_ids
        .iter()
        .filter_map(|binding_id| {
            ledger
                .bindings
                .get(binding_id)
                .map(|binding| binding.component_id.clone())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn components_match(
    paths: &SystemPaths,
    ledger: &InstallationLedger,
    installation_id: &str,
    component_ids: &[String],
) -> bool {
    let Some(record) = ledger.items.get(installation_id) else {
        return true;
    };
    let component_set = component_ids.iter().cloned().collect::<BTreeSet<_>>();
    record.binding_ids.iter().all(|binding_id| {
        ledger.bindings.get(binding_id).is_none_or(|binding| {
            if !component_set.contains(&binding.component_id) {
                return true;
            }
            binding.resource_ids.iter().all(|resource_id| {
                ledger
                    .resources
                    .get(resource_id)
                    .is_some_and(|resource| resource_matches(paths, resource).unwrap_or(false))
            })
        })
    })
}

fn detach_components(
    ledger: &mut InstallationLedger,
    installation_id: &str,
    component_ids: &[String],
) -> Vec<ResourceRecord> {
    let Some(record) = ledger.items.get(installation_id) else {
        return Vec::new();
    };
    let component_set = component_ids.iter().cloned().collect::<BTreeSet<_>>();
    let binding_ids = record
        .binding_ids
        .iter()
        .filter(|binding_id| {
            ledger
                .bindings
                .get(*binding_id)
                .is_some_and(|binding| component_set.contains(&binding.component_id))
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    for binding_id in &binding_ids {
        ledger.bindings.remove(binding_id);
    }
    let (remaining_binding_ids, was_all) =
        if let Some(record) = ledger.items.get_mut(installation_id) {
            record
                .binding_ids
                .retain(|binding_id| !binding_ids.contains(binding_id));
            let remaining = record.binding_ids.clone();
            let was_all = record.selected_component_ids.is_empty();
            record
                .selected_component_ids
                .retain(|component_id| !component_set.contains(component_id));
            (remaining, was_all)
        } else {
            (Vec::new(), false)
        };
    if was_all {
        let remaining_components = remaining_binding_ids
            .iter()
            .filter_map(|binding_id| {
                ledger
                    .bindings
                    .get(binding_id)
                    .map(|binding| binding.component_id.clone())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(record) = ledger.items.get_mut(installation_id) {
            record.selected_component_ids = remaining_components;
        }
    }
    if remaining_binding_ids.is_empty() {
        ledger.items.remove(installation_id);
    }
    let mut orphan_ids = Vec::new();
    for (resource_id, resource) in &mut ledger.resources {
        resource
            .consumer_binding_ids
            .retain(|consumer| !binding_ids.contains(consumer));
        if resource.consumer_binding_ids.is_empty() {
            orphan_ids.push(resource_id.clone());
        }
    }
    orphan_ids
        .into_iter()
        .filter_map(|resource_id| ledger.resources.remove(&resource_id))
        .collect()
}

fn detach_installation(
    ledger: &mut InstallationLedger,
    installation_id: &str,
) -> Vec<ResourceRecord> {
    let Some(record) = ledger.items.remove(installation_id) else {
        return Vec::new();
    };
    let binding_ids = record.binding_ids.into_iter().collect::<BTreeSet<_>>();
    for binding_id in &binding_ids {
        ledger.bindings.remove(binding_id);
    }
    let mut orphan_ids = Vec::new();
    for (resource_id, resource) in &mut ledger.resources {
        resource
            .consumer_binding_ids
            .retain(|consumer| !binding_ids.contains(consumer));
        if resource.consumer_binding_ids.is_empty() {
            orphan_ids.push(resource_id.clone());
        }
    }
    orphan_ids
        .into_iter()
        .filter_map(|resource_id| ledger.resources.remove(&resource_id))
        .collect()
}

fn preflight_new_resources(
    ledger: &InstallationLedger,
    plan: &OperationPlan,
    replaced_identities: &BTreeSet<String>,
    replace_unmanaged: bool,
) -> Result<(), String> {
    for planned in plan.resources.values() {
        let identity = planned.desired.identity();
        if let Some(existing) = ledger.resource_by_identity(&identity) {
            if existing.desired_digest != planned.desired.desired_digest()? {
                return Err(format!("{identity} is owned with different content."));
            }
            continue;
        }
        if replaced_identities.contains(&identity) {
            continue;
        }
        match &planned.desired {
            DesiredResource::Path(desired) if path_entry_exists(&desired.path) => {
                if !replace_unmanaged {
                    return Err(format!(
                        "{} already exists and is not an owned destination.",
                        desired.path.display()
                    ));
                }
            }
            DesiredResource::StructuredEntry(desired) => {
                let contents =
                    managed_documents::read_or_empty(&desired.document_path, desired.format)?;
                if managed_documents::entry_value(&contents, desired.format, &desired.key_path)?
                    .is_some()
                    && !replace_unmanaged
                {
                    return Err(format!(
                        "Configuration entry {} in {} already exists and is unmanaged.",
                        desired.key_path.join("."),
                        desired.document_path.display()
                    ));
                }
            }
            DesiredResource::TextBlock(desired) => {
                let contents = fs::read(&desired.document_path).unwrap_or_default();
                if managed_documents::text_block_body(&contents, &desired.marker_id)?.is_some()
                    && !replace_unmanaged
                {
                    return Err(format!(
                        "Instruction block {} in {} already exists and is unmanaged.",
                        desired.marker_id,
                        desired.document_path.display()
                    ));
                }
            }
            DesiredResource::Path(_) => {}
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct StageRequest<'a> {
    paths: &'a SystemPaths,
    transaction_id: &'a str,
    plan: &'a OperationPlan,
    removed: &'a [ResourceRecord],
    remaining_ledger: &'a InstallationLedger,
    replace_unmanaged: bool,
    force_modified: bool,
}

fn stage_changes(
    request: &StageRequest<'_>,
) -> Result<(TransactionJournal, Vec<ResourceRecord>, Vec<PathBuf>), String> {
    let StageRequest {
        paths,
        transaction_id,
        plan,
        removed,
        remaining_ledger,
        replace_unmanaged,
        force_modified,
    } = *request;
    let mut mutations = Vec::new();
    let mut installed = Vec::new();
    let mut backup_paths = Vec::new();
    let desired_identities = plan
        .resources
        .values()
        .map(|resource| resource.desired.identity())
        .collect::<BTreeSet<_>>();

    for planned in plan.resources.values() {
        if remaining_ledger
            .resource_by_identity(&planned.desired.identity())
            .is_some()
        {
            installed.push(ResourceRecord {
                id: planned.id.clone(),
                identity: planned.desired.identity(),
                desired_digest: planned.desired.desired_digest()?,
                owned: remaining_ledger
                    .resource_by_identity(&planned.desired.identity())
                    .expect("checked")
                    .owned
                    .clone(),
                consumer_binding_ids: planned.consumer_binding_ids.clone(),
                adapter_id: planned.adapter_id.clone(),
                dialect_id: planned.dialect_id.clone(),
            });
            continue;
        }
        let DesiredResource::Path(desired) = &planned.desired else {
            continue;
        };
        let target = validate_absolute_owned_path(paths, &desired.path)?;
        let staging = stage_path(desired, &target)?;
        let installed_digest = ledger::path_digest(&staging, desired.kind)?;
        let replacing_owned = removed
            .iter()
            .any(|resource| resource.identity == planned.desired.identity());
        let persistent = path_entry_exists(&target) && !replacing_owned && replace_unmanaged;
        let backup = mutation_backup(paths, transaction_id, &target, persistent)?;
        if persistent {
            backup_paths.push(backup.clone());
        }
        mutations.push(JournalMutation {
            target: target.display().to_string(),
            staging: Some(staging.display().to_string()),
            backup: path_entry_exists(&target).then(|| backup.display().to_string()),
            persistent_backup: persistent,
            target_existed: path_entry_exists(&target),
            original_digest: existing_path_digest(&target),
        });
        installed.push(ResourceRecord {
            id: planned.id.clone(),
            identity: planned.desired.identity(),
            desired_digest: planned.desired.desired_digest()?,
            owned: OwnedResource::Path(OwnedPath {
                path: target.display().to_string(),
                kind: desired.kind,
                installed_digest,
            }),
            consumer_binding_ids: planned.consumer_binding_ids.clone(),
            adapter_id: planned.adapter_id.clone(),
            dialect_id: planned.dialect_id.clone(),
        });
    }

    for old in removed {
        if desired_identities.contains(&old.identity) {
            continue;
        }
        let OwnedResource::Path(owned) = &old.owned else {
            continue;
        };
        let target = validate_absolute_owned_path(paths, Path::new(&owned.path))?;
        if !path_entry_exists(&target) {
            continue;
        }
        let matches = resource_matches(paths, old)?;
        if !matches && !force_modified {
            cleanup_mutations(&mutations);
            return Err(format!("{} contains local changes.", target.display()));
        }
        let persistent = !matches && force_modified;
        let backup = mutation_backup(paths, transaction_id, &target, persistent)?;
        if persistent {
            backup_paths.push(backup.clone());
        }
        mutations.push(JournalMutation {
            target: target.display().to_string(),
            staging: None,
            backup: Some(backup.display().to_string()),
            persistent_backup: persistent,
            target_existed: true,
            original_digest: existing_path_digest(&target),
        });
    }

    let documents = stage_documents(request, &mut backup_paths)?;
    for work in documents.values() {
        let target = validate_absolute_owned_path(paths, &work.path)?;
        let staging = stage_bytes(&target, &work.updated)?;
        let backup = mutation_backup(paths, transaction_id, &target, work.persistent_backup)?;
        mutations.push(JournalMutation {
            target: target.display().to_string(),
            staging: Some(staging.display().to_string()),
            backup: path_entry_exists(&target).then(|| backup.display().to_string()),
            persistent_backup: work.persistent_backup,
            target_existed: path_entry_exists(&target),
            original_digest: Some(ledger::bytes_digest(&work.original)),
        });
        let document_digest = ledger::bytes_digest(&work.updated);
        for planned in plan.resources.values() {
            match &planned.desired {
                DesiredResource::StructuredEntry(desired) if desired.document_path == work.path => {
                    installed.push(ResourceRecord {
                        id: planned.id.clone(),
                        identity: planned.desired.identity(),
                        desired_digest: planned.desired.desired_digest()?,
                        owned: OwnedResource::StructuredEntry(OwnedStructuredEntry {
                            document_path: work.path.display().to_string(),
                            format: desired.format,
                            key_path: desired.key_path.clone(),
                            value_digest: managed_documents::value_digest(&desired.value)?,
                            document_digest: document_digest.clone(),
                        }),
                        consumer_binding_ids: planned.consumer_binding_ids.clone(),
                        adapter_id: planned.adapter_id.clone(),
                        dialect_id: planned.dialect_id.clone(),
                    });
                }
                DesiredResource::TextBlock(desired) if desired.document_path == work.path => {
                    installed.push(ResourceRecord {
                        id: planned.id.clone(),
                        identity: planned.desired.identity(),
                        desired_digest: planned.desired.desired_digest()?,
                        owned: OwnedResource::TextBlock(OwnedTextBlock {
                            document_path: work.path.display().to_string(),
                            marker_id: desired.marker_id.clone(),
                            body_digest: ledger::bytes_digest(desired.body.trim_end().as_bytes()),
                            document_digest: document_digest.clone(),
                        }),
                        consumer_binding_ids: planned.consumer_binding_ids.clone(),
                        adapter_id: planned.adapter_id.clone(),
                        dialect_id: planned.dialect_id.clone(),
                    });
                }
                _ => {}
            }
        }
    }

    mutations.sort_by(|left, right| left.target.cmp(&right.target));
    Ok((
        TransactionJournal {
            version: 1,
            transaction_id: transaction_id.to_string(),
            mutations,
        },
        installed,
        backup_paths,
    ))
}

fn stage_documents(
    request: &StageRequest<'_>,
    backup_paths: &mut Vec<PathBuf>,
) -> Result<BTreeMap<String, DocumentWork>, String> {
    let StageRequest {
        paths,
        transaction_id,
        plan,
        removed,
        remaining_ledger,
        replace_unmanaged,
        force_modified,
    } = *request;
    let mut grouped = BTreeMap::<String, (PathBuf, Option<StructuredFormat>)>::new();
    for old in removed {
        match &old.owned {
            OwnedResource::StructuredEntry(owned) => {
                grouped.insert(
                    normalize_path(Path::new(&owned.document_path)),
                    (PathBuf::from(&owned.document_path), Some(owned.format)),
                );
            }
            OwnedResource::TextBlock(owned) => {
                grouped
                    .entry(normalize_path(Path::new(&owned.document_path)))
                    .or_insert((PathBuf::from(&owned.document_path), None));
            }
            OwnedResource::Path(_) => {}
        }
    }
    for planned in plan.resources.values() {
        match &planned.desired {
            DesiredResource::StructuredEntry(desired) => {
                let entry = grouped
                    .entry(normalize_path(&desired.document_path))
                    .or_insert((desired.document_path.clone(), Some(desired.format)));
                if entry.1.is_some_and(|format| format != desired.format) {
                    return Err(format!(
                        "{} is planned with incompatible structured formats.",
                        desired.document_path.display()
                    ));
                }
                entry.1 = Some(desired.format);
            }
            DesiredResource::TextBlock(desired) => {
                grouped
                    .entry(normalize_path(&desired.document_path))
                    .or_insert((desired.document_path.clone(), None));
            }
            DesiredResource::Path(_) => {}
        }
    }
    let desired_identities = plan
        .resources
        .values()
        .map(|resource| resource.desired.identity())
        .collect::<BTreeSet<_>>();
    let mut output = BTreeMap::new();
    for (key, (path, format)) in grouped {
        validate_absolute_owned_path(paths, &path)?;
        let original = match fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => match format {
                Some(format) => managed_documents::read_or_empty(&path, format)?,
                None => Vec::new(),
            },
            Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
        };
        let mut updated = original.clone();
        let mut persistent = false;
        for old in removed {
            if desired_identities.contains(&old.identity) || resource_document(old) != Some(&path) {
                continue;
            }
            let matches = resource_matches(paths, old)?;
            if !matches && !force_modified {
                return Err(format!(
                    "{} contains a modified managed entry.",
                    path.display()
                ));
            }
            if !matches && force_modified {
                persistent = true;
            }
            match &old.owned {
                OwnedResource::StructuredEntry(owned) => {
                    updated = managed_documents::remove_entries(
                        &updated,
                        owned.format,
                        std::slice::from_ref(&owned.key_path),
                    )?;
                }
                OwnedResource::TextBlock(owned) => {
                    updated = managed_documents::remove_text_blocks(
                        &updated,
                        std::slice::from_ref(&owned.marker_id),
                    )?;
                }
                OwnedResource::Path(_) => {}
            }
        }
        for planned in plan.resources.values() {
            match &planned.desired {
                DesiredResource::StructuredEntry(desired) if desired.document_path == path => {
                    let unmanaged = remaining_ledger
                        .resource_by_identity(&planned.desired.identity())
                        .is_none()
                        && !removed
                            .iter()
                            .any(|old| old.identity == planned.desired.identity())
                        && managed_documents::entry_value(
                            &updated,
                            desired.format,
                            &desired.key_path,
                        )?
                        .is_some();
                    if unmanaged && !replace_unmanaged {
                        return Err(format!(
                            "Configuration entry {} in {} is unmanaged.",
                            desired.key_path.join("."),
                            path.display()
                        ));
                    }
                    persistent |= unmanaged;
                    updated = managed_documents::set_entries(
                        &updated,
                        desired.format,
                        &[(desired.key_path.clone(), desired.value.clone())],
                    )?;
                }
                DesiredResource::TextBlock(desired) if desired.document_path == path => {
                    let unmanaged = remaining_ledger
                        .resource_by_identity(&planned.desired.identity())
                        .is_none()
                        && !removed
                            .iter()
                            .any(|old| old.identity == planned.desired.identity())
                        && managed_documents::text_block_body(&updated, &desired.marker_id)?
                            .is_some();
                    if unmanaged && !replace_unmanaged {
                        return Err(format!(
                            "Instruction block {} in {} is unmanaged.",
                            desired.marker_id,
                            path.display()
                        ));
                    }
                    persistent |= unmanaged;
                    updated = managed_documents::set_text_blocks(
                        &updated,
                        &[(desired.marker_id.clone(), desired.body.clone())],
                    )?;
                }
                _ => {}
            }
        }
        if updated == original {
            continue;
        }
        if persistent && path_entry_exists(&path) {
            backup_paths.push(mutation_backup(paths, transaction_id, &path, true)?);
        }
        output.insert(
            key,
            DocumentWork {
                path,
                original,
                updated,
                persistent_backup: persistent,
            },
        );
    }
    Ok(output)
}

fn commit(
    paths: &SystemPaths,
    journal: &TransactionJournal,
    ledger_state: &InstallationLedger,
) -> Result<(), String> {
    write_journal(paths, journal)?;
    if let Err(error) = activate(journal) {
        let rollback_error = rollback(journal).err();
        let _ = fs_retry::remove_file(&paths.app_data().join(JOURNAL_FILE));
        return match rollback_error {
            Some(rollback_error) => Err(format!("{error} Rollback also failed: {rollback_error}")),
            None => Err(error),
        };
    }
    if let Err(error) = ledger::write(&paths.app_data(), ledger_state) {
        let rollback_error = rollback(journal).err();
        let _ = fs_retry::remove_file(&paths.app_data().join(JOURNAL_FILE));
        return match rollback_error {
            Some(rollback_error) => Err(format!("{error} Rollback also failed: {rollback_error}")),
            None => Err(error),
        };
    }
    cleanup_committed(journal)?;
    fs_retry::remove_file(&paths.app_data().join(JOURNAL_FILE))
        .map_err(|error| format!("Could not remove the committed transaction journal: {error}"))?;
    sync_directory(&paths.app_data())
}

fn write_journal(paths: &SystemPaths, journal: &TransactionJournal) -> Result<(), String> {
    fs::create_dir_all(paths.app_data())
        .map_err(|error| format!("Could not create transaction state: {error}"))?;
    let path = paths.app_data().join(JOURNAL_FILE);
    if path.exists() {
        return Err(format!(
            "A pending transaction already exists at {}.",
            path.display()
        ));
    }
    let staging = temporary_path(&paths.app_data(), "resource-transaction-writing");
    let mut bytes = serde_json::to_vec_pretty(journal)
        .map_err(|error| format!("Could not serialize the transaction journal: {error}"))?;
    bytes.push(b'\n');
    fs::write(&staging, bytes)
        .map_err(|error| format!("Could not write {}: {error}", staging.display()))?;
    fs_retry::rename(&staging, &path)
        .map_err(|error| format!("Could not activate {}: {error}", path.display()))?;
    sync_directory(&paths.app_data())
}

fn activate(journal: &TransactionJournal) -> Result<(), String> {
    for mutation in &journal.mutations {
        let target = Path::new(&mutation.target);
        if let Some(expected) = &mutation.original_digest {
            let current = existing_path_digest(target);
            if current.as_ref() != Some(expected) {
                return Err(format!(
                    "{} changed after preflight; no changes were committed.",
                    target.display()
                ));
            }
        } else if mutation.target_existed != path_entry_exists(target) {
            return Err(format!(
                "{} changed existence after preflight; no changes were committed.",
                target.display()
            ));
        }
        if let Some(backup) = &mutation.backup {
            let backup = Path::new(backup);
            if let Some(parent) = backup.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
            }
            fs_retry::rename(target, backup).map_err(|error| {
                format!(
                    "Could not prepare {} for mutation: {error}",
                    target.display()
                )
            })?;
        }
        if let Some(staging) = &mutation.staging {
            let staging = Path::new(staging);
            fs_retry::rename(staging, target)
                .map_err(|error| format!("Could not activate {}: {error}", target.display()))?;
        }
        if let Some(parent) = target.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

fn rollback(journal: &TransactionJournal) -> Result<(), String> {
    let mut errors = Vec::new();
    for mutation in journal.mutations.iter().rev() {
        let target = Path::new(&mutation.target);
        if let Some(backup) = &mutation.backup {
            let backup = Path::new(backup);
            if path_entry_exists(backup) {
                if path_entry_exists(target) {
                    if let Err(error) = remove_any(target) {
                        errors.push(error);
                        continue;
                    }
                }
                if let Err(error) = fs_retry::rename(backup, target) {
                    errors.push(format!("Could not restore {}: {error}", target.display()));
                }
            }
        } else if let Some(staging) = &mutation.staging {
            if !path_entry_exists(Path::new(staging)) && path_entry_exists(target) {
                if let Err(error) = remove_any(target) {
                    errors.push(error);
                }
            }
        }
        if let Some(staging) = &mutation.staging {
            let staging = Path::new(staging);
            if path_entry_exists(staging) {
                let _ = remove_any(staging);
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join(" "))
    }
}

fn cleanup_committed(journal: &TransactionJournal) -> Result<(), String> {
    for mutation in &journal.mutations {
        if let Some(staging) = &mutation.staging {
            let staging = Path::new(staging);
            if path_entry_exists(staging) {
                remove_any(staging)?;
            }
        }
        if !mutation.persistent_backup {
            if let Some(backup) = &mutation.backup {
                let backup = Path::new(backup);
                if path_entry_exists(backup) {
                    remove_any(backup)?;
                }
            }
        }
    }
    Ok(())
}

fn stage_path(desired: &crate::resource::DesiredPath, target: &Path) -> Result<PathBuf, String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("{} has no parent.", target.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    let staging = temporary_path(parent, "resource-installing");
    let result = match desired.kind {
        OwnedPathKind::Directory => match &desired.materialization {
            PathMaterialization::Copy => copy_directory(&desired.source, &staging),
            PathMaterialization::AgentSkill { effective_name } => {
                materialize_agent_skill(&desired.source, &staging, effective_name)
            }
        },
        OwnedPathKind::File => fs::copy(&desired.source, &staging)
            .map(|_| ())
            .map_err(|error| {
                format!(
                    "Could not stage {} at {}: {error}",
                    desired.source.display(),
                    staging.display()
                )
            }),
    };
    if let Err(error) = result {
        let _ = remove_any(&staging);
        return Err(error);
    }
    Ok(staging)
}

fn stage_bytes(target: &Path, contents: &[u8]) -> Result<PathBuf, String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("{} has no parent.", target.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    let staging = temporary_path(parent, "document-writing");
    fs::write(&staging, contents)
        .map_err(|error| format!("Could not write {}: {error}", staging.display()))?;
    Ok(staging)
}

fn mutation_backup(
    paths: &SystemPaths,
    transaction_id: &str,
    target: &Path,
    persistent: bool,
) -> Result<PathBuf, String> {
    if !persistent {
        return Ok(temporary_path(
            target.parent().expect("validated target has parent"),
            "resource-previous",
        ));
    }
    let directory = paths
        .home
        .join(".agents/.skill-manager-backups")
        .join(transaction_id);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not create {}: {error}", directory.display()))?;
    let filename = target
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("resource");
    for suffix in 0..10_000_u16 {
        let name = if suffix == 0 {
            filename.to_string()
        } else {
            format!("{suffix}-{filename}")
        };
        let candidate = directory.join(name);
        if !path_entry_exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(format!(
        "Could not choose a backup path in {}.",
        directory.display()
    ))
}

fn update_document_digests_from_journal(
    ledger: &mut InstallationLedger,
    journal: &TransactionJournal,
) -> Result<(), String> {
    for mutation in &journal.mutations {
        let Some(staging) = &mutation.staging else {
            continue;
        };
        let target = Path::new(&mutation.target);
        let affects_document = ledger
            .resources
            .values()
            .any(|resource| resource_document(resource).is_some_and(|path| path == target));
        if !affects_document {
            continue;
        }
        let digest = ledger::bytes_digest(
            &fs::read(staging).map_err(|error| format!("Could not hash {staging}: {error}"))?,
        );
        for resource in ledger.resources.values_mut() {
            match &mut resource.owned {
                OwnedResource::StructuredEntry(owned)
                    if Path::new(&owned.document_path) == target =>
                {
                    owned.document_digest.clone_from(&digest);
                }
                OwnedResource::TextBlock(owned) if Path::new(&owned.document_path) == target => {
                    owned.document_digest.clone_from(&digest);
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn compatibility_destination(
    ledger: &InstallationLedger,
    plan: &OperationPlan,
    paths: &SystemPaths,
) -> Result<OwnedPath, String> {
    for planned in plan.resources.values() {
        if let Some(resource) = ledger.resource_by_identity(&planned.desired.identity()) {
            match &resource.owned {
                OwnedResource::Path(owned) => return Ok(owned.clone()),
                OwnedResource::StructuredEntry(owned) => {
                    return Ok(OwnedPath {
                        path: owned.document_path.clone(),
                        kind: OwnedPathKind::File,
                        installed_digest: owned.document_digest.clone(),
                    });
                }
                OwnedResource::TextBlock(owned) => {
                    return Ok(OwnedPath {
                        path: owned.document_path.clone(),
                        kind: OwnedPathKind::File,
                        installed_digest: owned.document_digest.clone(),
                    });
                }
            }
        }
    }
    let placeholder = paths.home.join(".agents/skills");
    Ok(OwnedPath {
        path: placeholder.display().to_string(),
        kind: OwnedPathKind::Directory,
        installed_digest: ledger::bytes_digest(b"no physical resource"),
    })
}

fn resource_document(resource: &ResourceRecord) -> Option<&Path> {
    match &resource.owned {
        OwnedResource::StructuredEntry(owned) => Some(Path::new(&owned.document_path)),
        OwnedResource::TextBlock(owned) => Some(Path::new(&owned.document_path)),
        OwnedResource::Path(_) => None,
    }
}

fn validate_plan_paths(plan: &OperationPlan) -> Result<(), String> {
    let mut whole_paths = Vec::new();
    let mut documents = Vec::new();
    for planned in plan.resources.values() {
        match &planned.desired {
            DesiredResource::Path(desired) => whole_paths.push(&desired.path),
            DesiredResource::StructuredEntry(desired) => documents.push(&desired.document_path),
            DesiredResource::TextBlock(desired) => documents.push(&desired.document_path),
        }
    }
    for (index, left) in whole_paths.iter().enumerate() {
        for right in whole_paths.iter().skip(index + 1) {
            if left.starts_with(right) || right.starts_with(left) {
                return Err(format!(
                    "Planned owned paths overlap: {} and {}.",
                    left.display(),
                    right.display()
                ));
            }
        }
        if let Some(document) = documents
            .iter()
            .find(|document| document.starts_with(left.as_path()))
        {
            return Err(format!(
                "Planned path {} contains managed document {}.",
                left.display(),
                document.display()
            ));
        }
    }
    Ok(())
}

fn validate_absolute_owned_path(paths: &SystemPaths, path: &Path) -> Result<PathBuf, String> {
    paths.validate_destination(path)
}

fn existing_path_digest(path: &Path) -> Option<String> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_dir() {
        ledger::path_digest(path, OwnedPathKind::Directory).ok()
    } else {
        ledger::path_digest(path, OwnedPathKind::File).ok()
    }
}

fn transaction_id(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    crate::resource::stable_id("tx", &format!("{label}:{nanos}:{}", std::process::id()))
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
        .to_lowercase()
}

fn path_entry_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn remove_any(path: &Path) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Could not inspect {}: {error}", path.display())),
    };
    if metadata.file_type().is_dir() {
        fs_retry::remove_dir_all(path)
            .map_err(|error| format!("Could not remove {}: {error}", path.display()))
    } else {
        fs_retry::remove_file(path)
            .map_err(|error| format!("Could not remove {}: {error}", path.display()))
    }
}

fn cleanup_mutations(mutations: &[JournalMutation]) {
    for mutation in mutations {
        if let Some(staging) = &mutation.staging {
            let _ = remove_any(Path::new(staging));
        }
    }
}

fn cleanup_staging(journal: &TransactionJournal) {
    cleanup_mutations(&journal.mutations);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::read_manifest_catalog;
    use crate::source::TEST_SOURCE_KEY;

    fn paths(root: &Path) -> SystemPaths {
        SystemPaths {
            home: root.join("home"),
            config: root.join("config"),
            data: root.join("data"),
            local_data: root.join("local-data"),
            cache: root.join("cache"),
        }
    }

    fn fixture(root: &Path) -> (ConfiguredSource, SourceSnapshot, CatalogItem) {
        let source_root = root.join("source");
        fs::create_dir_all(source_root.join("skills/review")).expect("skill");
        fs::write(
            source_root.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review code\n---\nBody\n",
        )
        .expect("skill");
        fs::write(
            source_root.join("skill-manager.json"),
            r#"{"version":2,"source":{"id":"acme","name":"Acme","description":"Test"},"packages":[{"id":"review","components":[{"kind":"skill","path":"skills/review"}]}]}"#,
        )
        .expect("manifest");
        let catalog = read_manifest_catalog(&source_root, TEST_SOURCE_KEY).expect("catalog");
        let source = ConfiguredSource {
            source_key: TEST_SOURCE_KEY.to_string(),
            source_id: "acme".to_string(),
            name: "Acme".to_string(),
            description: "Test".to_string(),
            locator: crate::locator::Locator::display_url(
                "https://nexus.example.com/repository/raw/sources/acme-latest.zip".to_string(),
            ),
            repository_key: None,
        };
        let item = catalog.items["review"].clone();
        let snapshot = SourceSnapshot {
            definition: source.clone(),
            commit: "a".repeat(40),
            path: source_root,
            catalog,
        };
        (source, snapshot, item)
    }

    fn batch_fixture(root: &Path) -> (ConfiguredSource, SourceSnapshot, Vec<CatalogItem>) {
        let source_root = root.join("batch-source");
        for name in ["review", "debug"] {
            let skill_root = source_root.join("skills").join(name);
            fs::create_dir_all(&skill_root).expect("skill");
            fs::write(
                skill_root.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {name} code\n---\nBody\n"),
            )
            .expect("skill file");
        }
        fs::write(
            source_root.join("skill-manager.json"),
            r#"{"version":2,"source":{"id":"acme","name":"Acme","description":"Test"},"packages":[{"id":"review","components":[{"kind":"skill","id":"review","path":"skills/review"}]},{"id":"debug","components":[{"kind":"skill","id":"debug","path":"skills/debug"}]}]}"#,
        )
        .expect("manifest");
        let catalog = read_manifest_catalog(&source_root, TEST_SOURCE_KEY).expect("catalog");
        let source = ConfiguredSource {
            source_key: TEST_SOURCE_KEY.to_string(),
            source_id: "acme".to_string(),
            name: "Acme".to_string(),
            description: "Test".to_string(),
            locator: crate::locator::Locator::display_url(
                "https://nexus.example.com/repository/raw/sources/acme-latest.zip".to_string(),
            ),
            repository_key: None,
        };
        let items = ["review", "debug"]
            .into_iter()
            .map(|id| catalog.items[id].clone())
            .collect();
        let snapshot = SourceSnapshot {
            definition: source.clone(),
            commit: "d".repeat(40),
            path: source_root,
            catalog,
        };
        (source, snapshot, items)
    }

    #[test]
    fn transaction_installs_and_reference_counted_uninstall_removes_path() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        crate::agent_profiles::set_enabled(&paths, crate::agent_profiles::TargetId::Cursor, true)
            .expect("enable");
        let (source, snapshot, item) = fixture(root.path());
        install(&paths, &source, &snapshot, &item, false, false).expect("install");
        assert!(paths.home.join(".agents/skills/acme-review").is_dir());
        let ledger = read_ledger(&paths).expect("ledger");
        assert_eq!(ledger.items.len(), 1);
        assert_eq!(ledger.bindings.len(), 1);
        assert_eq!(ledger.resources.len(), 1);
        uninstall(&paths, &source, &item.id, false).expect("uninstall");
        assert!(!paths.home.join(".agents/skills/acme-review").exists());
    }

    #[test]
    fn batch_preflight_is_all_or_nothing_and_success_uses_one_ledger_commit() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        crate::agent_profiles::set_enabled(&paths, crate::agent_profiles::TargetId::Cursor, true)
            .expect("enable");
        let (source, snapshot, items) = batch_fixture(root.path());
        fs::create_dir_all(paths.home.join(".agents/skills/acme-debug"))
            .expect("unmanaged conflict");
        let requests = items
            .iter()
            .map(|item| BatchInstall {
                source: &source,
                snapshot: &snapshot,
                item,
                replace_unmanaged: false,
            })
            .collect::<Vec<_>>();
        assert!(install_batch(&paths, &requests, false)
            .expect_err("conflict")
            .contains("already exists"));
        assert!(!paths.home.join(".agents/skills/acme-review").exists());
        assert!(read_ledger(&paths).expect("ledger").items.is_empty());

        fs::remove_dir(paths.home.join(".agents/skills/acme-debug")).expect("remove conflict");
        install_batch(&paths, &requests, false).expect("batch install");
        let ledger = read_ledger(&paths).expect("ledger");
        assert_eq!(ledger.items.len(), 2);
        assert!(ledger.last_transaction_id.is_some());
        assert!(paths.home.join(".agents/skills/acme-review").is_dir());
        assert!(paths.home.join(".agents/skills/acme-debug").is_dir());

        let ids = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
        uninstall_batch(&paths, &source, &ids, false).expect("batch uninstall");
        assert!(!paths.home.join(".agents/skills/acme-review").exists());
        assert!(!paths.home.join(".agents/skills/acme-debug").exists());
        assert!(read_ledger(&paths).expect("ledger").items.is_empty());
    }

    #[test]
    fn recovery_rolls_back_only_mutations_that_activated() {
        for activated_count in 0..=3 {
            let root = tempfile::tempdir().expect("root");
            let paths = paths(root.path());
            fs::create_dir_all(&paths.home).expect("home");
            let mutations = ["first", "second", "third"]
                .into_iter()
                .map(|name| {
                    let target = paths.home.join(format!("{name}.txt"));
                    let staging = paths.home.join(format!("{name}-stage.txt"));
                    let backup = paths.home.join(format!("{name}-backup.txt"));
                    fs::write(&target, format!("old-{name}")).expect("target");
                    fs::write(&staging, format!("new-{name}")).expect("staging");
                    JournalMutation {
                        target: target.display().to_string(),
                        staging: Some(staging.display().to_string()),
                        backup: Some(backup.display().to_string()),
                        persistent_backup: false,
                        target_existed: true,
                        original_digest: Some(ledger::bytes_digest(
                            format!("old-{name}").as_bytes(),
                        )),
                    }
                })
                .collect::<Vec<_>>();
            let journal = TransactionJournal {
                version: 1,
                transaction_id: format!("tx-interrupted-{activated_count}"),
                mutations,
            };
            write_journal(&paths, &journal).expect("journal");
            for mutation in journal.mutations.iter().take(activated_count) {
                fs::rename(
                    Path::new(&mutation.target),
                    Path::new(mutation.backup.as_ref().expect("backup")),
                )
                .expect("backup target");
                fs::rename(
                    Path::new(mutation.staging.as_ref().expect("staging")),
                    Path::new(&mutation.target),
                )
                .expect("activate target");
            }

            recover(&paths).expect("recover");
            for name in ["first", "second", "third"] {
                assert_eq!(
                    fs::read_to_string(paths.home.join(format!("{name}.txt"))).expect("target"),
                    format!("old-{name}")
                );
                assert!(!paths.home.join(format!("{name}-backup.txt")).exists());
            }
            assert!(!paths.app_data().join(JOURNAL_FILE).exists());
        }
    }

    #[test]
    fn recovery_keeps_a_transaction_whose_ledger_committed() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        fs::create_dir_all(&paths.home).expect("home");
        let target = paths.home.join("managed.txt");
        let backup = paths.home.join("managed-backup.txt");
        fs::write(&target, "new").expect("target");
        fs::write(&backup, "old").expect("backup");
        let transaction_id = "tx-committed".to_string();
        let journal = TransactionJournal {
            version: 1,
            transaction_id: transaction_id.clone(),
            mutations: vec![JournalMutation {
                target: target.display().to_string(),
                staging: None,
                backup: Some(backup.display().to_string()),
                persistent_backup: false,
                target_existed: true,
                original_digest: Some(ledger::bytes_digest(b"old")),
            }],
        };
        let ledger_state = InstallationLedger {
            last_transaction_id: Some(transaction_id),
            ..InstallationLedger::default()
        };
        ledger::write(&paths.app_data(), &ledger_state).expect("ledger");
        write_journal(&paths, &journal).expect("journal");

        recover(&paths).expect("recover");
        assert_eq!(fs::read_to_string(target).expect("target"), "new");
        assert!(!backup.exists());
    }

    #[test]
    fn disabling_one_target_retains_a_shared_skill_until_the_last_consumer() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        let source_root = root.path().join("source-v2");
        fs::create_dir_all(source_root.join("skills/review")).expect("skill");
        fs::write(
            source_root.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review code\n---\nBody\n",
        )
        .expect("skill");
        fs::write(
            source_root.join("skill-manager.json"),
            r#"{
              "version": 2,
              "source": {"id":"acme","name":"Acme","description":"Test"},
              "packages": [{
                "id":"review",
                "components":[{"kind":"skill","id":"review","path":"skills/review"}]
              }]
            }"#,
        )
        .expect("manifest");
        let catalog = read_manifest_catalog(&source_root, TEST_SOURCE_KEY).expect("catalog");
        let source = ConfiguredSource {
            source_key: TEST_SOURCE_KEY.to_string(),
            source_id: "acme".to_string(),
            name: "Acme".to_string(),
            description: "Test".to_string(),
            locator: crate::locator::Locator::display_url(
                "https://nexus.example.com/repository/raw/sources/acme-latest.zip".to_string(),
            ),
            repository_key: None,
        };
        let item = catalog.items["review"].clone();
        let snapshot = SourceSnapshot {
            definition: source.clone(),
            commit: "b".repeat(40),
            path: source_root,
            catalog,
        };
        for target in [TargetId::Cursor, TargetId::Codex, TargetId::OpenCode] {
            crate::agent_profiles::set_enabled(&paths, target, true).expect("enable");
        }
        install(&paths, &source, &snapshot, &item, false, false).expect("install");
        let skill = paths.home.join(".agents/skills/acme-review");
        let ledger = read_ledger(&paths).expect("ledger");
        assert_eq!(ledger.resources.len(), 1);
        assert_eq!(
            ledger
                .resources
                .values()
                .next()
                .expect("resource")
                .consumer_binding_ids
                .len(),
            3
        );

        disable_target(&paths, TargetId::Cursor, false).expect("disable cursor");
        assert!(skill.is_dir());
        disable_target(&paths, TargetId::Codex, false).expect("disable codex");
        assert!(skill.is_dir());
        disable_target(&paths, TargetId::OpenCode, false).expect("disable opencode");
        assert!(!skill.exists());
        assert!(read_ledger(&paths).expect("ledger").items.is_empty());
    }

    #[test]
    fn installed_package_conflicts_are_enforced_in_both_install_orders() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        let source_root = root.path().join("conflict-source");
        for name in ["old", "new"] {
            let skill_root = source_root.join("skills").join(name);
            fs::create_dir_all(&skill_root).expect("skill");
            fs::write(
                skill_root.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {name} skill\n---\nBody\n"),
            )
            .expect("skill file");
        }
        fs::write(
            source_root.join("skill-manager.json"),
            r#"{
              "version":2,
              "source":{"id":"acme","name":"Acme","description":"Test"},
              "packages":[
                {"id":"old","components":[{"kind":"skill","path":"skills/old"}],"conflictsWith":["acme/new"]},
                {"id":"new","components":[{"kind":"skill","path":"skills/new"}]}
              ]
            }"#,
        )
        .expect("manifest");
        let catalog = read_manifest_catalog(&source_root, TEST_SOURCE_KEY).expect("catalog");
        let source = ConfiguredSource {
            source_key: TEST_SOURCE_KEY.to_string(),
            source_id: "acme".to_string(),
            name: "Acme".to_string(),
            description: "Test".to_string(),
            locator: crate::locator::Locator::display_url(
                "https://nexus.example.com/repository/raw/sources/acme-latest.zip".to_string(),
            ),
            repository_key: None,
        };
        let old = catalog.items["old"].clone();
        let new = catalog.items["new"].clone();
        let snapshot = SourceSnapshot {
            definition: source.clone(),
            commit: "e".repeat(40),
            path: source_root,
            catalog,
        };
        crate::agent_profiles::set_enabled(&paths, TargetId::Codex, true).expect("enable");

        install(&paths, &source, &snapshot, &old, false, false).expect("install old");
        assert!(install(&paths, &source, &snapshot, &new, false, false)
            .expect_err("conflict")
            .contains("declares an incompatibility"));
        assert!(paths.home.join(".agents/skills/acme-old").is_dir());
        assert!(!paths.home.join(".agents/skills/acme-new").exists());
        assert_eq!(read_ledger(&paths).expect("ledger").items.len(), 1);
    }

    #[test]
    fn source_reset_wipes_foreign_source_key_and_leaves_other_sources() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        crate::agent_profiles::set_enabled(&paths, crate::agent_profiles::TargetId::Cursor, true)
            .expect("enable");
        let (source, snapshot, item) = fixture(root.path());
        install(&paths, &source, &snapshot, &item, false, false).expect("install");
        let other_root = root.path().join("other-source");
        fs::create_dir_all(other_root.join("skills/keep")).expect("other skill");
        fs::write(
            other_root.join("skills/keep/SKILL.md"),
            "---\nname: keep\ndescription: Keep this\n---\nBody\n",
        )
        .expect("other skill file");
        fs::write(
            other_root.join("skill-manager.json"),
            r#"{"version":2,"source":{"id":"other","name":"Other","description":"Keep"},"packages":[{"id":"keep","components":[{"kind":"skill","path":"skills/keep"}]}]}"#,
        )
        .expect("other manifest");
        let other_key = "source-other000000";
        let other_catalog = read_manifest_catalog(&other_root, other_key).expect("other catalog");
        let other_source = ConfiguredSource {
            source_key: other_key.to_string(),
            source_id: "other".to_string(),
            name: "Other".to_string(),
            description: "Keep".to_string(),
            locator: crate::locator::Locator::display_url(
                "https://nexus.example.com/repository/raw/sources/other-latest.zip".to_string(),
            ),
            repository_key: None,
        };
        let other_item = other_catalog.items["keep"].clone();
        let other_snapshot = SourceSnapshot {
            definition: other_source.clone(),
            commit: "f".repeat(40),
            path: other_root,
            catalog: other_catalog,
        };
        install(
            &paths,
            &other_source,
            &other_snapshot,
            &other_item,
            false,
            false,
        )
        .expect("install other");

        let mut ledger = read_ledger(&paths).expect("ledger");
        ledger.items.get_mut(&item.id).expect("record").source_key = "stale-source-key".to_string();
        crate::ledger::write(&paths.app_data(), &ledger).expect("rewrite");
        assert_eq!(
            crate::install::item_status(
                &paths,
                &read_ledger(&paths).expect("ledger"),
                Some(&item),
                &item.id
            ),
            crate::install::ItemStatus::SourceConflict
        );
        assert!(uninstall(&paths, &source, &item.id, true)
            .expect_err("foreign")
            .contains("owned by a different source"));

        let target = paths.home.join(".agents/skills/acme-review");
        fs::write(target.join("local.txt"), "edit").expect("local edit");
        let outcome = reset_source(&paths, &source, Some(&snapshot)).expect("reset");
        assert!(!outcome.backup_paths.is_empty());
        assert!(!target.exists());
        assert!(paths.home.join(".agents/skills/other-keep").is_dir());
        let ledger = read_ledger(&paths).expect("ledger");
        assert!(!ledger.items.contains_key(&item.id));
        assert!(ledger.items.contains_key(&other_item.id));
        assert_eq!(
            crate::install::item_status(&paths, &ledger, Some(&item), &item.id),
            crate::install::ItemStatus::Available
        );
    }

    #[test]
    fn source_reset_is_a_noop_when_the_source_has_no_ledger_records() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        let (source, snapshot, _) = fixture(root.path());
        let outcome = reset_source(&paths, &source, Some(&snapshot)).expect("reset");
        assert!(outcome.backup_paths.is_empty());
        assert!(read_ledger(&paths).expect("ledger").items.is_empty());
    }

    #[test]
    fn source_reset_clears_conflict_when_a_resource_cannot_be_staged() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        crate::agent_profiles::set_enabled(&paths, crate::agent_profiles::TargetId::Cursor, true)
            .expect("enable");
        let (source, snapshot, item) = fixture(root.path());
        install(&paths, &source, &snapshot, &item, false, false).expect("install");
        let mut ledger = read_ledger(&paths).expect("ledger");
        let binding_id = ledger.items[&item.id].binding_ids[0].clone();
        let poison = paths.data.join("skill-manager/poison.toml");
        let resource_id = "resource-poison00000000000000".to_string();
        ledger.resources.insert(
            resource_id.clone(),
            ResourceRecord {
                id: resource_id.clone(),
                identity: "entry://poison:key".to_string(),
                desired_digest: "a".repeat(64),
                owned: OwnedResource::StructuredEntry(OwnedStructuredEntry {
                    document_path: poison.display().to_string(),
                    format: StructuredFormat::Toml,
                    key_path: vec!["poison".to_string()],
                    value_digest: "a".repeat(64),
                    document_digest: "a".repeat(64),
                }),
                consumer_binding_ids: vec![binding_id.clone()],
                adapter_id: "cursor".to_string(),
                dialect_id: "cursor-2026-08".to_string(),
            },
        );
        ledger
            .bindings
            .get_mut(&binding_id)
            .expect("binding")
            .resource_ids
            .push(resource_id);
        ledger.items.get_mut(&item.id).expect("record").source_key = "stale-source-key".to_string();
        crate::ledger::write(&paths.app_data(), &ledger).expect("rewrite");

        reset_source(&paths, &source, Some(&snapshot)).expect("reset");
        let ledger = read_ledger(&paths).expect("ledger");
        assert!(!ledger.items.contains_key(&item.id));
        assert_eq!(
            crate::install::item_status(&paths, &ledger, Some(&item), &item.id),
            crate::install::ItemStatus::Available
        );
        assert!(!paths.home.join(".agents/skills/acme-review").exists());
    }

    #[test]
    fn source_reset_removes_leftover_namespaced_skill_directories() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        let (source, snapshot, _) = fixture(root.path());
        let leftover = paths.home.join(".agents/skills/acme-review");
        fs::create_dir_all(&leftover).expect("leftover");
        fs::write(leftover.join("SKILL.md"), "stale").expect("stale skill");
        reset_source(&paths, &source, Some(&snapshot)).expect("reset");
        assert!(!leftover.exists());
    }

    #[test]
    fn mcp_install_requires_trust_and_preserves_user_content() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        let source_root = root.path().join("source-config");
        fs::create_dir_all(source_root.join("mcp")).expect("mcp");
        fs::write(
            source_root.join("mcp/database.json"),
            r#"{
              "$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json",
              "mcpServers":{"database":{"type":"stdio","command":"node","args":["server.js"],"env":{"MODE":"safe"}}}
            }"#,
        )
        .expect("mcp config");
        fs::write(
            source_root.join("skill-manager.json"),
            r#"{
              "version":2,
              "source":{"id":"acme","name":"Acme","description":"Test"},
              "packages":[{
                "id":"tools",
                "components":[
                  {"kind":"mcpServer","id":"database","path":"mcp/database.json"}
                ]
              }]
            }"#,
        )
        .expect("manifest");
        let catalog = read_manifest_catalog(&source_root, TEST_SOURCE_KEY).expect("catalog");
        let source = ConfiguredSource {
            source_key: TEST_SOURCE_KEY.to_string(),
            source_id: "acme".to_string(),
            name: "Acme".to_string(),
            description: "Test".to_string(),
            locator: crate::locator::Locator::display_url(
                "https://nexus.example.com/repository/raw/sources/acme-latest.zip".to_string(),
            ),
            repository_key: None,
        };
        let item = catalog.items["tools"].clone();
        let snapshot = SourceSnapshot {
            definition: source.clone(),
            commit: "c".repeat(40),
            path: source_root,
            catalog,
        };
        crate::agent_profiles::set_enabled(&paths, TargetId::Codex, true).expect("enable");
        fs::create_dir_all(paths.home.join(".codex")).expect("codex");
        fs::write(
            paths.home.join(".codex/config.toml"),
            "model = \"gpt\" # keep\n",
        )
        .expect("config");

        assert!(install(&paths, &source, &snapshot, &item, false, false)
            .expect_err("approval")
            .contains("Tier 3"));
        install(&paths, &source, &snapshot, &item, false, true).expect("install");
        let config = fs::read_to_string(paths.home.join(".codex/config.toml")).expect("config");
        assert!(config.contains("model = \"gpt\" # keep"));
        assert!(config.contains("acme-database"));

        uninstall(&paths, &source, &item.id, false).expect("uninstall");
        let config = fs::read_to_string(paths.home.join(".codex/config.toml")).expect("config");
        assert!(config.contains("model = \"gpt\" # keep"));
        assert!(!config.contains("acme-database"));
    }

    fn mixed_fixture(root: &Path) -> (ConfiguredSource, SourceSnapshot, CatalogItem) {
        let source_root = root.join("mixed-source");
        fs::create_dir_all(source_root.join("skills/review")).expect("skill");
        fs::write(
            source_root.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review code\n---\nBody\n",
        )
        .expect("skill");
        fs::create_dir_all(source_root.join("mcp")).expect("mcp");
        fs::write(
            source_root.join("mcp/database.json"),
            r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json","mcpServers":{"database":{"type":"stdio","command":"node","args":["server.js"]}}}"#,
        )
        .expect("mcp");
        fs::write(
            source_root.join("skill-manager.json"),
            r#"{
              "version":2,
              "source":{"id":"acme","name":"Acme","description":"Test"},
              "packages":[{
                "id":"tools",
                "components":[
                  {"kind":"skill","id":"review","path":"skills/review"},
                  {"kind":"mcpServer","id":"database","path":"mcp/database.json"}
                ]
              }]
            }"#,
        )
        .expect("manifest");
        let catalog = read_manifest_catalog(&source_root, TEST_SOURCE_KEY).expect("catalog");
        let source = ConfiguredSource {
            source_key: TEST_SOURCE_KEY.to_string(),
            source_id: "acme".to_string(),
            name: "Acme".to_string(),
            description: "Test".to_string(),
            locator: crate::locator::Locator::display_url(
                "https://nexus.example.com/repository/raw/sources/acme-latest.zip".to_string(),
            ),
            repository_key: None,
        };
        let item = catalog.items["tools"].clone();
        let snapshot = SourceSnapshot {
            definition: source.clone(),
            commit: "e".repeat(40),
            path: source_root,
            catalog,
        };
        (source, snapshot, item)
    }

    #[test]
    fn components_can_be_installed_and_uninstalled_independently() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        crate::agent_profiles::set_enabled(&paths, TargetId::Codex, true).expect("enable");
        let (source, snapshot, item) = mixed_fixture(root.path());
        fs::create_dir_all(paths.home.join(".codex")).expect("codex");
        fs::write(paths.home.join(".codex/config.toml"), "model = \"gpt\"\n").expect("config");

        install_components(
            &paths,
            &source,
            &snapshot,
            &item,
            false,
            false,
            Some(&["review".to_string()]),
        )
        .expect("install skill");
        assert!(paths.home.join(".agents/skills/acme-review").is_dir());
        assert!(!fs::read_to_string(paths.home.join(".codex/config.toml"))
            .expect("config")
            .contains("acme-database"));
        let ledger = read_ledger(&paths).expect("ledger");
        assert_eq!(
            ledger.items[&item.id].selected_component_ids,
            vec!["review".to_string()]
        );

        install_components(
            &paths,
            &source,
            &snapshot,
            &item,
            false,
            true,
            Some(&["database".to_string()]),
        )
        .expect("install mcp");
        assert!(paths.home.join(".agents/skills/acme-review").is_dir());
        assert!(fs::read_to_string(paths.home.join(".codex/config.toml"))
            .expect("config")
            .contains("acme-database"));
        let ledger = read_ledger(&paths).expect("ledger");
        assert!(ledger.items[&item.id].selected_component_ids.is_empty());

        uninstall_components(
            &paths,
            &source,
            &item.id,
            Some(&["review".to_string()]),
            false,
        )
        .expect("uninstall skill");
        assert!(!paths.home.join(".agents/skills/acme-review").exists());
        assert!(fs::read_to_string(paths.home.join(".codex/config.toml"))
            .expect("config")
            .contains("acme-database"));
        let ledger = read_ledger(&paths).expect("ledger");
        assert_eq!(
            ledger.items[&item.id].selected_component_ids,
            vec!["database".to_string()]
        );

        uninstall_components(
            &paths,
            &source,
            &item.id,
            Some(&["database".to_string()]),
            false,
        )
        .expect("uninstall mcp");
        assert!(!ledger_has_item(&paths, &item.id));
        assert!(!fs::read_to_string(paths.home.join(".codex/config.toml"))
            .expect("config")
            .contains("acme-database"));
    }

    fn ledger_has_item(paths: &SystemPaths, id: &str) -> bool {
        read_ledger(paths).expect("ledger").items.contains_key(id)
    }
}
