//! Compatibility facade and system paths for the resource planner/executor.

use crate::catalog_v1::{CatalogItem, ResolvedDestination};
use crate::ledger::{InstallationLedger, OwnedPath};
use crate::source_v1::{ConfiguredSource, SourceSnapshot};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct SystemPaths {
    pub(crate) home: PathBuf,
    pub(crate) config: PathBuf,
    pub(crate) data: PathBuf,
    pub(crate) local_data: PathBuf,
    pub(crate) cache: PathBuf,
}

impl SystemPaths {
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

    pub(crate) fn resolve(&self, destination: &ResolvedDestination) -> Result<PathBuf, String> {
        self.validate_destination(&destination.path)
    }

    pub(crate) fn resolve_owned(&self, owned: &OwnedPath) -> Result<PathBuf, String> {
        self.validate_destination(Path::new(&owned.path))
    }

    pub(crate) fn read_ledger(&self) -> Result<InstallationLedger, String> {
        crate::executor::read_ledger(self)
    }

    pub(crate) fn validate_destination(&self, path: &Path) -> Result<PathBuf, String> {
        if path.as_os_str().is_empty() || !path.is_absolute() || path.file_name().is_none() {
            return Err("Owned destinations must be non-root absolute paths.".to_string());
        }
        if path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        }) {
            return Err("Owned destinations may not contain . or .. components.".to_string());
        }
        let state_roots = [
            self.config.join("skill-manager"),
            self.data.join("skill-manager"),
            self.local_data.join("skill-manager"),
            self.cache.join("skill-manager"),
        ];
        if state_roots
            .iter()
            .any(|state_root| path == state_root || path.starts_with(state_root))
        {
            return Err(format!(
                "Destination {} is inside Skill Manager's own state.",
                path.display()
            ));
        }
        Ok(path.to_path_buf())
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
    PartiallyInstalled,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationOutcome {
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
    pub(crate) items: Vec<RemovalItemPlan>,
}

pub(crate) fn item_status(
    paths: &SystemPaths,
    ledger: &InstallationLedger,
    item: Option<&CatalogItem>,
    canonical_id: &str,
) -> ItemStatus {
    let Some(record) = ledger.items.get(canonical_id) else {
        return item.map_or(ItemStatus::Removed, |item| {
            if item.manifest_version == 1
                && paths
                    .resolve(&item.destination)
                    .is_ok_and(|path| path_entry_exists(&path))
            {
                ItemStatus::Conflict
            } else {
                ItemStatus::Available
            }
        });
    };
    if item.is_some_and(|item| item.source_key != record.source_key) {
        return ItemStatus::SourceConflict;
    }
    if !crate::executor::installation_matches(paths, ledger, canonical_id) {
        return ItemStatus::Modified;
    }
    match item {
        None => ItemStatus::Removed,
        Some(item) if item.digest != record.item_digest => ItemStatus::UpdateAvailable,
        Some(_) => ItemStatus::Installed,
    }
}

#[cfg(test)]
pub(crate) fn install_item(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
) -> Result<OperationOutcome, String> {
    crate::executor::install(paths, source, snapshot, item, false, false)
}

pub(crate) fn install_item_approved(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    trust_approved: bool,
) -> Result<OperationOutcome, String> {
    crate::executor::install(paths, source, snapshot, item, false, trust_approved)
}

#[cfg(test)]
pub(crate) fn replace_item(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
) -> Result<OperationOutcome, String> {
    crate::executor::install(paths, source, snapshot, item, true, false)
}

pub(crate) fn replace_item_approved(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    trust_approved: bool,
) -> Result<OperationOutcome, String> {
    crate::executor::install(paths, source, snapshot, item, true, trust_approved)
}

pub(crate) fn uninstall_item(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    canonical_id: &str,
    force_modified: bool,
) -> Result<OperationOutcome, String> {
    crate::executor::uninstall(paths, source, canonical_id, force_modified)
}

