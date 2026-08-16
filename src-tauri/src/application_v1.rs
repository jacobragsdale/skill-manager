//! Application service for source synchronization and file installation.

use crate::agent_profiles::{self, AgentProfileState, TargetId};
use crate::app_state::{
    AgentEnablePreview, AppState, AutoUpdateReport, BulkAction, BulkFailure, BulkPlan,
    BulkPlanEntry, BulkResult, CatalogItemState, ComponentState, ItemFailure, ItemReference,
    ListedSourceState, PreparedRepository, PreparedSource, RepositoryState, SourceState,
    SourceStatus,
};
use crate::catalog_v1::{CatalogComponentKind, CatalogItem};
use crate::executor::TargetCleanupPreview;
use crate::install_v1::{self, ItemStatus, OperationOutcome, SourceRemovalPlan};
use crate::ledger::{self, InstallationRecord};
use crate::locator::{default_catalog_locator, Locator};
use crate::paths::SystemPaths;
use crate::source_v1::{
    self, ConfiguredRepository, ConfiguredSource, RepositoryCandidate, RepositorySnapshot,
    SourceCandidate, SourceSnapshot, SourcesConfig,
};
use crate::sources::{cache_base_dir, config_base_dir};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{
    async_runtime::{self, Mutex},
    AppHandle, Emitter, Manager, Runtime,
};
use tokio::time::{self, MissedTickBehavior};

const SCHEDULED_SYNC_EVENT: &str = "scheduled-sync";
const SCHEDULED_SYNC_INTERVAL: Duration = Duration::from_secs(15 * 60);

pub(crate) struct RuntimeState {
    operation_lock: Mutex<()>,
    sync_lock: Mutex<()>,
    pending_sources: Mutex<BTreeMap<String, SourceCandidate>>,
    pending_repositories: Mutex<BTreeMap<String, RepositoryCandidate>>,
}

impl RuntimeState {
    pub(crate) fn new() -> Result<Self, String> {
        Ok(Self {
            operation_lock: Mutex::new(()),
            sync_lock: Mutex::new(()),
            pending_sources: Mutex::new(BTreeMap::new()),
            pending_repositories: Mutex::new(BTreeMap::new()),
        })
    }
}

struct LoadedSource {
    definition: ConfiguredSource,
    snapshot: Option<SourceSnapshot>,
    status: SourceStatus,
    refresh_failed: bool,
    message: Option<String>,
}

struct LoadedRepository {
    definition: ConfiguredRepository,
    snapshot: Option<RepositorySnapshot>,
    status: SourceStatus,
    refresh_failed: bool,
    message: Option<String>,
}

async fn run_blocking<T, F>(context: &'static str, task: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    async_runtime::spawn_blocking(task)
        .await
        .map_err(|error| format!("{context} worker failed: {error}"))?
}

pub(crate) async fn load_cached_app_state(
    runtime: &RuntimeState,
) -> Result<Option<AppState>, String> {
    let _guard = runtime.operation_lock.lock().await;
    run_blocking("Cached source load", || {
        let paths = SystemPaths::from_system()?;
        retire_unsupported_legacy_installs(&paths)?;
        agent_profiles::apply_detected_defaults(&paths)?;
        let cache = cache_base_dir()?;
        let config = config_base_dir()?;
        let checked = current_epoch_seconds();
        let config_file = source_v1::read_sources_config(&config)?;
        let repositories = config_file
            .repositories
            .into_iter()
            .map(
                |definition| match source_v1::load_current_repository(&cache, &definition) {
                    Ok(snapshot) => LoadedRepository {
                        definition,
                        snapshot,
                        status: SourceStatus::Cached,
                        refresh_failed: false,
                        message: None,
                    },
                    Err(message) => LoadedRepository {
                        definition,
                        snapshot: None,
                        status: SourceStatus::Error,
                        refresh_failed: true,
                        message: Some(message),
                    },
                },
            )
            .collect::<Vec<_>>();
        let loaded = config_file
            .sources
            .into_iter()
            .map(
                |definition| match source_v1::load_current(&cache, &definition) {
                    Ok(snapshot) => LoadedSource {
                        definition,
                        snapshot,
                        status: SourceStatus::Cached,
                        refresh_failed: false,
                        message: None,
                    },
                    Err(message) => LoadedSource {
                        definition,
                        snapshot: None,
                        status: SourceStatus::Error,
                        refresh_failed: true,
                        message: Some(message),
                    },
                },
            )
            .collect::<Vec<_>>();
        build_app_state(
            &paths,
            &repositories,
            &loaded,
            checked,
            AutoUpdateReport::default(),
            None,
        )
        .map(Some)
    })
    .await
}

pub(crate) async fn sync_app_state(runtime: &RuntimeState) -> Result<AppState, String> {
    let _sync_guard = runtime.sync_lock.lock().await;
    let _operation_guard = runtime.operation_lock.lock().await;
    run_blocking("Source synchronization", synchronize).await
}

