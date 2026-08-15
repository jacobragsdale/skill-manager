//! Versioned ownership ledger for logical installs, bindings, and physical resources.

use crate::digest::directory_digest;
use crate::fs_retry;
use crate::resource::{stable_id, CapabilityResult, StructuredFormat};
use crate::sources::{sync_directory, temporary_path};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path};

const LEDGER_FILE: &str = "installations.json";
const LEDGER_BACKUP_FILE: &str = "installations.json.previous";
const LEDGER_VERSION: u8 = 4;

pub(crate) struct LegacyPathRoots<'a> {
    pub(crate) home: &'a Path,
    pub(crate) config: &'a Path,
    pub(crate) data: &'a Path,
    pub(crate) local_data: &'a Path,
    pub(crate) cache: &'a Path,
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
    pub(crate) path: String,
    pub(crate) kind: OwnedPathKind,
    pub(crate) installed_digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct OwnedStructuredEntry {
    pub(crate) document_path: String,
    pub(crate) format: StructuredFormat,
    pub(crate) key_path: Vec<String>,
    pub(crate) value_digest: String,
    pub(crate) document_digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct OwnedTextBlock {
    pub(crate) document_path: String,
    pub(crate) marker_id: String,
    pub(crate) body_digest: String,
    pub(crate) document_digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "resourceType")]
