//! Executable-source trust bound to canonical repository identity.

use crate::sources::{repository_url_key, sync_directory, temporary_path};
use crate::{fs_retry, install};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const TRUST_FILE: &str = "executable-trust.json";
const TRUST_VERSION: u8 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct TrustRecord {
    pub(crate) source_key: String,
    pub(crate) url: String,
    pub(crate) granted_at_epoch_seconds: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TrustFile {
    version: u8,
    records: Vec<TrustRecord>,
}

pub(crate) fn trust_path(config_base: &Path) -> PathBuf {
    config_base.join(TRUST_FILE)
}

pub(crate) fn is_trusted(config_base: &Path, source_key: &str, canonical_url: &str) -> bool {
    read_trust(config_base).is_ok_and(|records| {
        records.iter().any(|record| {
            record.source_key == source_key
                && repository_url_key(&record.url) == repository_url_key(canonical_url)
        })
    })
}

pub(crate) fn grant(
    config_base: &Path,
    source_key: &str,
    canonical_url: &str,
) -> Result<(), String> {
    let mut records = read_trust(config_base)?;
    records.retain(|record| record.source_key != source_key);
    records.push(TrustRecord {
        source_key: source_key.to_string(),
        url: canonical_url.to_string(),
        granted_at_epoch_seconds: install::current_epoch_seconds(),
    });
    records.sort_by(|left, right| left.source_key.cmp(&right.source_key));
    write_trust(config_base, &records)
}

pub(crate) fn revoke(config_base: &Path, source_key: &str) -> Result<(), String> {
    let mut records = read_trust(config_base)?;
    records.retain(|record| record.source_key != source_key);
    write_trust(config_base, &records)
}

fn read_trust(config_base: &Path) -> Result<Vec<TrustRecord>, String> {
    let path = trust_path(config_base);
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
    };
    let file = serde_json::from_slice::<TrustFile>(&contents)
        .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
    if file.version != TRUST_VERSION {
        return Err(format!(
            "{} uses an unsupported trust version.",
            path.display()
        ));
    }
    Ok(file.records)
}

fn write_trust(config_base: &Path, records: &[TrustRecord]) -> Result<(), String> {
    fs::create_dir_all(config_base)
        .map_err(|error| format!("Could not create {}: {error}", config_base.display()))?;
    let file = TrustFile {
        version: TRUST_VERSION,
        records: records.to_vec(),
    };
    let mut contents = serde_json::to_vec_pretty(&file)
        .map_err(|error| format!("Could not serialize executable trust: {error}"))?;
    contents.push(b'\n');
    let path = trust_path(config_base);
    let staging = temporary_path(config_base, "trust-writing");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .map_err(|error| format!("Could not create {}: {error}", staging.display()))?;
    output
        .write_all(&contents)
        .and_then(|()| output.sync_all())
        .map_err(|error| format!("Could not write {}: {error}", staging.display()))?;
    drop(output);
    if path.exists() {
        fs_retry::remove_file(&path)
            .map_err(|error| format!("Could not replace {}: {error}", path.display()))?;
    }
    fs_retry::rename(&staging, &path)
        .map_err(|error| format!("Could not activate {}: {error}", path.display()))?;
    sync_directory(config_base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_is_url_and_source_key_specific_and_revocable() {
        let config = tempfile::tempdir().expect("config");
        grant(config.path(), "source-one", "https://example.com/one.git").expect("grant");
        assert!(is_trusted(
            config.path(),
            "source-one",
            "https://example.com/one"
        ));
        assert!(!is_trusted(
            config.path(),
            "source-two",
            "https://example.com/one"
        ));
        assert!(!is_trusted(
            config.path(),
            "source-one",
            "https://example.com/two"
        ));
        revoke(config.path(), "source-one").expect("revoke");
        assert!(!is_trusted(
            config.path(),
            "source-one",
            "https://example.com/one"
        ));
    }
}