fn synchronize() -> Result<AppState, String> {
    let paths = SystemPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let checked = current_epoch_seconds();
    let mut config_file = source_v1::read_sources_config(&config)?;
    let catalog_message = ensure_default_catalog(&cache, &mut config_file.repositories);
    let (updated_repositories, loaded_repositories) =
        refresh_repositories(&cache, config_file.repositories);
    let (updated_sources, loaded_sources) = refresh_sources(&cache, config_file.sources);
    source_v1::write_sources_config(
        &config,
        &SourcesConfig {
            repositories: updated_repositories,
            sources: updated_sources,
        },
    )?;
    retire_unsupported_legacy_installs(&paths)?;
    agent_profiles::apply_detected_defaults(&paths)?;
    let report = reconcile_installed_items(&paths, &loaded_sources)?;
    build_app_state(
        &paths,
        &loaded_repositories,
        &loaded_sources,
        checked,
        report,
        catalog_message,
    )
}

fn ensure_default_catalog(
    cache: &std::path::Path,
    repositories: &mut Vec<ConfiguredRepository>,
) -> Option<String> {
    let locator = match default_catalog_locator() {
        Ok(Some(locator)) => locator,
        Ok(None) => return None,
        Err(message) => return Some(message),
    };
    if repositories
        .iter()
        .any(|repository| repository.locator.same_identity(&locator))
    {
        return None;
    }
    match source_v1::prepare_new_repository(&locator, cache) {
        Ok(candidate) => match source_v1::activate_repository(cache, candidate) {
            Ok(snapshot) => {
                repositories.push(snapshot.definition);
                None
            }
            Err(message) => Some(message),
        },
        Err(message) => Some(message),
    }
}

fn refresh_repositories(
    cache: &std::path::Path,
    definitions: Vec<ConfiguredRepository>,
) -> (Vec<ConfiguredRepository>, Vec<LoadedRepository>) {
    let mut updated = Vec::with_capacity(definitions.len());
    let mut loaded = Vec::with_capacity(definitions.len());
    for definition in definitions {
        match source_v1::prepare_repository_refresh(&definition, cache) {
            Ok(candidate) => {
                if candidate.definition.repository_id != definition.repository_id {
                    source_v1::discard_repository(&candidate);
                    let snapshot = source_v1::load_current_repository(cache, &definition)
                        .ok()
                        .flatten();
                    let message = format!(
                        "The catalog changed repository.id from {} to {}. The last validated revision remains active.",
                        definition.repository_id, candidate.definition.repository_id
                    );
                    updated.push(definition.clone());
                    loaded.push(LoadedRepository {
                        definition,
                        snapshot,
                        status: SourceStatus::Error,
                        refresh_failed: true,
                        message: Some(message),
                    });
                    continue;
                }
                match source_v1::activate_repository(cache, candidate) {
                    Ok(snapshot) => {
                        updated.push(snapshot.definition.clone());
                        loaded.push(LoadedRepository {
                            definition: snapshot.definition.clone(),
                            snapshot: Some(snapshot),
                            status: SourceStatus::Fresh,
                            refresh_failed: false,
                            message: None,
                        });
                    }
                    Err(message) => {
                        let snapshot = source_v1::load_current_repository(cache, &definition)
                            .ok()
                            .flatten();
                        updated.push(definition.clone());
                        loaded.push(LoadedRepository {
                            definition,
                            snapshot,
                            status: SourceStatus::Error,
                            refresh_failed: true,
                            message: Some(message),
                        });
                    }
                }
            }
            Err(message) => {
                let snapshot = source_v1::load_current_repository(cache, &definition)
                    .ok()
                    .flatten();
                updated.push(definition.clone());
                loaded.push(LoadedRepository {
                    definition,
                    snapshot,
                    status: SourceStatus::Error,
                    refresh_failed: true,
                    message: Some(message),
                });
            }
        }
    }
    (updated, loaded)
}