pub(crate) fn source_removal_plan(
    paths: &SystemPaths,
    source: &ConfiguredSource,
) -> Result<SourceRemovalPlan, String> {
    let ledger_state = paths.read_ledger()?;
    let mut items = ledger_state
        .items
        .iter()
        .filter(|(_, record)| record.source_key == source.source_key)
        .map(|(id, record)| {
            let mut warnings = Vec::new();
            let binding_ids = record
                .binding_ids
                .iter()
                .collect::<std::collections::BTreeSet<_>>();
            let mut seen = std::collections::BTreeSet::new();
            for binding_id in &record.binding_ids {
                let Some(binding) = ledger_state.bindings.get(binding_id) else {
                    continue;
                };
                for resource_id in &binding.resource_ids {
                    let Some(resource) = ledger_state.resources.get(resource_id) else {
                        continue;
                    };
                    if resource
                        .consumer_binding_ids
                        .iter()
                        .any(|consumer| !binding_ids.contains(consumer))
                    {
                        continue;
                    }
                    let path = match &resource.owned {
                        crate::ledger::OwnedResource::Path(owned) => owned.path.clone(),
                        crate::ledger::OwnedResource::StructuredEntry(owned) => {
                            owned.document_path.clone()
                        }
                        crate::ledger::OwnedResource::TextBlock(owned) => {
                            owned.document_path.clone()
                        }
                    };
                    if seen.insert(path.clone()) {
                        warnings.push(RemovalPathWarning {
                            path,
                            modified: !crate::executor::resource_matches(paths, resource)?,
                        });
                    }
                }
            }
            Ok(RemovalItemPlan {
                id: id.clone(),
                paths: warnings,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    items.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(SourceRemovalPlan {
        source_id: source.source_id.clone(),
        items,
    })
}

fn path_entry_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_v1::read_manifest_catalog;
    use crate::source_v1::BUILT_IN_SOURCE_KEY;

    fn paths(root: &Path) -> SystemPaths {
        SystemPaths {
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
        fs::create_dir(skill.join("scripts")).expect("scripts");
        let script = skill.join("scripts/check.sh");
        fs::write(&script, "#!/bin/sh\nexit 0\n").expect("script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
                .expect("executable script");
        }
        let destination = serde_json::to_string(
            &root
                .join("home/.agents/skills/skillbook-review")
                .display()
                .to_string(),
        )
        .expect("destination JSON");
        fs::write(
            source_root.join("skill-manager.json"),
            format!(
                r#"{{
              "version": 1,
              "source": {{ "id": "skillbook", "name": "Skillbook", "description": "Skills" }},
              "installs": [{{
                "id": "review",
                "source": "skills/review",
                "destination": {destination}
              }}]
            }}"#
            ),
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

    #[test]
    fn install_materializes_namespaced_skill_and_uninstall_removes_it() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        let (source, snapshot, item) = snapshot(root.path());
        install_item(&paths, &source, &snapshot, &item).expect("install");
        let target = paths.home.join(".agents/skills/skillbook-review");
        assert!(fs::read_to_string(target.join("SKILL.md"))
            .expect("skill")
            .contains("name: skillbook-review"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_ne!(
                fs::metadata(target.join("scripts/check.sh"))
                    .expect("script metadata")
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
        assert_eq!(
            item_status(
                &paths,
                &paths.read_ledger().expect("ledger"),
                Some(&item),
                &item.id
            ),
            ItemStatus::Installed
        );
        uninstall_item(&paths, &source, &item.id, false).expect("uninstall");
        assert!(!target.exists());
    }

    #[test]
    fn modified_owned_path_is_protected() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        let (source, snapshot, item) = snapshot(root.path());
        install_item(&paths, &source, &snapshot, &item).expect("install");
        let target = paths.home.join(".agents/skills/skillbook-review");
        fs::write(target.join("local.txt"), "edit").expect("local edit");
        assert!(uninstall_item(&paths, &source, &item.id, false)
            .expect_err("protected")
            .contains("local changes"));
        assert!(target.join("local.txt").exists());
    }

    #[test]
    #[cfg(unix)]
    fn unmanaged_replacement_keeps_backup_and_does_not_follow_symlink() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        let (source, snapshot, item) = snapshot(root.path());
        let target = paths.home.join(".agents/skills/skillbook-review");
        let external = root.path().join("external");
        fs::create_dir_all(target.parent().expect("parent")).expect("parent");
        fs::create_dir_all(&external).expect("external");
        fs::write(external.join("keep.txt"), "untouched").expect("external file");
        std::os::unix::fs::symlink(&external, &target).expect("symlink");
        let outcome = replace_item(&paths, &source, &snapshot, &item).expect("replace");
        assert_eq!(outcome.backup_paths.len(), 1);
        assert!(Path::new(&outcome.backup_paths[0]).is_symlink());
        assert_eq!(
            fs::read_to_string(external.join("keep.txt")).expect("external file"),
            "untouched"
        );
        assert!(target.is_dir());
    }

    #[test]
    fn source_removal_plan_reports_modified_destination() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        let (source, snapshot, item) = snapshot(root.path());
        install_item(&paths, &source, &snapshot, &item).expect("install");
        fs::write(
            paths.home.join(".agents/skills/skillbook-review/local.txt"),
            "edit",
        )
        .expect("edit");
        let plan = source_removal_plan(&paths, &source).expect("plan");
        assert!(plan.items[0].paths[0].modified);
    }

}
