//! Ledger commit after a staged journal activates.

use super::journal::{activate, cleanup_committed, rollback, write_journal};
use super::{resource_document, transaction_id, TransactionJournal, JOURNAL_FILE};
use crate::fs_retry;
use crate::ledger::{self, InstallationLedger, OwnedResource};
use crate::paths::SystemPaths;
use crate::sources::sync_directory;
use std::fs;
use std::path::Path;

pub(super) fn commit(
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

pub(super) fn persist_reset_ledger(
    paths: &SystemPaths,
    next: &mut InstallationLedger,
    label: &str,
) -> Result<(), String> {
    next.last_transaction_id = Some(transaction_id(label));
    crate::ledger::write(&paths.app_data(), next)
}

pub(super) fn update_document_digests_from_journal(
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