fn refresh_sources(
    cache: &std::path::Path,
    definitions: Vec<ConfiguredSource>,
) -> (Vec<ConfiguredSource>, Vec<LoadedSource>) {
    let mut claimed = definitions
        .iter()
        .map(|source| (source.source_id.clone(), source.source_key.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut updated_definitions = Vec::with_capacity(definitions.len());
    let mut loaded = Vec::with_capacity(definitions.len());

    for definition in definitions {
        match source_v1::prepare_refresh(&definition, cache) {
            Ok(candidate) => {
                let source_id_changed = candidate.definition.source_id != definition.source_id;
                let duplicate_namespace = claimed
                    .get(&candidate.definition.source_id)
                    .is_some_and(|source_key| source_key != &definition.source_key);
                if source_id_changed || duplicate_namespace {
                    let message = if source_id_changed {
                        format!(
                            "The source changed source.id from {} to {}. The last validated revision remains active.",
                            definition.source_id, candidate.definition.source_id
                        )
                    } else {
                        format!(
                            "The namespace {} is already claimed by another source.",
                            candidate.definition.source_id
                        )
                    };
                    source_v1::discard_candidate(&candidate);
                    let snapshot = source_v1::load_current(cache, &definition).ok().flatten();
                    updated_definitions.push(definition.clone());
                    loaded.push(LoadedSource {
                        definition,
                        snapshot,
                        status: SourceStatus::Error,
                        refresh_failed: true,
                        message: Some(message),
                    });
                    continue;
                }
                match source_v1::activate_candidate(cache, candidate) {
                    Ok(snapshot) => {
                        claimed.insert(
                            snapshot.definition.source_id.clone(),
                            snapshot.definition.source_key.clone(),
                        );
                        updated_definitions.push(snapshot.definition.clone());
                        loaded.push(LoadedSource {
                            definition: snapshot.definition.clone(),
                            snapshot: Some(snapshot),
                            status: SourceStatus::Fresh,
                            refresh_failed: false,
                            message: None,
                        });
                    }
                    Err(message) => push_refresh_error(
                        cache,
                        definition,
                        message,
                        &mut updated_definitions,
                        &mut loaded,
                    ),
                }
            }
            Err(message) => push_refresh_error(
                cache,
                definition,
                message,
                &mut updated_definitions,
                &mut loaded,
            ),
        }
    }
    (updated_definitions, loaded)
}

fn is_unsupported_legacy_install(record: &InstallationRecord) -> bool {
    record.manifest_version == 1
        || matches!(
            record.component_kind.as_str(),
            "agentPlugin" | "legacyFileTree" | "fileTree"
        )
}

fn retire_unsupported_legacy_installs(paths: &SystemPaths) -> Result<(), String> {
    let ledger = crate::executor::read_ledger(paths)?;
    let mut groups = BTreeMap::<String, (ConfiguredSource, Vec<String>)>::new();
    for (id, record) in &ledger.items {
        if !is_unsupported_legacy_install(record) {
            continue;
        }
        groups
            .entry(record.source_key.clone())
            .or_insert_with(|| {
                (
                    ConfiguredSource {
                        source_key: record.source_key.clone(),
                        source_id: record.source_id.clone(),
                        name: record.source_id.clone(),
                        description: "Retired unsupported legacy installation.".to_string(),
                        locator: Locator::parse(&record.source_url)
                            .unwrap_or_else(|_| Locator::display_url(record.source_url.clone())),
                        repository_key: None,
                    },
                    Vec::new(),
                )
            })
            .1
            .push(id.clone());
    }
    for (source, ids) in groups.into_values() {
        crate::executor::uninstall_batch(paths, &source, &ids, true)?;
    }
    Ok(())
}

fn push_refresh_error(
    cache: &std::path::Path,
    definition: ConfiguredSource,
    message: String,
    updated_definitions: &mut Vec<ConfiguredSource>,
    loaded: &mut Vec<LoadedSource>,
) {
    let snapshot = source_v1::load_current(cache, &definition).ok().flatten();
    updated_definitions.push(definition.clone());
    loaded.push(LoadedSource {
        definition,
        snapshot,
        status: SourceStatus::Error,
        refresh_failed: true,
        message: Some(message),
    });
}

fn reconcile_installed_items(
    paths: &SystemPaths,
    loaded: &[LoadedSource],
) -> Result<AutoUpdateReport, String> {
    let mut report = AutoUpdateReport::default();
    let agents_enabled = agent_profiles::read(paths)?
        .iter()
        .any(|profile| profile.enabled);
    for source in loaded {
        let Some(snapshot) = &source.snapshot else {
            continue;
        };
        for item in snapshot.catalog.items.values() {
            if item.manifest_version == 2 && !agents_enabled {
                continue;
            }
            let ledger_state = crate::executor::read_ledger(paths)?;
            if refined_item_status(paths, &ledger_state, snapshot, item, None)
                != ItemStatus::UpdateAvailable
            {
                continue;
            }
            let selected = ledger_state
                .items
                .get(&item.id)
                .map(|record| crate::planner::selected_component_ids(record, item));
            match install_v1::install_item_components_approved(
                paths,
                &source.definition,
                snapshot,
                item,
                false,
                selected.as_deref(),
            ) {
                Ok(_) => report.updated_items.push(ItemReference {
                    id: item.id.clone(),
                    source_id: item.source_id.clone(),
                    local_id: item.local_id.clone(),
                }),
                Err(message) => report.failed_items.push(ItemFailure {
                    id: item.id.clone(),
                    message,
                }),
            }
        }
    }
    Ok(report)
}

fn build_app_state(
    paths: &SystemPaths,
    repositories: &[LoadedRepository],
    loaded: &[LoadedSource],
    checked: u64,
    report: AutoUpdateReport,
    catalog_message: Option<String>,
) -> Result<AppState, String> {
    let ledger_state = crate::executor::read_ledger(paths)?;
    let mut current_ids = BTreeSet::new();
    let mut items = Vec::new();
    let mut sources = Vec::new();
    let configured_urls = loaded
        .iter()
        .map(|source| source.definition.locator.url().to_string())
        .collect::<BTreeSet<_>>();
    let mut repository_states = repositories
        .iter()
        .map(|repository| repository_state(repository, &configured_urls, checked))
        .collect::<Vec<_>>();
    for loaded_source in loaded {
        let catalog_errors = loaded_source
            .snapshot
            .as_ref()
            .map_or_else(Vec::new, |snapshot| snapshot.catalog.errors.clone());
        sources.push(SourceState {
            source_id: loaded_source.definition.source_id.clone(),
            source_key: loaded_source.definition.source_key.clone(),
            name: loaded_source.definition.name.clone(),
            description: loaded_source.definition.description.clone(),
            url: loaded_source.definition.url().to_string(),
            repository_key: loaded_source.definition.repository_key.clone(),
            status: loaded_source.status,
            refresh_failed: loaded_source.refresh_failed,
            message: loaded_source.message.clone(),
            commit: loaded_source
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.commit.clone()),
            checked_at_epoch_seconds: checked,
            catalog_errors,
        });
        if let Some(snapshot) = &loaded_source.snapshot {
            for item in snapshot.catalog.items.values() {
                current_ids.insert(item.id.clone());
                items.push(current_item_state(
                    paths,
                    &ledger_state,
                    &loaded_source.definition,
                    snapshot,
                    item,
                )?);
            }
        }
    }
    for (id, record) in &ledger_state.items {
        if current_ids.contains(id) || is_unsupported_legacy_install(record) {
            continue;
        }
        let definition = loaded
            .iter()
            .find(|source| source.definition.source_key == record.source_key)
            .map(|source| source.definition.clone())
            .unwrap_or_else(|| ConfiguredSource {
                source_key: record.source_key.clone(),
                source_id: record.source_id.clone(),
                name: record.source_id.clone(),
                description: "This source is no longer configured.".to_string(),
                locator: Locator::parse(&record.source_url)
                    .unwrap_or_else(|_| Locator::display_url(record.source_url.clone())),
                repository_key: None,
            });
        items.push(removed_item_state(
            paths,
            &ledger_state,
            &definition,
            id,
            record,
        )?);
    }
    items.sort_by(|left, right| left.id.cmp(&right.id));
    sources.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.source_id.cmp(&right.source_id))
    });
    repository_states.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.repository_id.cmp(&right.repository_id))
    });
    Ok(AppState {
        checked_at_epoch_seconds: checked,
        auto_update_report: report,
        catalog_message,
        repositories: repository_states,
        sources,
        items,
        agent_profiles: agent_profiles::states(paths)?,
    })
}

