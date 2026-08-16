//! Staging of planned path and document mutations.

use super::journal::cleanup_mutations;
use super::matching::resource_matches;
use super::{
    existing_path_digest, normalize_path, path_entry_exists, remove_any, resource_document,
    validate_absolute_owned_path, JournalMutation, TransactionJournal,
};
use crate::catalog::materialize_agent_skill;
use crate::ledger::{
    self, InstallationLedger, OwnedPath, OwnedPathKind, OwnedResource, OwnedStructuredEntry,
    OwnedTextBlock, ResourceRecord,
};
use crate::managed_documents;
use crate::paths::SystemPaths;
use crate::resource::{DesiredResource, OperationPlan, PathMaterialization, StructuredFormat};
use crate::sources::{copy_directory, temporary_path};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(super) struct DocumentWork {
    path: PathBuf,
    updated: Vec<u8>,
    persistent_backup: bool,
}

#[derive(Clone, Copy)]
pub(super) struct StageRequest<'a> {
    pub(super) paths: &'a SystemPaths,
    pub(super) transaction_id: &'a str,
    pub(super) plan: &'a OperationPlan,
    pub(super) removed: &'a [ResourceRecord],
    pub(super) remaining_ledger: &'a InstallationLedger,
    pub(super) replace_unmanaged: bool,
    pub(super) force_modified: bool,
}

pub(super) fn stage_changes(
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
            original_digest: existing_path_digest(&target),
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

pub(super) fn stage_documents(
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
                updated,
                persistent_backup: persistent,
            },
        );
    }
    Ok(output)
}

pub(super) fn stage_path(
    desired: &crate::resource::DesiredPath,
    target: &Path,
) -> Result<PathBuf, String> {
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

pub(super) fn stage_bytes(target: &Path, contents: &[u8]) -> Result<PathBuf, String> {
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

pub(super) fn mutation_backup(
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
