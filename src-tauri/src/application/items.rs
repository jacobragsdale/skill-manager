use super::RuntimeState;
use crate::app_state::{BulkAction, BulkFailure, BulkPlan, BulkPlanEntry, BulkResult};
use crate::catalog::CatalogItem;
use crate::install::{self, ItemStatus, OperationOutcome, SourceRemovalPlan};
use crate::paths::SystemPaths;
use crate::source::{self, ConfiguredSource, SourceSnapshot};
use crate::sources::{cache_base_dir, config_base_dir};
use std::collections::BTreeSet;

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

pub(crate) async fn reset_source(
    runtime: &RuntimeState,
    source_id: &str,
) -> Result<BulkResult, String> {
    let _guard = runtime.operation_lock.lock().await;
    let paths = SystemPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let source = source::configured_source(&config, source_id)?;
    let snapshot = source::load_current(&cache, &source)?;
    let catalog_ids = snapshot
        .as_ref()
        .map(|snapshot| {
            snapshot
                .catalog
                .items
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let records = install::source_reset_ids(
        &crate::executor::read_ledger(&paths)?,
        &source,
        &catalog_ids,
    );
    let outcome = match crate::executor::reset_source(&paths, &source, snapshot.as_ref()) {
        Ok(outcome) => outcome,
        Err(message) => {
            let failures = records
                .into_iter()
                .map(|id| BulkFailure {
                    id,
                    message: format!("Source reset transaction rolled back: {message}"),
                })
                .collect();
            return Ok(BulkResult {
                completed: Vec::new(),
                failures,
                backup_paths: Vec::new(),
            });
        }
    };
    if !install::source_reset_ids(
        &crate::executor::read_ledger(&paths)?,
        &source,
        &catalog_ids,
    )
    .is_empty()
    {
        return Ok(BulkResult {
            completed: Vec::new(),
            failures: vec![BulkFailure {
                id: source.source_id.clone(),
                message: "Source resources were reset, but ledger cleanup was incomplete."
                    .to_string(),
            }],
            backup_paths: Vec::new(),
        });
    }
    Ok(BulkResult {
        completed: records,
        failures: Vec::new(),
        backup_paths: outcome.backup_paths,
    })
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