fn repository_state(
    loaded: &LoadedRepository,
    configured_urls: &BTreeSet<String>,
    checked: u64,
) -> RepositoryState {
    let listed = loaded
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.manifest.canonical_sources().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|source| {
            let already_added = source
                .locator()
                .is_ok_and(|locator| configured_urls.contains(locator.url()));
            ListedSourceState {
                name: source.name,
                description: source.description,
                url: source.url,
                source_id: source.source_id,
                already_added,
            }
        })
        .collect();
    RepositoryState {
        repository_id: loaded.definition.repository_id.clone(),
        repository_key: loaded.definition.repository_key.clone(),
        name: loaded.definition.name.clone(),
        description: loaded.definition.description.clone(),
        url: loaded.definition.url().to_string(),
        status: loaded.status,
        refresh_failed: loaded.refresh_failed,
        message: loaded.message.clone(),
        revision: loaded
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.revision.clone()),
        checked_at_epoch_seconds: checked,
        sources: listed,
    }
}

fn current_item_state(
    paths: &SystemPaths,
    ledger_state: &ledger::InstallationLedger,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
) -> Result<CatalogItemState, String> {
    let plan = crate::planner::plan_install(paths, snapshot, item).ok();
    let compatibility = plan
        .as_ref()
        .map(|plan| plan.compatibility.clone())
        .unwrap_or_default();
    let record = ledger_state.items.get(&item.id);
    let status = refined_item_status(paths, ledger_state, snapshot, item, plan.as_ref());
    Ok(CatalogItemState {
        id: item.id.clone(),
        local_id: item.local_id.clone(),
        source_id: source.source_id.clone(),
        source_key: source.source_key.clone(),
        source_name: source.name.clone(),
        source_url: source.url().to_string(),
        name: item.name.clone(),
        description: item.description.clone(),
        manual_invocation: item.disable_model_invocation,
        source: item.source.clone(),
        source_is_directory: item.source_is_directory,
        manifest_version: item.manifest_version,
        components: item
            .components
            .iter()
            .map(|component| ComponentState {
                id: component.id.clone(),
                kind: component_kind_label(component.kind).to_string(),
                status: component_status(
                    paths,
                    ledger_state,
                    snapshot,
                    item,
                    &component.id,
                    record,
                    status,
                ),
            })
            .collect(),
        compatibility,
        destination: match record {
            Some(record) => Some(
                paths
                    .resolve_owned(&record.destination)?
                    .display()
                    .to_string(),
            ),
            None => None,
        },
        status,
    })
}

fn refined_item_status(
    paths: &SystemPaths,
    ledger_state: &ledger::InstallationLedger,
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
        crate::planner::plan_install_components(paths, snapshot, item, Some(&selected)).ok()
    };
    let mut status = install_v1::item_status(paths, ledger_state, Some(item), &item.id);
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

