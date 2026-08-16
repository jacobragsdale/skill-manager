use super::RuntimeState;
use crate::agent_profiles::{self, AgentProfileState, TargetId};
use crate::app_state::AgentEnablePreview;
use crate::catalog::CatalogItem;
use crate::executor::TargetCleanupPreview;
use crate::paths::SystemPaths;
use crate::source::{self, ConfiguredSource, SourceSnapshot};
use crate::sources::{cache_base_dir, config_base_dir};
use std::collections::BTreeSet;

pub(crate) async fn list_agent_profiles(
    runtime: &RuntimeState,
) -> Result<Vec<AgentProfileState>, String> {
    let _guard = runtime.operation_lock.lock().await;
    agent_profiles::states(&SystemPaths::from_system()?)
}

pub(crate) async fn preview_agent_cleanup(
    runtime: &RuntimeState,
    target_id: TargetId,
) -> Result<TargetCleanupPreview, String> {
    let _guard = runtime.operation_lock.lock().await;
    crate::executor::preview_target_cleanup(&SystemPaths::from_system()?, target_id)
}

pub(crate) async fn preview_agent_enable(
    runtime: &RuntimeState,
    target_id: TargetId,
) -> Result<AgentEnablePreview, String> {
    let _guard = runtime.operation_lock.lock().await;
    let paths = SystemPaths::from_system()?;
    let mut profiles = agent_profiles::read(&paths)?;
    profiles
        .iter_mut()
        .find(|profile| profile.target_id == target_id)
        .expect("all known profiles are materialized")
        .enabled = true;
    let ledger_state = crate::executor::read_ledger(&paths)?;
    let packages = installed_v2_contexts(&paths)?
        .iter()
        .map(|(_, snapshot, item)| {
            let selected = ledger_state.items.get(&item.id).and_then(|record| {
                if record.selected_component_ids.is_empty() {
                    None
                } else {
                    Some(crate::planner::selected_component_ids(record, item))
                }
            });
            let plan = match selected.as_deref() {
                None => crate::planner::plan(&paths, snapshot, item, Some(&profiles), None)?,
                Some(ids) => {
                    crate::planner::plan(&paths, snapshot, item, Some(&profiles), Some(ids))?
                }
            };
            Ok(crate::planner::preview(item, &plan))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(AgentEnablePreview {
        target_id,
        packages,
    })
}

pub(crate) async fn set_agent_enabled(
    runtime: &RuntimeState,
    target_id: TargetId,
    enabled: bool,
    acknowledge_modified_resources: bool,
    trust_approved: bool,
) -> Result<Vec<AgentProfileState>, String> {
    let _guard = runtime.operation_lock.lock().await;
    let paths = SystemPaths::from_system()?;
    if !enabled {
        crate::executor::disable_target(&paths, target_id, acknowledge_modified_resources)?;
        agent_profiles::set_enabled(&paths, target_id, false)?;
        if agent_profiles::read(&paths)?
            .iter()
            .any(|profile| profile.enabled)
        {
            if let Err(error) = reconcile_enabled_target(&paths, true) {
                return Err(format!(
                    "Disabled {}, but remaining agents could not be reconfigured: {error}",
                    target_id.display_name()
                ));
            }
        }
        return agent_profiles::states(&paths);
    }
    agent_profiles::set_enabled(&paths, target_id, true)?;
    if let Err(error) = reconcile_enabled_target(&paths, trust_approved) {
        let cleanup = crate::executor::disable_target(&paths, target_id, false);
        let profile_reset = agent_profiles::set_enabled(&paths, target_id, false);
        return match (cleanup, profile_reset) {
            (Ok(_), Ok(_)) => Err(error),
            (cleanup, reset) => Err(format!(
                "{error} Reverting the target failed: cleanup={cleanup:?}, profile={reset:?}."
            )),
        };
    }
    agent_profiles::states(&paths)
}

pub(super) fn reconcile_enabled_target(
    paths: &SystemPaths,
    trust_approved: bool,
) -> Result<(), String> {
    let contexts = installed_v2_contexts(paths)?;
    let requests = contexts
        .iter()
        .map(|(source, snapshot, item)| crate::executor::BatchInstall {
            source,
            snapshot,
            item,
            replace_unmanaged: false,
        })
        .collect::<Vec<_>>();
    crate::executor::install_batch(paths, &requests, trust_approved)?;
    Ok(())
}

pub(super) fn installed_v2_contexts(
    paths: &SystemPaths,
) -> Result<Vec<(ConfiguredSource, SourceSnapshot, CatalogItem)>, String> {
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let installed = crate::executor::read_ledger(paths)?
        .items
        .iter()
        .filter(|(_, record)| record.manifest_version == 2)
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    if installed.is_empty() {
        return Ok(Vec::new());
    }
    let mut contexts = Vec::new();
    let mut found = BTreeSet::new();
    for source in source::read_sources(&config)? {
        let Some(snapshot) = source::load_current(&cache, &source)? else {
            continue;
        };
        for item in snapshot
            .catalog
            .items
            .values()
            .filter(|item| installed.contains(&item.id))
        {
            found.insert(item.id.clone());
            contexts.push((source.clone(), snapshot.clone(), item.clone()));
        }
    }
    if let Some(missing) = installed.difference(&found).next() {
        return Err(format!(
            "Installed portable package {missing} has no available validated source revision."
        ));
    }
    Ok(contexts)
}