pub(crate) enum OwnedResource {
    Path(OwnedPath),
    StructuredEntry(OwnedStructuredEntry),
    TextBlock(OwnedTextBlock),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ResourceRecord {
    pub(crate) id: String,
    pub(crate) identity: String,
    pub(crate) desired_digest: String,
    pub(crate) owned: OwnedResource,
    pub(crate) consumer_binding_ids: Vec<String>,
    pub(crate) adapter_id: String,
    pub(crate) dialect_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct BindingRecord {
    pub(crate) id: String,
    pub(crate) installation_id: String,
    pub(crate) component_id: String,
    pub(crate) target_id: String,
    pub(crate) dialect_id: String,
    pub(crate) scope: String,
    pub(crate) capability: CapabilityResult,
    pub(crate) resource_ids: Vec<String>,
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
    pub(crate) name: String,
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) disable_model_invocation: bool,
    pub(crate) source: String,
    pub(crate) destination: OwnedPath,
    #[serde(default = "manifest_v1")]
    pub(crate) manifest_version: u8,
    #[serde(default = "legacy_component_kind")]
    pub(crate) component_kind: String,
    #[serde(default)]
    pub(crate) binding_ids: Vec<String>,
    #[serde(default)]
    pub(crate) selected_component_ids: Vec<String>,
    #[serde(default)]
    pub(crate) conflicts_with: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct InstallationLedger {
    pub(crate) items: BTreeMap<String, InstallationRecord>,
    pub(crate) bindings: BTreeMap<String, BindingRecord>,
    pub(crate) resources: BTreeMap<String, ResourceRecord>,
    pub(crate) last_transaction_id: Option<String>,
}

impl InstallationLedger {
    pub(crate) fn resource_by_identity(&self, identity: &str) -> Option<&ResourceRecord> {
        self.resources
            .values()
            .find(|resource| resource.identity == identity)
    }

    pub(crate) fn resource_by_identity_mut(
        &mut self,
        identity: &str,
    ) -> Option<&mut ResourceRecord> {
        self.resources
            .values_mut()
            .find(|resource| resource.identity == identity)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LedgerFileV4 {
    version: u8,
    items: BTreeMap<String, InstallationRecord>,
    bindings: BTreeMap<String, BindingRecord>,
    resources: BTreeMap<String, ResourceRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_transaction_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LedgerFileV3 {
    version: u8,
    items: BTreeMap<String, LegacyInstallationRecord>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LegacyInstallationRecord {
    source_key: String,
    source_url: String,
    source_id: String,
    local_id: String,
    commit: String,
    item_digest: String,
    name: String,
    description: String,
    #[serde(default)]
    disable_model_invocation: bool,
    source: String,
    destination: OwnedPath,
}

pub(crate) fn read(
    data_base: &Path,
    legacy_roots: LegacyPathRoots<'_>,
) -> Result<InstallationLedger, String> {
    recover(data_base)?;
    let path = data_base.join(LEDGER_FILE);
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(InstallationLedger::default());
        }
        Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
    };
    let mut value = serde_json::from_slice::<serde_json::Value>(&contents)
        .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
    let mut version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("{} has no valid ledger version.", path.display()))?;
    if version == 2 {
        migrate_legacy_destinations(&mut value, &legacy_roots)?;
        version = 3;
    }
    let (ledger, migrated) = match version {
        3 => {
            let legacy = serde_json::from_value::<LedgerFileV3>(value)
                .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
            (migrate_v3(legacy, &legacy_roots)?, true)
        }
        4 => {
            let file = serde_json::from_value::<LedgerFileV4>(value)
                .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
            (
                InstallationLedger {
                    items: file.items,
                    bindings: file.bindings,
                    resources: file.resources,
                    last_transaction_id: file.last_transaction_id,
                },
                false,
            )
        }
        _ => {
            return Err(format!(
                "{} uses an unsupported ledger version; restore a supported backup.",
                path.display()
            ));
        }
    };
    validate(&path, &ledger)?;
    if migrated {
        let migration_backup = data_base.join("installations.v3.json");
        if !migration_backup.exists() {
            fs::copy(&path, &migration_backup).map_err(|error| {
                format!(
                    "Could not preserve the v3 ledger at {}: {error}",
                    migration_backup.display()
                )
            })?;
        }
        write(data_base, &ledger)?;
        let reread = fs::read(data_base.join(LEDGER_FILE))
            .map_err(|error| format!("Could not reread the migrated ledger: {error}"))?;
        let migrated_file = serde_json::from_slice::<LedgerFileV4>(&reread)
            .map_err(|error| format!("Could not verify the migrated ledger: {error}"))?;
        if migrated_file.version != LEDGER_VERSION {
            return Err("The migrated ledger did not retain version 4.".to_string());
        }
    }
    Ok(ledger)
}

fn migrate_v3(
    file: LedgerFileV3,
    roots: &LegacyPathRoots<'_>,
) -> Result<InstallationLedger, String> {
    if file.version != 3 {
        return Err("The legacy ledger is not version 3.".to_string());
    }
    let mut ledger = InstallationLedger::default();
    for (installation_id, legacy) in file.items {
        let proven_plugin = legacy.destination.kind == OwnedPathKind::Directory
            && Path::new(&legacy.destination.path)
                .join("plugin.json")
                .is_file();
        let binding_id = stable_id("binding", &format!("{installation_id}:legacy-v1"));
        let identity = format!(
            "path:{}",
            normalize_path(Path::new(&legacy.destination.path))
        );
        let resource_id = stable_id("resource", &identity);
        ledger.resources.insert(
            resource_id.clone(),
            ResourceRecord {
                id: resource_id.clone(),
                identity,
                desired_digest: legacy.destination.installed_digest.clone(),
                owned: OwnedResource::Path(legacy.destination.clone()),
                consumer_binding_ids: vec![binding_id.clone()],
                adapter_id: "legacy-v1".to_string(),
                dialect_id: "manifest-v1".to_string(),
            },
        );
        ledger.bindings.insert(
            binding_id.clone(),
            BindingRecord {
                id: binding_id.clone(),
                installation_id: installation_id.clone(),
                component_id: legacy.local_id.clone(),
                target_id: "legacy-v1".to_string(),
                dialect_id: "manifest-v1".to_string(),
                scope: "explicit".to_string(),
                capability: CapabilityResult::Native,
                resource_ids: vec![resource_id],
            },
        );
        let mut binding_ids = vec![binding_id];
        if proven_plugin {
            for (target_id, path, dialect_id) in [
                (
                    "cursor",
                    roots.home.join(".cursor/plugins/local").join(&legacy.name),
                    "cursor-local-plugin-2026-08",
                ),
                (
                    "github-copilot",
                    roots
                        .home
                        .join(".copilot/installed-plugins/_direct")
                        .join(&legacy.name),
                    "copilot-direct-plugin-2026-08",
                ),
            ] {
                if !path.is_dir()
                    || path_digest(&path, OwnedPathKind::Directory).ok().as_deref()
                        != Some(&legacy.destination.installed_digest)
                {
                    continue;
                }
                let binding_id = stable_id(
                    "binding",
                    &format!("{installation_id}:{target_id}:legacy-plugin"),
                );
                let identity = format!("path:{}", normalize_path(&path));
                let resource_id = stable_id("resource", &identity);
                ledger.resources.insert(
                    resource_id.clone(),
                    ResourceRecord {
                        id: resource_id.clone(),
                        identity,
                        desired_digest: legacy.destination.installed_digest.clone(),
                        owned: OwnedResource::Path(OwnedPath {
                            path: path.display().to_string(),
                            kind: OwnedPathKind::Directory,
                            installed_digest: legacy.destination.installed_digest.clone(),
                        }),
                        consumer_binding_ids: vec![binding_id.clone()],
                        adapter_id: target_id.to_string(),
                        dialect_id: dialect_id.to_string(),
                    },
                );
                ledger.bindings.insert(
                    binding_id.clone(),
                    BindingRecord {
                        id: binding_id.clone(),
                        installation_id: installation_id.clone(),
                        component_id: legacy.local_id.clone(),
                        target_id: target_id.to_string(),
                        dialect_id: dialect_id.to_string(),
                        scope: "user".to_string(),
                        capability: CapabilityResult::Native,
                        resource_ids: vec![resource_id],
                    },
                );
                binding_ids.push(binding_id);
            }
        }
        ledger.items.insert(
            installation_id,
            InstallationRecord {
                source_key: legacy.source_key,
                source_url: legacy.source_url,
                source_id: legacy.source_id,
                local_id: legacy.local_id,
                commit: legacy.commit,
                item_digest: legacy.item_digest,
                name: legacy.name,
                description: legacy.description,
                disable_model_invocation: legacy.disable_model_invocation,
                source: legacy.source,
                destination: legacy.destination,
                manifest_version: 1,
                component_kind: if proven_plugin {
                    "agentPlugin".to_string()
                } else {
                    legacy_component_kind()
                },
                binding_ids,
                selected_component_ids: Vec::new(),
                conflicts_with: Vec::new(),
            },
        );
    }
    Ok(ledger)
}

fn validate(path: &Path, ledger: &InstallationLedger) -> Result<(), String> {
    for (id, record) in &ledger.items {
        if id != &format!("{}/{}", record.source_id, record.local_id)
            || record.source_key.is_empty()
            || record.source_url.is_empty()
            || record.commit.is_empty()
            || record.name.is_empty()
            || record.description.is_empty()
            || record.source.is_empty()
            || record.destination.path.is_empty()
            || !Path::new(&record.destination.path).is_absolute()
            || !valid_digest(&record.item_digest)
            || !valid_digest(&record.destination.installed_digest)
            || record.manifest_version == 0
        {
            return Err(format!(
                "{} contains an invalid installation record for {id}.",
                path.display()
            ));
        }
        if record
            .binding_ids
            .iter()
            .any(|binding_id| !ledger.bindings.contains_key(binding_id))
        {
            return Err(format!(
                "{} has a dangling binding for {id}.",
                path.display()
            ));
        }
    }
    let mut identities = BTreeSet::new();
    for (id, resource) in &ledger.resources {
        if id != &resource.id
            || resource.identity.is_empty()
            || !valid_digest(&resource.desired_digest)
            || !identities.insert(&resource.identity)
            || resource.consumer_binding_ids.is_empty()
            || resource
                .consumer_binding_ids
                .iter()
                .any(|binding_id| !ledger.bindings.contains_key(binding_id))
        {
            return Err(format!(
                "{} contains an invalid resource {id}.",
                path.display()
            ));
        }
    }
    for (id, binding) in &ledger.bindings {
        if id != &binding.id
            || !ledger.items.contains_key(&binding.installation_id)
            || binding
                .resource_ids
                .iter()
                .any(|resource_id| !ledger.resources.contains_key(resource_id))
        {
            return Err(format!(
                "{} contains an invalid binding {id}.",
                path.display()
            ));
        }
    }
    Ok(())
}

fn migrate_legacy_destinations(
    value: &mut serde_json::Value,
    roots: &LegacyPathRoots<'_>,
) -> Result<(), String> {
    let items = value
        .get_mut("items")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| "The legacy installation ledger has no valid items object.".to_string())?;
    for (id, record) in items {
        let destination = record
            .get_mut("destination")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| {
                format!("The legacy installation record for {id} has no destination.")
            })?;
        let anchor = destination
            .remove("anchor")
            .and_then(|value| value.as_str().map(str::to_string))
            .ok_or_else(|| format!("The legacy installation record for {id} has no anchor."))?;
        let relative = destination
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("The legacy installation record for {id} has no path."))?;
        let relative = Path::new(relative);
        if relative.as_os_str().is_empty()
            || relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(format!(
                "The legacy installation record for {id} has an invalid path."
            ));
        }
        let root = match anchor.as_str() {
            "home" => roots.home,
            "config" => roots.config,
            "data" => roots.data,
            "localData" => roots.local_data,
            "cache" => roots.cache,
            _ => {
                return Err(format!(
                    "The legacy installation record for {id} has an unknown anchor."
                ));
            }
        };
        destination.insert(
            "path".to_string(),
            serde_json::Value::String(root.join(relative).display().to_string()),
        );
    }
    value["version"] = serde_json::Value::from(3);
    Ok(())
}