fn component_status(
    paths: &SystemPaths,
    ledger_state: &ledger::InstallationLedger,
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
    let plan = crate::planner::plan_install_components(
        paths,
        snapshot,
        item,
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

fn component_resources_match(
    paths: &SystemPaths,
    ledger_state: &ledger::InstallationLedger,
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

fn removed_item_state(
    paths: &SystemPaths,
    ledger_state: &ledger::InstallationLedger,
    source: &ConfiguredSource,
    id: &str,
    record: &InstallationRecord,
) -> Result<CatalogItemState, String> {
    Ok(CatalogItemState {
        id: id.to_string(),
        local_id: record.local_id.clone(),
        source_id: record.source_id.clone(),
        source_key: record.source_key.clone(),
        source_name: source.name.clone(),
        source_url: record.source_url.clone(),
        name: record.name.clone(),
        description: format!(
            "{} This install is no longer published by its source.",
            record.description
        ),
        manual_invocation: record.disable_model_invocation,
        source: record.source.clone(),
        source_is_directory: false,
        manifest_version: record.manifest_version,
        components: vec![ComponentState {
            id: record.local_id.clone(),
            kind: record.component_kind.clone(),
            status: install_v1::item_status(paths, ledger_state, None, id),
        }],
        compatibility: Vec::new(),
        destination: Some(
            paths
                .resolve_owned(&record.destination)?
                .display()
                .to_string(),
        ),
        status: install_v1::item_status(paths, ledger_state, None, id),
    })
}

pub(crate) async fn prepare_source(
    runtime: &RuntimeState,
    url: &str,
    repository_key: String,
) -> Result<PreparedSource, String> {
    let _guard = runtime.operation_lock.lock().await;
    let locator = Locator::parse(url)?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let config_file = source_v1::read_sources_config(&config)?;
    let repository = config_file
        .repositories
        .iter()
        .find(|repository| repository.repository_key == repository_key)
        .ok_or_else(|| "That source catalog is no longer configured.".to_string())?;
    let snapshot = source_v1::load_current_repository(&cache, repository)?
        .ok_or_else(|| "That source catalog has no validated revision.".to_string())?;
    let listed = snapshot.manifest.canonical_sources()?;
    let listing = listed
        .iter()
        .find(|source| {
            source
                .locator()
                .is_ok_and(|listed_locator| listed_locator.same_identity(&locator))
        })
        .ok_or_else(|| "That source is no longer listed by the catalog.".to_string())?;
    let expected_source_id = listing.source_id.clone();
    let repository_key_for_prep = repository_key.clone();
    let candidate = run_blocking("Source preparation", move || {
        source_v1::prepare_new_source(
            &locator,
            &cache,
            Some(repository_key_for_prep),
            expected_source_id.as_deref(),
        )
    })
    .await?;
    if config_file.sources.iter().any(|source| {
        source.source_key == candidate.definition.source_key
            || source.locator.same_identity(&candidate.definition.locator)
    }) {
        source_v1::discard_candidate(&candidate);
        return Err(format!(
            "{} is already configured.",
            candidate.definition.url()
        ));
    }
    if config_file
        .sources
        .iter()
        .any(|source| source.source_id == candidate.definition.source_id)
    {
        source_v1::discard_candidate(&candidate);
        return Err(format!(
            "The namespace {} is already claimed by another locator.",
            candidate.definition.source_id
        ));
    }
    let token = prepared_token(&candidate);
    let preview = PreparedSource {
        token: token.clone(),
        source_id: candidate.definition.source_id.clone(),
        source_key: candidate.definition.source_key.clone(),
        name: candidate.definition.name.clone(),
        description: candidate.definition.description.clone(),
        url: candidate.definition.url().to_string(),
        commit: candidate.commit.clone(),
        item_count: candidate.catalog.items.len(),
    };
    runtime
        .pending_sources
        .lock()
        .await
        .insert(token, candidate);
    Ok(preview)
}

pub(crate) async fn confirm_source(
    runtime: &RuntimeState,
    token: &str,
) -> Result<AppState, String> {
    let _guard = runtime.operation_lock.lock().await;
    let candidate = runtime
        .pending_sources
        .lock()
        .await
        .remove(token)
        .ok_or_else(|| {
            "The prepared source is no longer available. Prepare it again.".to_string()
        })?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let snapshot = run_blocking("Prepared source activation", move || {
        source_v1::activate_candidate(&cache, candidate)
    })
    .await?;
    let mut config_file = source_v1::read_sources_config(&config)?;
    if config_file.sources.iter().any(|source| {
        source.source_key == snapshot.definition.source_key
            || source.source_id == snapshot.definition.source_id
            || source.locator.same_identity(&snapshot.definition.locator)
    }) {
        return Err("The source was configured while confirmation was open.".to_string());
    }
    config_file.sources.push(snapshot.definition);
    config_file.sources.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.source_id.cmp(&right.source_id))
    });
    source_v1::write_sources_config(&config, &config_file)?;
    cached_state_now()
}

pub(crate) async fn cancel_prepared_source(
    runtime: &RuntimeState,
    token: &str,
) -> Result<(), String> {
    if let Some(candidate) = runtime.pending_sources.lock().await.remove(token) {
        source_v1::discard_candidate(&candidate);
    }
    Ok(())
}

