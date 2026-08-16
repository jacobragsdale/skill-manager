use super::RuntimeState;
use crate::app_state::{BulkAction, BulkFailure, BulkPlan, BulkPlanEntry, BulkResult};
use crate::catalog::CatalogItem;
use crate::install::{self, ItemStatus, OperationOutcome, SourceRemovalPlan};
use crate::paths::SystemPaths;
use crate::source::{self, ConfiguredSource, SourceSnapshot};
use crate::sources::{cache_base_dir, config_base_dir};
use std::io;

pub(crate) async fn install_item(
    runtime: &RuntimeState,
    source_id: &str,
    local_id: &str,
    trust_approved: bool,
    component_id: Option<&str>,
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let (paths, source, snapshot, item) = item_context(source_id, local_id)?;
    let ids = requested_component_ids(&item, component_id)?;
    match ids.as_deref() {
        None => install::install_item_approved(&paths, &source, &snapshot, &item, trust_approved),
        Some(ids) => install::install_item_components_approved(
            &paths,
            &source,
            &snapshot,
            &item,
            trust_approved,
            Some(ids),
        ),
    }
}

pub(crate) async fn replace_item(
    runtime: &RuntimeState,
    source_id: &str,
    local_id: &str,
    trust_approved: bool,
    component_id: Option<&str>,
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let (paths, source, snapshot, item) = item_context(source_id, local_id)?;
    let ids = requested_component_ids(&item, component_id)?;
    match ids.as_deref() {
        None => install::replace_item_approved(&paths, &source, &snapshot, &item, trust_approved),
        Some(ids) => install::replace_item_components_approved(
            &paths,
            &source,
            &snapshot,
            &item,
            trust_approved,
            Some(ids),
        ),
    }
}

pub(crate) async fn preview_install(
    runtime: &RuntimeState,
    source_id: &str,
    local_id: &str,
    component_id: Option<&str>,
) -> Result<crate::planner::InstallPreview, String> {
    let _guard = runtime.operation_lock.lock().await;
    let (paths, _source, snapshot, item) = item_context(source_id, local_id)?;
    let ids = requested_component_ids(&item, component_id)?;
    let plan = crate::planner::plan(&paths, &snapshot, &item, None, ids.as_deref())?;
    Ok(crate::planner::preview(&item, &plan))
}

pub(super) fn requested_component_ids(
    item: &CatalogItem,
    component_id: Option<&str>,
) -> Result<Option<Vec<String>>, String> {
    match component_id {
        None => Ok(None),
        Some(component_id) => {
            crate::planner::validate_component_id(item, component_id)?;
            Ok(Some(vec![component_id.to_string()]))
        }
    }
}

pub(crate) async fn uninstall_item(
    runtime: &RuntimeState,
    source_id: &str,
    local_id: &str,
    component_id: Option<&str>,
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let paths = SystemPaths::from_system()?;
    let config = config_base_dir()?;
    let source = source::configured_source(&config, source_id)?;
    let ids = component_id.map(|component_id| vec![component_id.to_string()]);
    install::uninstall_item_components(
        &paths,
        &source,
        &format!("{source_id}/{local_id}"),
        ids.as_deref(),
        false,
    )
}

pub(crate) async fn bulk_plan(
    runtime: &RuntimeState,
    source_id: &str,
    action: BulkAction,
) -> Result<BulkPlan, String> {
    let _guard = runtime.operation_lock.lock().await;
    let paths = SystemPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let source = source::configured_source(&config, source_id)?;
    let snapshot = source::load_current(&cache, &source)?
        .ok_or_else(|| format!("{} has no validated revision.", source.source_id))?;
    let ledger_state = crate::executor::read_ledger(&paths)?;
    let entries = snapshot
        .catalog
        .items
        .values()
        .map(|item| {
            let status =
                super::status::refined_item_status(&paths, &ledger_state, &snapshot, item, None);
            BulkPlanEntry {
                id: item.id.clone(),
                local_id: item.local_id.clone(),
                status,
                will_run: match action {
                    BulkAction::Install => {
                        matches!(
                            status,
                            ItemStatus::Available
                                | ItemStatus::UpdateAvailable
                                | ItemStatus::PartiallyInstalled
                        )
                    }
                    BulkAction::Replace => status == ItemStatus::Conflict,
                    BulkAction::Uninstall => {
                        matches!(
                            status,
                            ItemStatus::Installed
                                | ItemStatus::UpdateAvailable
                                | ItemStatus::PartiallyInstalled
                        )
                    }
                },
            }
        })
        .collect();
    Ok(BulkPlan {
        source_id: source.source_id,
        action,
        entries,
    })
}