pub(crate) fn write(data_base: &Path, ledger: &InstallationLedger) -> Result<(), String> {
    fs::create_dir_all(data_base)
        .map_err(|error| format!("Could not create {}: {error}", data_base.display()))?;
    recover(data_base)?;
    let file = LedgerFileV4 {
        version: LEDGER_VERSION,
        items: ledger.items.clone(),
        bindings: ledger.bindings.clone(),
        resources: ledger.resources.clone(),
        last_transaction_id: ledger.last_transaction_id.clone(),
    };
    let mut contents = serde_json::to_vec_pretty(&file)
        .map_err(|error| format!("Could not serialize the installation ledger: {error}"))?;
    contents.push(b'\n');
    atomic_write(
        data_base,
        &data_base.join(LEDGER_FILE),
        &data_base.join(LEDGER_BACKUP_FILE),
        &contents,
    )
}

pub(crate) fn path_digest(path: &Path, kind: OwnedPathKind) -> Result<String, String> {
    match kind {
        OwnedPathKind::Directory => directory_digest(path),
        OwnedPathKind::File => {
            let bytes = fs::read(path)
                .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
            Ok(hex_digest(Sha256::digest(bytes)))
        }
    }
}

pub(crate) fn bytes_digest(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn atomic_write(
    directory: &Path,
    path: &Path,
    backup: &Path,
    contents: &[u8],
) -> Result<(), String> {
    let staging = temporary_path(directory, "installations-writing");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .map_err(|error| format!("Could not create {}: {error}", staging.display()))?;
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("Could not write {}: {error}", staging.display()))?;
    if path.exists() {
        if backup.exists() {
            fs_retry::remove_file(backup)
                .map_err(|error| format!("Could not remove {}: {error}", backup.display()))?;
        }
        fs_retry::rename(path, backup)
            .map_err(|error| format!("Could not stage {}: {error}", path.display()))?;
        sync_directory(directory)?;
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
        sync_directory(directory)?;
        fs_retry::remove_file(backup).map_err(|error| {
            format!(
                "The ledger updated, but {} could not be removed: {error}",
                backup.display()
            )
        })?;
    } else {
        fs_retry::rename(&staging, path)
            .map_err(|error| format!("Could not activate {}: {error}", path.display()))?;
    }
    sync_directory(directory)
}