pub(crate) async fn prepare_source_repository(
    runtime: &RuntimeState,
    url: &str,
) -> Result<PreparedRepository, String> {
    let _guard = runtime.operation_lock.lock().await;
    let locator = Locator::parse(url)?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let configured = source_v1::read_sources_config(&config)?;
    let candidate = run_blocking("Source repository preparation", move || {
        source_v1::prepare_new_repository(&locator, &cache)
    })
    .await?;
    if configured.repositories.iter().any(|repository| {
        repository.repository_key == candidate.definition.repository_key
            || repository.repository_id == candidate.definition.repository_id
            || repository
                .locator
                .same_identity(&candidate.definition.locator)
    }) {
        source_v1::discard_repository(&candidate);
        return Err(format!(
            "{} is already configured.",
            candidate.definition.url()
        ));
    }
    let token = prepared_repository_token(&candidate);
    let preview = PreparedRepository {
        token: token.clone(),
        repository_id: candidate.definition.repository_id.clone(),
        repository_key: candidate.definition.repository_key.clone(),
        name: candidate.definition.name.clone(),
        description: candidate.definition.description.clone(),
        url: candidate.definition.url().to_string(),
        revision: candidate.revision.clone(),
        source_count: candidate.manifest.sources.len(),
    };
    runtime
        .pending_repositories
        .lock()
        .await
        .insert(token, candidate);
    Ok(preview)
}

pub(crate) async fn confirm_source_repository(
    runtime: &RuntimeState,
    token: &str,
) -> Result<AppState, String> {
    let _guard = runtime.operation_lock.lock().await;
    let candidate = runtime
        .pending_repositories
        .lock()
        .await
        .remove(token)
        .ok_or_else(|| {
            "The prepared source repository is no longer available. Prepare it again.".to_string()
        })?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let snapshot = run_blocking("Prepared source repository activation", move || {
        source_v1::activate_repository(&cache, candidate)
    })
    .await?;
    let mut config_file = source_v1::read_sources_config(&config)?;
    if config_file.repositories.iter().any(|repository| {
        repository.repository_key == snapshot.definition.repository_key
            || repository.repository_id == snapshot.definition.repository_id
            || repository
                .locator
                .same_identity(&snapshot.definition.locator)
    }) {
        return Err(
            "The source repository was configured while confirmation was open.".to_string(),
        );
    }
    config_file.repositories.push(snapshot.definition);
    config_file.repositories.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.repository_id.cmp(&right.repository_id))
    });
    source_v1::write_sources_config(&config, &config_file)?;
    cached_state_now()
}

pub(crate) async fn cancel_prepared_source_repository(
    runtime: &RuntimeState,
    token: &str,
) -> Result<(), String> {
    if let Some(candidate) = runtime.pending_repositories.lock().await.remove(token) {
        source_v1::discard_repository(&candidate);
    }
    Ok(())
}

