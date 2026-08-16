//! Install status, source removal plans, and thin wrappers around the executor.

use crate::catalog::CatalogItem;
use crate::ledger::InstallationLedger;
use crate::paths::SystemPaths;
use crate::source::{ConfiguredSource, SourceSnapshot};
use serde::Serialize;
use std::collections::BTreeSet;

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
    install_item_components_approved(paths, source, snapshot, item, trust_approved, None)
}

pub(crate) fn install_item_components_approved(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    trust_approved: bool,
    component_ids: Option<&[String]>,
) -> Result<OperationOutcome, String> {
    crate::executor::install_components(
        paths,
        source,
        snapshot,
        item,
        false,
        trust_approved,
        component_ids,
    )
}

#[cfg(all(test, unix))]
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
    replace_item_components_approved(paths, source, snapshot, item, trust_approved, None)
}

pub(crate) fn replace_item_components_approved(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    trust_approved: bool,
    component_ids: Option<&[String]>,
) -> Result<OperationOutcome, String> {
    crate::executor::install_components(
        paths,
        source,
        snapshot,
        item,
        true,
        trust_approved,
        component_ids,
    )
}

#[cfg(test)]
pub(crate) fn uninstall_item(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    canonical_id: &str,
    force_modified: bool,
) -> Result<OperationOutcome, String> {
    uninstall_item_components(paths, source, canonical_id, None, force_modified)
}

pub(crate) fn uninstall_item_components(
    paths: &SystemPaths,
    source: &ConfiguredSource,
    canonical_id: &str,
    component_ids: Option<&[String]>,
    force_modified: bool,
) -> Result<OperationOutcome, String> {
    crate::executor::uninstall_components(
        paths,
        source,
        canonical_id,
        component_ids,
        force_modified,
    )
}

pub(crate) fn source_removal_plan(
    paths: &SystemPaths,
    source: &ConfiguredSource,
) -> Result<SourceRemovalPlan, String> {
    let ledger_state = crate::executor::read_ledger(paths)?;
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

pub(crate) fn source_reset_ids(
    ledger: &InstallationLedger,
    source: &ConfiguredSource,
    catalog_ids: &BTreeSet<String>,
) -> Vec<String> {
    let mut ids = ledger
        .items
        .iter()
        .filter(|(id, record)| {
            record.source_key == source.source_key
                || record.source_id == source.source_id
                || catalog_ids.contains(*id)
        })
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::read_manifest_catalog;
    use crate::source::TEST_SOURCE_KEY;
    use std::fs;
    use std::path::Path;

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
        fs::write(
            source_root.join("skill-manager.json"),
            r#"{
              "version": 2,
              "source": { "id": "skillbook", "name": "Skillbook", "description": "Skills" },
              "packages": [{
                "id": "review",
                "components": [{"kind": "skill", "path": "skills/review"}]
              }]
            }"#,
        )
        .expect("manifest");
        let catalog = read_manifest_catalog(&source_root, TEST_SOURCE_KEY).expect("catalog");
        let mut source = ConfiguredSource::test_fixture(
            "skillbook",
            "https://nexus.example.com/repository/raw/sources/skillbook-latest.zip",
        );
        source.source_key = TEST_SOURCE_KEY.to_string();
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
        crate::agent_profiles::set_enabled(&paths, crate::agent_profiles::TargetId::Cursor, true)
            .expect("enable");
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
            crate::application::status::item_status(
                &paths,
                &crate::executor::read_ledger(&paths).expect("ledger"),
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
        crate::agent_profiles::set_enabled(&paths, crate::agent_profiles::TargetId::Cursor, true)
            .expect("enable");
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
        crate::agent_profiles::set_enabled(&paths, crate::agent_profiles::TargetId::Cursor, true)
            .expect("enable");
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
        crate::agent_profiles::set_enabled(&paths, crate::agent_profiles::TargetId::Cursor, true)
            .expect("enable");
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

    #[test]
    fn source_reset_ids_include_foreign_source_key_and_catalog_id() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        crate::agent_profiles::set_enabled(&paths, crate::agent_profiles::TargetId::Cursor, true)
            .expect("enable");
        let (source, snapshot, item) = snapshot(root.path());
        install_item(&paths, &source, &snapshot, &item).expect("install");
        let mut ledger = crate::executor::read_ledger(&paths).expect("ledger");
        ledger.items.get_mut(&item.id).expect("record").source_key = "stale-source-key".to_string();
        crate::ledger::write(&paths.app_data(), &ledger).expect("rewrite");
        let ledger = crate::executor::read_ledger(&paths).expect("reread");
        assert_eq!(
            crate::application::status::item_status(&paths, &ledger, Some(&item), &item.id),
            ItemStatus::SourceConflict
        );
        let catalog_ids = std::iter::once(item.id.clone()).collect();
        assert_eq!(
            source_reset_ids(&ledger, &source, &catalog_ids),
            vec![item.id.clone()]
        );
        assert!(source_reset_ids(&ledger, &source, &BTreeSet::new()).contains(&item.id));
    }
}
