//! Recovery journal, activation, and rollback.

use super::{existing_path_digest, path_entry_exists, remove_any};
use crate::fs_retry;
use crate::ledger::{self, InstallationLedger, LegacyPathRoots};
use crate::paths::SystemPaths;
use crate::sources::{sync_directory, temporary_path};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub(super) const JOURNAL_FILE: &str = "resource-transaction.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct TransactionJournal {
    pub(super) version: u8,
    pub(super) transaction_id: String,
    pub(super) mutations: Vec<JournalMutation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct JournalMutation {
    pub(super) target: String,
    pub(super) staging: Option<String>,
    pub(super) backup: Option<String>,
    pub(super) persistent_backup: bool,
    pub(super) target_existed: bool,
    pub(super) original_digest: Option<String>,
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

pub(super) fn read_ledger_raw(paths: &SystemPaths) -> Result<InstallationLedger, String> {
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

pub(super) fn write_journal(
    paths: &SystemPaths,
    journal: &TransactionJournal,
) -> Result<(), String> {
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

pub(super) fn activate(journal: &TransactionJournal) -> Result<(), String> {
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

pub(super) fn rollback(journal: &TransactionJournal) -> Result<(), String> {
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

pub(super) fn cleanup_committed(journal: &TransactionJournal) -> Result<(), String> {
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

pub(super) fn cleanup_mutations(mutations: &[JournalMutation]) {
    for mutation in mutations {
        if let Some(staging) = &mutation.staging {
            let _ = remove_any(Path::new(staging));
        }
    }
}

pub(super) fn cleanup_staging(journal: &TransactionJournal) {
    cleanup_mutations(&journal.mutations);
}