pub(crate) async fn remove_source_repository(
    runtime: &RuntimeState,
    repository_key: &str,
) -> Result<AppState, String> {
    let _guard = runtime.operation_lock.lock().await;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let mut config_file = source_v1::read_sources_config(&config)?;
    if !config_file
        .repositories
        .iter()
        .any(|repository| repository.repository_key == repository_key)
    {
        return Err("Unknown source repository.".to_string());
    }
    config_file
        .repositories
        .retain(|repository| repository.repository_key != repository_key);
    source_v1::write_sources_config(&config, &config_file)?;
    source_v1::remove_repository_cache(&cache, repository_key)?;
    cached_state_now()
}

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
        None => {
            install_v1::install_item_approved(&paths, &source, &snapshot, &item, trust_approved)
        }
        Some(ids) => install_v1::install_item_components_approved(
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
        None => {
            install_v1::replace_item_approved(&paths, &source, &snapshot, &item, trust_approved)
        }
        Some(ids) => install_v1::replace_item_components_approved(
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
    let plan = crate::planner::plan_install_components(&paths, &snapshot, &item, ids.as_deref())?;
    Ok(crate::planner::preview(&item, &plan))
}

fn requested_component_ids(
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
                None => {
                    crate::planner::plan_install_with_profiles(&paths, snapshot, item, &profiles)?
                }
                Some(ids) => crate::planner::plan_install_components_with_profiles(
                    &paths,
                    snapshot,
                    item,
                    &profiles,
                    Some(ids),
                )?,
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

fn reconcile_enabled_target(paths: &SystemPaths, trust_approved: bool) -> Result<(), String> {
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

fn installed_v2_contexts(
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
    for source in source_v1::read_sources(&config)? {
        let Some(snapshot) = source_v1::load_current(&cache, &source)? else {
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

pub(crate) async fn uninstall_item(
    runtime: &RuntimeState,
    source_id: &str,
    local_id: &str,
    component_id: Option<&str>,
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let paths = SystemPaths::from_system()?;
    let config = config_base_dir()?;
    let source = source_v1::configured_source(&config, source_id)?;
    let ids = component_id.map(|component_id| vec![component_id.to_string()]);
    install_v1::uninstall_item_components(
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
    let source = source_v1::configured_source(&config, source_id)?;
    let snapshot = source_v1::load_current(&cache, &source)?
        .ok_or_else(|| format!("{} has no validated revision.", source.source_id))?;
    let ledger_state = crate::executor::read_ledger(&paths)?;
    let entries = snapshot
        .catalog
        .items
        .values()
        .map(|item| {
            let status = refined_item_status(&paths, &ledger_state, &snapshot, item, None);
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
    let source = source_v1::configured_source(&config, source_id)?;
    let snapshot = source_v1::load_current(&cache, &source)?
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
    let source = source_v1::configured_source(&config, source_id)?;
    install_v1::source_removal_plan(&paths, &source)
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
    let source = source_v1::configured_source(&config, source_id)?;
    let plan = install_v1::source_removal_plan(&paths, &source)?;
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
    let mut config_file = source_v1::read_sources_config(&config)?;
    config_file
        .sources
        .retain(|configured| configured.source_key != source.source_key);
    source_v1::write_sources_config(&config, &config_file)?;
    source_v1::remove_source_cache(&cache, &source.source_key)?;
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
    let source = source_v1::configured_source(&config, source_id)?;
    let snapshot = source_v1::load_current(&cache, &source)?;
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
    let records = install_v1::source_reset_ids(
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
    if !install_v1::source_reset_ids(
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

fn item_context(
    source_id: &str,
    local_id: &str,
) -> Result<(SystemPaths, ConfiguredSource, SourceSnapshot, CatalogItem), String> {
    let paths = SystemPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let source = source_v1::configured_source(&config, source_id)?;
    let snapshot = source_v1::load_current(&cache, &source)?
        .ok_or_else(|| format!("{} has no validated revision.", source.source_id))?;
    let item = snapshot
        .catalog
        .items
        .get(local_id)
        .cloned()
        .ok_or_else(|| format!("Unknown catalog item: {source_id}/{local_id}"))?;
    Ok((paths, source, snapshot, item))
}

fn component_kind_label(kind: CatalogComponentKind) -> &'static str {
    match kind {
        CatalogComponentKind::Skill => "skill",
        CatalogComponentKind::McpServer => "mcpServer",
    }
}

fn cached_state_now() -> Result<AppState, String> {
    let paths = SystemPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let checked = current_epoch_seconds();
    let config_file = source_v1::read_sources_config(&config)?;
    let repositories = config_file
        .repositories
        .into_iter()
        .map(|definition| LoadedRepository {
            snapshot: source_v1::load_current_repository(&cache, &definition)
                .ok()
                .flatten(),
            definition,
            status: SourceStatus::Cached,
            refresh_failed: false,
            message: None,
        })
        .collect::<Vec<_>>();
    let loaded = config_file
        .sources
        .into_iter()
        .map(|definition| LoadedSource {
            snapshot: source_v1::load_current(&cache, &definition).ok().flatten(),
            definition,
            status: SourceStatus::Cached,
            refresh_failed: false,
            message: None,
        })
        .collect::<Vec<_>>();
    build_app_state(
        &paths,
        &repositories,
        &loaded,
        checked,
        AutoUpdateReport::default(),
        None,
    )
}

fn prepared_token(candidate: &SourceCandidate) -> String {
    hash_token(&candidate.definition.source_key, &candidate.commit)
}

fn prepared_repository_token(candidate: &RepositoryCandidate) -> String {
    hash_token(&candidate.definition.repository_key, &candidate.revision)
}

fn hash_token(key: &str, revision: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hasher.update(revision.as_bytes());
    hasher.update(current_epoch_seconds().to_le_bytes());
    hasher
        .finalize()
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn current_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) async fn run_scheduled_sync<R: Runtime>(app: AppHandle<R>) {
    let mut interval = time::interval(SCHEDULED_SYNC_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval.tick().await;
    loop {
        interval.tick().await;
        let Some(runtime) = app.try_state::<RuntimeState>() else {
            eprintln!("Scheduled source sync stopped because runtime state is unavailable.");
            return;
        };
        let event = match sync_app_state(runtime.inner()).await {
            Ok(state) => crate::app_state::ScheduledSync::Updated {
                state: Box::new(state),
            },
            Err(message) => crate::app_state::ScheduledSync::Failed { message },
        };
        if let Err(error) = app.emit(SCHEDULED_SYNC_EVENT, &event) {
            eprintln!("Could not publish scheduled source sync: {error}");
        }
    }
}

pub(crate) fn spawn_app_sync<R: Runtime>(app: AppHandle<R>) {
    async_runtime::spawn(async move {
        let Some(runtime) = app.try_state::<RuntimeState>() else {
            return;
        };
        if let Ok(state) = sync_app_state(runtime.inner()).await {
            let _ = app.emit(
                SCHEDULED_SYNC_EVENT,
                crate::app_state::ScheduledSync::Updated {
                    state: Box::new(state),
                },
            );
        }
    });
}

#[cfg(test)]
mod live_nexus_tests {
    use super::*;

    #[test]
    #[ignore = "hits the live Nexus catalog; run with SKILL_MANAGER_QA_ROOT set"]
    fn live_nexus_catalog_round_trip() {
        assert!(
            crate::qa_paths::root().expect("qa root").is_some(),
            "SKILL_MANAGER_QA_ROOT must name a directory under the process temp dir"
        );
        async_runtime::block_on(async {
            let runtime = RuntimeState::new().expect("runtime");
            match live_step().as_str() {
                "sync" => {
                    print_live_state(&sync_app_state(&runtime).await.expect("sync"));
                }
                "add" => {
                    print_live_state(&add_listed_skillbook(&runtime).await.expect("add"));
                }
                "refresh" => {
                    print_live_state(&sync_app_state(&runtime).await.expect("refresh"));
                }
                "install" => {
                    print_live_state(&install_git_ops(&runtime).await.expect("install"));
                }
                "remove" => {
                    let result = remove_source(&runtime, "skillbook", false)
                        .await
                        .expect("remove");
                    assert!(
                        result.failures.is_empty(),
                        "remove failed: {:?}",
                        result.failures
                    );
                    print_live_state(
                        &load_cached_app_state(&runtime)
                            .await
                            .expect("load")
                            .expect("state"),
                    );
                }
                _ => {
                    let added = add_listed_skillbook(&runtime).await.expect("add");
                    assert!(
                        added.catalog_message.is_none(),
                        "{:?}",
                        added.catalog_message
                    );
                    assert_eq!(added.repositories.len(), 1);
                    assert_eq!(added.repositories[0].name, "Ragsdale sources");
                    assert_eq!(
                        added.repositories[0].description,
                        "Official portable sources published from repo.ragsdale.dev."
                    );
                    assert_eq!(added.repositories[0].sources[0].name, "Skillbook");
                    assert_eq!(added.sources.len(), 1);
                    assert_eq!(added.items.len(), 27);
                    assert!(added
                        .items
                        .iter()
                        .all(|item| item.status == ItemStatus::Available));
                    let commit = added.sources[0].commit.clone();
                    let refreshed = sync_app_state(&runtime).await.expect("refresh");
                    assert_eq!(refreshed.sources[0].commit, commit);
                    assert!(!refreshed.sources[0].refresh_failed);
                    let removed = remove_source(&runtime, "skillbook", false)
                        .await
                        .expect("remove");
                    assert!(removed.failures.is_empty(), "{:?}", removed.failures);
                    let after = load_cached_app_state(&runtime)
                        .await
                        .expect("load")
                        .expect("state");
                    assert!(after.sources.is_empty());
                    assert_eq!(after.repositories.len(), 1);
                    assert!(!after.repositories[0].sources[0].already_added);
                    print_live_state(&after);
                }
            }
        });
    }

    fn live_step() -> String {
        std::env::var("SKILL_MANAGER_LIVE_STEP").unwrap_or_else(|_| "all".to_string())
    }

    async fn add_listed_skillbook(runtime: &RuntimeState) -> Result<AppState, String> {
        let state = sync_app_state(runtime).await?;
        if state
            .sources
            .iter()
            .any(|source| source.source_id == "skillbook")
        {
            return Ok(state);
        }
        let repository = state
            .repositories
            .first()
            .ok_or_else(|| "Live catalog was not added.".to_string())?;
        let listed = repository
            .sources
            .first()
            .ok_or_else(|| "Live catalog listed no sources.".to_string())?;
        let prepared =
            prepare_source(runtime, &listed.url, repository.repository_key.clone()).await?;
        confirm_source(runtime, &prepared.token).await
    }

    async fn install_git_ops(runtime: &RuntimeState) -> Result<AppState, String> {
        let state = add_listed_skillbook(runtime).await?;
        if !state.agent_profiles.iter().any(|profile| profile.enabled) {
            set_agent_enabled(runtime, TargetId::GrokBuild, true, false, true).await?;
        }
        install_item(runtime, "skillbook", "git-ops", true, None).await?;
        load_cached_app_state(runtime)
            .await?
            .ok_or_else(|| "App state missing after install.".to_string())
    }

    fn print_live_state(state: &AppState) {
        let repository = state.repositories.first();
        println!(
            "LIVE catalog_message={} repo_name={} repo_refresh_failed={} listed={} already_added={}",
            state.catalog_message.as_deref().unwrap_or("-"),
            repository.map_or("-", |repository| repository.name.as_str()),
            repository.is_some_and(|repository| repository.refresh_failed),
            repository.map_or(0, |repository| repository.sources.len()),
            repository
                .and_then(|repository| repository.sources.first())
                .is_some_and(|source| source.already_added)
        );
        if let Some(source) = state.sources.first() {
            let installed = state
                .items
                .iter()
                .filter(|item| item.status == ItemStatus::Installed)
                .count();
            println!(
                "LIVE source={} commit={} items={} installed={} refresh_failed={}",
                source.source_id,
                source.commit.as_deref().unwrap_or("-"),
                state.items.len(),
                installed,
                source.refresh_failed
            );
        } else {
            println!("LIVE source=-");
        }
    }
}