fn recover(data_base: &Path) -> Result<(), String> {
    let path = data_base.join(LEDGER_FILE);
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

fn manifest_v1() -> u8 {
    1
}

fn legacy_component_kind() -> String {
    "legacyFileTree".to_string()
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
        .to_lowercase()
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let mut output = String::with_capacity(64);
    for byte in digest.as_ref() {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_plugin(path: &Path, divergent: bool) {
        fs::create_dir_all(path).expect("plugin directory");
        fs::write(
            path.join("plugin.json"),
            r#"{
              "$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
              "name":"acme-tools",
              "version":"1.0.0",
              "description":"Acme tools"
            }"#,
        )
        .expect("plugin manifest");
        if divergent {
            fs::write(path.join("local-change.txt"), "changed").expect("local change");
        }
    }

    fn record(path: &Path) -> InstallationRecord {
        let binding_id = stable_id("binding", "acme/review:legacy-v1");
        InstallationRecord {
            source_key: "source-key".to_string(),
            source_url: "https://example.com/source".to_string(),
            source_id: "acme".to_string(),
            local_id: "review".to_string(),
            commit: "a".repeat(40),
            item_digest: "b".repeat(64),
            name: "acme-review".to_string(),
            description: "Review code.".to_string(),
            disable_model_invocation: false,
            source: "skills/review".to_string(),
            destination: OwnedPath {
                path: path.display().to_string(),
                kind: OwnedPathKind::Directory,
                installed_digest: "c".repeat(64),
            },
            manifest_version: 1,
            component_kind: "legacyFileTree".to_string(),
            binding_ids: vec![binding_id],
            selected_component_ids: Vec::new(),
            conflicts_with: Vec::new(),
        }
    }

    #[test]
    fn ledger_round_trips_atomically() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut ledger = InstallationLedger::default();
        let installation_id = "acme/review".to_string();
        let record = record(&root.path().join("acme-review"));
        let binding_id = record.binding_ids[0].clone();
        let identity = format!(
            "path:{}",
            normalize_path(Path::new(&record.destination.path))
        );
        let resource_id = stable_id("resource", &identity);
        ledger.items.insert(installation_id.clone(), record.clone());
        ledger.bindings.insert(
            binding_id.clone(),
            BindingRecord {
                id: binding_id.clone(),
                installation_id,
                component_id: "review".to_string(),
                target_id: "legacy-v1".to_string(),
                dialect_id: "manifest-v1".to_string(),
                scope: "explicit".to_string(),
                capability: CapabilityResult::Native,
                resource_ids: vec![resource_id.clone()],
            },
        );
        ledger.resources.insert(
            resource_id.clone(),
            ResourceRecord {
                id: resource_id,
                identity,
                desired_digest: record.destination.installed_digest.clone(),
                owned: OwnedResource::Path(record.destination),
                consumer_binding_ids: vec![binding_id],
                adapter_id: "legacy-v1".to_string(),
                dialect_id: "manifest-v1".to_string(),
            },
        );
        write(root.path(), &ledger).expect("write");
        let roots = LegacyPathRoots {
            home: root.path(),
            config: root.path(),
            data: root.path(),
            local_data: root.path(),
            cache: root.path(),
        };
        let reloaded = read(root.path(), roots).expect("read");
        assert_eq!(reloaded.items, ledger.items);
        assert_eq!(reloaded.bindings, ledger.bindings);
        assert_eq!(reloaded.resources, ledger.resources);
    }

    #[test]
    fn v3_migration_is_repeatable_and_adopts_only_proven_identical_plugin_copies() {
        let root = tempfile::tempdir().expect("tempdir");
        let data_base = root.path().join("data");
        let home = root.path().join("home");
        let primary = home.join(".agents/plugins/acme-tools");
        let cursor = home.join(".cursor/plugins/local/acme-tools");
        let copilot = home.join(".copilot/installed-plugins/_direct/acme-tools");
        write_plugin(&primary, false);
        write_plugin(&cursor, false);
        write_plugin(&copilot, true);
        let digest = directory_digest(&primary).expect("digest");
        fs::create_dir_all(&data_base).expect("data");
        let original = serde_json::to_vec_pretty(&serde_json::json!({
            "version": 3,
            "items": {
                "acme/tools": {
                    "sourceKey": "source-key",
                    "sourceUrl": "https://example.com/acme.git",
                    "sourceId": "acme",
                    "localId": "tools",
                    "commit": "a".repeat(40),
                    "itemDigest": "b".repeat(64),
                    "name": "acme-tools",
                    "description": "Acme tools.",
                    "disableModelInvocation": false,
                    "source": "plugins/tools",
                    "destination": {
                        "path": primary.display().to_string(),
                        "kind": "directory",
                        "installedDigest": digest
                    }
                }
            }
        }))
        .expect("legacy ledger");
        fs::write(data_base.join(LEDGER_FILE), &original).expect("write legacy ledger");
        let roots = LegacyPathRoots {
            home: &home,
            config: root.path(),
            data: root.path(),
            local_data: root.path(),
            cache: root.path(),
        };

        let migrated = read(&data_base, roots).expect("migrate");
        assert_eq!(migrated.items.len(), 1);
        assert_eq!(migrated.bindings.len(), 2);
        assert_eq!(migrated.resources.len(), 2);
        assert!(migrated
            .resources
            .values()
            .any(|resource| resource.identity.contains(".cursor/plugins/local")));
        assert!(!migrated
            .resources
            .values()
            .any(|resource| resource.identity.contains(".copilot/installed-plugins")));
        assert_eq!(
            fs::read(data_base.join("installations.v3.json")).expect("migration backup"),
            original
        );
        let live = fs::read(data_base.join(LEDGER_FILE)).expect("v4 ledger");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&live).expect("v4 JSON")["version"],
            4
        );

        let roots = LegacyPathRoots {
            home: &home,
            config: root.path(),
            data: root.path(),
            local_data: root.path(),
            cache: root.path(),
        };
        let repeated = read(&data_base, roots).expect("repeat read");
        assert_eq!(repeated.items, migrated.items);
        assert_eq!(repeated.bindings, migrated.bindings);
        assert_eq!(repeated.resources, migrated.resources);

        fs::rename(
            data_base.join(LEDGER_FILE),
            data_base.join(LEDGER_BACKUP_FILE),
        )
        .expect("simulate interrupted activation");
        let roots = LegacyPathRoots {
            home: &home,
            config: root.path(),
            data: root.path(),
            local_data: root.path(),
            cache: root.path(),
        };
        let recovered = read(&data_base, roots).expect("recover backup");
        assert_eq!(recovered.items, migrated.items);
    }
}
