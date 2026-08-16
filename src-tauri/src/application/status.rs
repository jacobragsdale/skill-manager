use crate::catalog::CatalogItem;
use crate::install::ItemStatus;
use crate::ledger::{InstallationLedger, InstallationRecord};
use crate::paths::SystemPaths;
use crate::source::SourceSnapshot;

pub(super) fn refined_item_status(
    paths: &SystemPaths,
    ledger_state: &InstallationLedger,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    full_plan: Option<&crate::resource::OperationPlan>,
) -> ItemStatus {
    let record = ledger_state.items.get(&item.id);
    let selected = record
        .map(|record| crate::planner::selected_component_ids(record, item))
        .unwrap_or_default();
    let selected_plan = if selected.is_empty() {
        None
    } else {
        crate::planner::plan(paths, snapshot, item, None, Some(&selected)).ok()
    };
    let mut status = item_status(paths, ledger_state, Some(item), &item.id);
    if status == ItemStatus::UpdateAvailable
        && selected_plan.as_ref().is_some_and(|plan| {
            crate::executor::plan_satisfied(ledger_state, plan).unwrap_or(false)
        })
    {
        status = if selected.len() < item.components.len() {
            ItemStatus::PartiallyInstalled
        } else {
            ItemStatus::Installed
        };
    }
    if status == ItemStatus::Installed
        && ((!selected.is_empty() && selected.len() < item.components.len())
            || selected_plan.as_ref().or(full_plan).is_some_and(|plan| {
                !crate::executor::plan_satisfied(ledger_state, plan).unwrap_or(false)
            }))
    {
        status = ItemStatus::PartiallyInstalled;
    }
    status
}

pub(super) fn component_status(
    paths: &SystemPaths,
    ledger_state: &InstallationLedger,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    component_id: &str,
    record: Option<&InstallationRecord>,
    package_status: ItemStatus,
) -> ItemStatus {
    match package_status {
        ItemStatus::SourceConflict => return ItemStatus::SourceConflict,
        ItemStatus::Removed => return ItemStatus::Removed,
        ItemStatus::Conflict => return ItemStatus::Conflict,
        _ => {}
    }
    let Some(record) = record else {
        return ItemStatus::Available;
    };
    let selected = crate::planner::selected_component_ids(record, item);
    if !selected.iter().any(|id| id == component_id) {
        return ItemStatus::Available;
    }
    let plan = crate::planner::plan(
        paths,
        snapshot,
        item,
        None,
        Some(&[component_id.to_string()]),
    );
    let Ok(plan) = plan else {
        return ItemStatus::Available;
    };
    let bindings_exist = record.binding_ids.iter().any(|binding_id| {
        ledger_state
            .bindings
            .get(binding_id)
            .is_some_and(|binding| binding.component_id == component_id)
    });
    if bindings_exist && !component_resources_match(paths, ledger_state, record, component_id) {
        return ItemStatus::Modified;
    }
    if !crate::executor::plan_satisfied(ledger_state, &plan).unwrap_or(false) {
        if item.digest != record.item_digest {
            return ItemStatus::UpdateAvailable;
        }
        return if bindings_exist {
            ItemStatus::PartiallyInstalled
        } else {
            ItemStatus::Available
        };
    }
    ItemStatus::Installed
}

pub(super) fn component_resources_match(
    paths: &SystemPaths,
    ledger_state: &InstallationLedger,
    record: &InstallationRecord,
    component_id: &str,
) -> bool {
    record.binding_ids.iter().all(|binding_id| {
        ledger_state.bindings.get(binding_id).is_none_or(|binding| {
            if binding.component_id != component_id {
                return true;
            }
            binding.resource_ids.iter().all(|resource_id| {
                ledger_state
                    .resources
                    .get(resource_id)
                    .is_some_and(|resource| {
                        crate::executor::resource_matches(paths, resource).unwrap_or(false)
                    })
            })
        })
    })
}