pub(crate) async fn bulk_run(
    runtime: &RuntimeState,
    source_id: &str,
    action: BulkAction,
    trust_approved: bool,
) -> Result<BulkResult, String> {
    let plan = bulk_plan(runtime, source_id, action).await?;
    let entries = plan
        .entries
        .into_iter()
        .filter(|entry| entry.will_run)
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Ok(BulkResult {
            completed: Vec::new(),
            failures: Vec::new(),
            backup_paths: Vec::new(),
        });
    }
    let _guard = runtime.operation_lock.lock().await;
    let paths = SystemPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let source = source::configured_source(&config, source_id)?;
    let snapshot = source::load_current(&cache, &source)?
        .ok_or_else(|| format!("{} has no validated revision.", source.source_id))?;
    let result = match action {
        BulkAction::Install | BulkAction::Replace => {
            let requests = entries
                .iter()
                .map(|entry| {
                    snapshot
                        .catalog
                        .items
                        .get(&entry.local_id)
                        .map(|item| crate::executor::BatchInstall {
                            source: &source,
                            snapshot: &snapshot,
                            item,
                            replace_unmanaged: action == BulkAction::Replace,
                        })
                        .ok_or_else(|| format!("Unknown catalog item: {}", entry.id))
                })
                .collect::<Result<Vec<_>, String>>()?;
            crate::executor::install_batch(&paths, &requests, trust_approved)
        }
        BulkAction::Uninstall => crate::executor::uninstall_batch(
            &paths,
            &source,
            &entries
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>(),
            false,
        ),
    };
    match result {
        Ok(outcome) => Ok(BulkResult {
            completed: entries.into_iter().map(|entry| entry.id).collect(),
            failures: Vec::new(),
            backup_paths: outcome.backup_paths,
        }),
        Err(message) => Ok(BulkResult {
            completed: Vec::new(),
            failures: entries
                .into_iter()
                .map(|entry| BulkFailure {
                    id: entry.id,
                    message: format!("Batch transaction rolled back: {message}"),
                })
                .collect(),
            backup_paths: Vec::new(),
        }),
    }
}

pub(crate) async fn plan_source_removal(
    runtime: &RuntimeState,
    source_id: &str,
) -> Result<SourceRemovalPlan, String> {
    let _guard = runtime.operation_lock.lock().await;
    let paths = SystemPaths::from_system()?;
    let config = config_base_dir()?;
    let source = source::configured_source(&config, source_id)?;
    install::source_removal_plan(&paths, &source)
}

pub(crate) async fn remove_source(
    runtime: &RuntimeState,
    source_id: &str,
    acknowledge_modified_paths: bool,
) -> Result<BulkResult, String> {
    let _guard = runtime.operation_lock.lock().await;
    let paths = SystemPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let source = source::configured_source(&config, source_id)?;
    let plan = install::source_removal_plan(&paths, &source)?;
    if plan
        .items
        .iter()
        .flat_map(|item| &item.paths)
        .any(|path| path.modified)
        && !acknowledge_modified_paths
    {
        return Err(
            "Source cleanup includes locally modified paths. Confirm the warning before continuing."
                .to_string(),
        );
    }
    let records = crate::executor::read_ledger(&paths)?
        .items
        .values()
        .filter(|record| record.source_key == source.source_key)
        .map(|record| format!("{}/{}", record.source_id, record.local_id))
        .collect::<Vec<_>>();
    let outcome = match crate::executor::uninstall_batch(
        &paths,
        &source,
        &records,
        acknowledge_modified_paths,
    ) {
        Ok(outcome) => outcome,
        Err(message) => {
            let failures = records
                .into_iter()
                .map(|id| BulkFailure {
                    id,
                    message: format!("Source removal transaction rolled back: {message}"),
                })
                .collect();
            return Ok(BulkResult {
                completed: Vec::new(),
                failures,
                backup_paths: Vec::new(),
            });
        }
    };
    if !records.is_empty()
        && !crate::executor::read_ledger(&paths)?
            .items
            .values()
            .all(|record| record.source_key != source.source_key)
    {
        return Ok(BulkResult {
            completed: Vec::new(),
            failures: vec![BulkFailure {
                id: source.source_id.clone(),
                message: "Source resources were removed, but ledger cleanup was incomplete."
                    .to_string(),
            }],
            backup_paths: Vec::new(),
        });
    }
    let mut config_file = source::read_sources_config(&config)?;
    config_file
        .sources
        .retain(|configured| configured.source_key != source.source_key);
    source::write_sources_config(&config, &config_file)?;
    source::remove_source_cache(&cache, &source.source_key)?;
    Ok(BulkResult {
        completed: records,
        failures: Vec::new(),
        backup_paths: outcome.backup_paths,
    })
}

