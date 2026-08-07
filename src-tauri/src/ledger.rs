//! Atomic ownership ledger for manifest-driven item installations.

use crate::digest::directory_digest;
use crate::fs_retry;
use crate::manifest::DestinationAnchor;
use crate::sources::{sync_directory, temporary_path};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const LEDGER_FILE: &str = "installations.json";
const LEDGER_BACKUP_FILE: &str = "installations.json.previous";
const LEDGER_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LifecyclePhase {
    Complete,
    PostInstallIncomplete,
    PostUpdateIncomplete,
    PostUninstallIncomplete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum OwnedPathKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct OwnedPath {
    pub(crate) anchor: DestinationAnchor,
    pub(crate) path: String,
    pub(crate) kind: OwnedPathKind,
    pub(crate) installed_digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct InstallationRecord {
    pub(crate) source_key: String,
    pub(crate) source_url: String,
    pub(crate) source_id: String,
    pub(crate) local_id: String,
    pub(crate) commit: String,
    pub(crate) item_digest: String,
    pub(crate) materialized_skill_name: Option<String>,
    pub(crate) destination_roots: Vec<OwnedPath>,
    pub(crate) lifecycle_phase: LifecyclePhase,
    pub(crate) retained_snapshot: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct InstallationLedger {
    pub(crate) items: BTreeMap<String, InstallationRecord>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LedgerFile {
    version: u8,
    items: BTreeMap<String, InstallationRecord>,
}

pub(crate) fn ledger_path(data_base: &Path) -> PathBuf {
    data_base.join(LEDGER_FILE)
}

pub(crate) fn read(data_base: &Path) -> Result<InstallationLedger, String> {
    recover(data_base)?;
    let path = ledger_path(data_base);
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(InstallationLedger::default());
        }
        Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
    };
    let ledger = serde_json::from_slice::<LedgerFile>(&contents)
        .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
    if ledger.version != LEDGER_VERSION {
        return Err(format!(
            "{} uses an unsupported ledger version.",
            path.display()
        ));
    }
    for (id, record) in &ledger.items {
        if id != &format!("{}/{}", record.source_id, record.local_id)
            || record.source_key.is_empty()
            || record.source_url.is_empty()
            || record.commit.is_empty()
            || !valid_digest(&record.item_digest)
            || record.destination_roots.iter().any(|owned| {
                owned.path.is_empty()
                    || Path::new(&owned.path).is_absolute()
                    || !valid_digest(&owned.installed_digest)
            })
        {
            return Err(format!(
                "{} contains an invalid installation record for {id}.",
                path.display()
            ));
        }
    }
    Ok(InstallationLedger {
        items: ledger.items,
    })
}

pub(crate) fn write(data_base: &Path, ledger: &InstallationLedger) -> Result<(), String> {
    fs::create_dir_all(data_base)
        .map_err(|error| format!("Could not create {}: {error}", data_base.display()))?;
    recover(data_base)?;
    let file = LedgerFile {
        version: LEDGER_VERSION,
        items: ledger.items.clone(),
    };
    let mut contents = serde_json::to_vec_pretty(&file)
        .map_err(|error| format!("Could not serialize the installation ledger: {error}"))?;
    contents.push(b'\n');
    let path = ledger_path(data_base);
    let backup = data_base.join(LEDGER_BACKUP_FILE);
    let staging = temporary_path(data_base, "installations-writing");
    write_new_file(&staging, &contents)?;

    if path.exists() {
        if backup.exists() {
            fs_retry::remove_file(&backup)
                .map_err(|error| format!("Could not remove {}: {error}", backup.display()))?;
        }
        fs_retry::rename(&path, &backup)
            .map_err(|error| format!("Could not stage {}: {error}", path.display()))?;
        sync_directory(data_base)?;
        if let Err(error) = fs_retry::rename(&staging, &path) {
            let restore = fs_retry::rename(&backup, &path);
            return match restore {
                Ok(()) => Err(format!("Could not activate {}: {error}", path.display())),
                Err(restore_error) => Err(format!(
                    "Could not activate {} ({error}) or restore it ({restore_error}).",
                    path.display()
                )),
            };
        }
        sync_directory(data_base)?;
        fs_retry::remove_file(&backup).map_err(|error| {
            format!(
                "The ledger updated, but {} could not be removed: {error}",
                backup.display()
            )
        })?;
    } else {
        fs_retry::rename(&staging, &path)
            .map_err(|error| format!("Could not activate {}: {error}", path.display()))?;
    }
    sync_directory(data_base)
}

pub(crate) fn path_digest(path: &Path, kind: OwnedPathKind) -> Result<String, String> {
    match kind {
        OwnedPathKind::Directory => directory_digest(path),
        OwnedPathKind::File => {
            let bytes = fs::read(path)
                .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
            let digest = Sha256::digest(bytes);
            let mut output = String::with_capacity(64);
            for byte in digest {
                use std::fmt::Write as _;
                write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
            }
            Ok(output)
        }
    }
}

pub(crate) fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn recover(data_base: &Path) -> Result<(), String> {
    let path = ledger_path(data_base);
    if path.exists() {
        return Ok(());
    }
    let backup = data_base.join(LEDGER_BACKUP_FILE);
    match fs_retry::rename(&backup, &path) {
        Ok(()) => sync_directory(data_base),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not recover {}: {error}", path.display())),
    }
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

    fn record() -> InstallationRecord {
        InstallationRecord {
            source_key: "source-key".to_string(),
            source_url: "https://example.com/source".to_string(),
            source_id: "acme".to_string(),
            local_id: "review".to_string(),
            commit: "a".repeat(40),
            item_digest: "b".repeat(64),
            materialized_skill_name: Some("acme-review".to_string()),
            destination_roots: vec![OwnedPath {
                anchor: DestinationAnchor::Home,
                path: ".agents/skills/acme-review".to_string(),
                kind: OwnedPathKind::Directory,
                installed_digest: "c".repeat(64),
            }],
            lifecycle_phase: LifecyclePhase::Complete,
            retained_snapshot: "/cache/revision".to_string(),
        }
    }

    #[test]
    fn ledger_round_trips_canonical_item_records() {
        let data = tempfile::tempdir().expect("data");
        let mut ledger = InstallationLedger::default();
        ledger.items.insert("acme/review".to_string(), record());
        write(data.path(), &ledger).expect("write ledger");
        assert_eq!(read(data.path()).expect("read ledger").items, ledger.items);
    }

    #[test]
    fn file_and_directory_digests_are_distinct_and_stable() {
        let root = tempfile::tempdir().expect("root");
        fs::write(root.path().join("file"), "content").expect("file");
        let file_digest =
            path_digest(&root.path().join("file"), OwnedPathKind::File).expect("file digest");
        let directory_digest =
            path_digest(root.path(), OwnedPathKind::Directory).expect("directory digest");
        assert!(valid_digest(&file_digest));
        assert!(valid_digest(&directory_digest));
        assert_ne!(file_digest, directory_digest);
    }
}