pub(crate) fn item_status(
    paths: &SystemPaths,
    ledger: &InstallationLedger,
    item: Option<&CatalogItem>,
    canonical_id: &str,
) -> ItemStatus {
    let Some(record) = ledger.items.get(canonical_id) else {
        return item.map_or(ItemStatus::Removed, |_| ItemStatus::Available);
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
mod tests {
    use super::{component_status, item_status, refined_item_status};
    use crate::catalog::read_manifest_catalog;
    use crate::install::{self, ItemStatus};
    use crate::paths::SystemPaths;
    use crate::source::{ConfiguredSource, SourceSnapshot, TEST_SOURCE_KEY};
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

    fn write_skill(root: &Path, name: &str) {
        let skill = root.join(format!("skills/{name}"));
        fs::create_dir_all(&skill).expect("skill");
        fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name}\nlicense: MIT\n---\nBody\n"),
        )
        .expect("skill");
    }

    fn two_component_snapshot(root: &Path) -> (ConfiguredSource, SourceSnapshot) {
        let source_root = root.join("source");
        write_skill(&source_root, "review");
        write_skill(&source_root, "docs");
        fs::write(
            source_root.join("skill-manager.json"),
            r#"{
              "version": 2,
              "source": { "id": "skillbook", "name": "Skillbook", "description": "Skills" },
              "packages": [{
                "id": "tools",
                "components": [
                  {"kind": "skill", "id": "review", "path": "skills/review"},
                  {"kind": "skill", "id": "docs", "path": "skills/docs"}
                ]
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
        let snapshot = SourceSnapshot {
            definition: source.clone(),
            commit: "a".repeat(40),
            path: source_root,
            catalog,
        };
        (source, snapshot)
    }

    fn enable_cursor(paths: &SystemPaths) {
        crate::agent_profiles::set_enabled(paths, crate::agent_profiles::TargetId::Cursor, true)
            .expect("enable");
    }

    #[test]
    fn status_matrix_matches_current_rules() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        enable_cursor(&paths);
        let (source, snapshot) = two_component_snapshot(root.path());
        let item = snapshot.catalog.items["tools"].clone();
        let review = &item.components[0].id;

        let empty = crate::executor::read_ledger(&paths).expect("ledger");
        assert_eq!(
            item_status(&paths, &empty, Some(&item), &item.id),
            ItemStatus::Available
        );
        assert_eq!(
            refined_item_status(&paths, &empty, &snapshot, &item, None),
            ItemStatus::Available
        );
        assert_eq!(
            component_status(
                &paths,
                &empty,
                &snapshot,
                &item,
                review,
                None,
                ItemStatus::Available
            ),
            ItemStatus::Available
        );
        assert_eq!(
            item_status(&paths, &empty, None, &item.id),
            ItemStatus::Removed
        );

        install::install_item_components_approved(
            &paths,
            &source,
            &snapshot,
            &item,
            false,
            Some(std::slice::from_ref(review)),
        )
        .expect("install review");
        let ledger = crate::executor::read_ledger(&paths).expect("ledger");
        let record = ledger.items.get(&item.id);
        assert_eq!(
            refined_item_status(&paths, &ledger, &snapshot, &item, None),
            ItemStatus::PartiallyInstalled
        );
        assert_eq!(
            component_status(
                &paths,
                &ledger,
                &snapshot,
                &item,
                review,
                record,
                ItemStatus::PartiallyInstalled
            ),
            ItemStatus::Installed
        );
        assert_eq!(
            component_status(
                &paths,
                &ledger,
                &snapshot,
                &item,
                "docs",
                record,
                ItemStatus::PartiallyInstalled
            ),
            ItemStatus::Available
        );

        install::install_item_components_approved(&paths, &source, &snapshot, &item, false, None)
            .expect("install all");
        let ledger = crate::executor::read_ledger(&paths).expect("ledger");
        assert_eq!(
            refined_item_status(&paths, &ledger, &snapshot, &item, None),
            ItemStatus::Installed
        );

        fs::write(
            paths.home.join(".agents/skills/skillbook-review/local.txt"),
            "edit",
        )
        .expect("edit");
        let ledger = crate::executor::read_ledger(&paths).expect("ledger");
        assert_eq!(
            item_status(&paths, &ledger, Some(&item), &item.id),
            ItemStatus::Modified
        );

        let mut ledger = crate::executor::read_ledger(&paths).expect("ledger");
        ledger.items.get_mut(&item.id).expect("record").source_key = "stale".to_string();
        assert_eq!(
            item_status(&paths, &ledger, Some(&item), &item.id),
            ItemStatus::SourceConflict
        );
        assert_eq!(
            component_status(
                &paths,
                &ledger,
                &snapshot,
                &item,
                review,
                ledger.items.get(&item.id),
                ItemStatus::SourceConflict
            ),
            ItemStatus::SourceConflict
        );
    }
}