pub(crate) async fn reset_app(runtime: &RuntimeState) -> Result<BulkResult, String> {
    let _sync_guard = runtime.sync_lock.lock().await;
    let _guard = runtime.operation_lock.lock().await;
    discard_pending(runtime).await;
    let paths = SystemPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let sources = source::read_sources_config(&config)?
        .sources
        .into_iter()
        .map(|source| {
            let snapshot = source::load_current(&cache, &source).ok().flatten();
            (source, snapshot)
        })
        .collect::<Vec<_>>();
    let records = crate::executor::read_ledger(&paths)?
        .items
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let outcome = match crate::executor::reset_app(&paths, &sources) {
        Ok(outcome) => outcome,
        Err(message) => {
            let failures = records
                .into_iter()
                .map(|id| BulkFailure {
                    id,
                    message: format!("App reset transaction rolled back: {message}"),
                })
                .collect();
            return Ok(BulkResult {
                completed: Vec::new(),
                failures,
                backup_paths: Vec::new(),
            });
        }
    };
    if !crate::executor::read_ledger(&paths)?.items.is_empty() {
        return Ok(BulkResult {
            completed: Vec::new(),
            failures: vec![BulkFailure {
                id: "app".to_string(),
                message: "Resources were reset, but ledger cleanup was incomplete.".to_string(),
            }],
            backup_paths: Vec::new(),
        });
    }
    wipe_app_state(&paths)?;
    Ok(BulkResult {
        completed: records,
        failures: Vec::new(),
        backup_paths: outcome.backup_paths,
    })
}

async fn discard_pending(runtime: &RuntimeState) {
    let pending_sources = {
        let mut pending = runtime.pending_sources.lock().await;
        std::mem::take(&mut *pending)
    };
    for candidate in pending_sources.into_values() {
        source::discard_candidate(&candidate);
    }
    let pending_repositories = {
        let mut pending = runtime.pending_repositories.lock().await;
        std::mem::take(&mut *pending)
    };
    for candidate in pending_repositories.into_values() {
        source::discard_repository(&candidate);
    }
}

fn wipe_app_state(paths: &SystemPaths) -> Result<(), String> {
    for root in paths.state_roots() {
        match crate::fs_retry::remove_dir_all(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("Could not remove {}: {error}", root.display()));
            }
        }
    }
    Ok(())
}

pub(super) fn item_context(
    source_id: &str,
    local_id: &str,
) -> Result<(SystemPaths, ConfiguredSource, SourceSnapshot, CatalogItem), String> {
    let paths = SystemPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let source = source::configured_source(&config, source_id)?;
    let snapshot = source::load_current(&cache, &source)?
        .ok_or_else(|| format!("{} has no validated revision.", source.source_id))?;
    let item = snapshot
        .catalog
        .items
        .get(local_id)
        .cloned()
        .ok_or_else(|| format!("Unknown catalog item: {source_id}/{local_id}"))?;
    Ok((paths, source, snapshot, item))
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn wipe_app_state_removes_every_skill_manager_state_root() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        for directory in paths.state_roots() {
            fs::create_dir_all(&directory).expect("state root");
            fs::write(directory.join("marker.txt"), "keep out").expect("marker");
        }
        fs::create_dir_all(paths.home.join("other")).expect("other");
        fs::write(paths.home.join("other/file.txt"), "keep").expect("other file");

        wipe_app_state(&paths).expect("wipe");

        for directory in paths.state_roots() {
            assert!(!directory.exists(), "{}", directory.display());
        }
        assert_eq!(
            fs::read_to_string(paths.home.join("other/file.txt")).expect("kept"),
            "keep"
        );
    }

    #[test]
    fn wipe_app_state_ignores_missing_state_roots() {
        let root = tempfile::tempdir().expect("root");
        wipe_app_state(&paths(root.path())).expect("wipe");
    }
}
