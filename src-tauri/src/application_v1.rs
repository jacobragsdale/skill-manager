//! Application service for source synchronization and file installation.

use crate::agent_profiles::{self, AgentProfileState, TargetId};
use crate::catalog_v1::{CatalogComponentKind, CatalogItem};
use crate::executor::TargetCleanupPreview;
use crate::install_v1::{self, ItemStatus, OperationOutcome, SourceRemovalPlan, SystemPaths};
use crate::ipc_v1::{
    AgentEnablePreview, AppState, AutoUpdateReport, BulkAction, BulkFailure, BulkPlan,
    BulkPlanEntry, BulkResult, CatalogItemState, ComponentState, ItemFailure, ItemReference,
    ListedSourceState, PreparedRepository, PreparedSource, RepositoryState, SourceState,
    SourceStatus,
};
use crate::ledger::{self, InstallationRecord};
use crate::locator::{Locator, LocatorKind};
use crate::source_v1::{
    self, ConfiguredRepository, ConfiguredSource, RepositoryCandidate, RepositorySnapshot,
    SourceCandidate, SourceSnapshot, SourcesConfig, BUILT_IN_SOURCE_KEY, CATALOG_SOURCE,
};
use crate::sources::{cache_base_dir, config_base_dir, repository_url_key};
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
    let config_file = source_v1::read_sources_config(&config)?;
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
    )
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
    let ledger = paths.read_ledger()?;
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
                        locator: Locator::Git {
                            url: record.source_url.clone(),
                        },
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
            let ledger_state = paths.read_ledger()?;
            if install_v1::item_status(paths, &ledger_state, Some(item), &item.id)
                != ItemStatus::UpdateAvailable
            {
                continue;
            }
            match install_v1::install_item_approved(
                paths,
                &source.definition,
                snapshot,
                item,
                false,
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
) -> Result<AppState, String> {
    let ledger_state = paths.read_ledger()?;
    let mut current_ids = BTreeSet::new();
    let mut items = Vec::new();
    let mut sources = Vec::new();
    let configured_locators = loaded
        .iter()
        .map(|source| {
            (
                source.definition.locator.kind(),
                source.definition.locator.identity_key().to_string(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut repository_states = repositories
        .iter()
        .map(|repository| repository_state(repository, &configured_locators, checked))
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
            locator_kind: loaded_source.definition.locator.kind(),
            repository_key: loaded_source.definition.repository_key.clone(),
            built_in: loaded_source.definition.source_key == BUILT_IN_SOURCE_KEY,
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
                locator: Locator::Git {
                    url: record.source_url.clone(),
                },
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
        repositories: repository_states,
        sources,
        items,
        agent_profiles: agent_profiles::states(paths)?,
    })
}

fn repository_state(
    loaded: &LoadedRepository,
    configured_locators: &BTreeSet<(LocatorKind, String)>,
    checked: u64,
) -> RepositoryState {
    let listed = loaded
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.manifest.canonical_sources().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|source| {
            let already_added = configured_locators.contains(&(
                source.locator.kind(),
                source.locator.identity_key().to_string(),
            ));
            ListedSourceState {
                name: source.name,
                description: source.description,
                locator_kind: source.locator.kind(),
                url: source.locator.url().to_string(),
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
        locator_kind: loaded.definition.locator.kind(),
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
    let mut status = install_v1::item_status(paths, ledger_state, Some(item), &item.id);
    if status == ItemStatus::Installed
        && plan.as_ref().is_some_and(|plan| {
            !crate::executor::plan_satisfied(ledger_state, plan).unwrap_or(false)
        })
    {
        status = ItemStatus::PartiallyInstalled;
    }
    Ok(CatalogItemState {
        id: item.id.clone(),
        local_id: item.local_id.clone(),
        source_id: source.source_id.clone(),
        source_key: source.source_key.clone(),
        source_name: source.name.clone(),
        source_url: source.url().to_string(),
        locator_kind: source.locator.kind(),
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
            })
            .collect(),
        compatibility,
        destination: match ledger_state.items.get(&item.id) {
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
        locator_kind: source.locator.kind(),
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
    kind: LocatorKind,
    url: &str,
    repository_key: Option<String>,
) -> Result<PreparedSource, String> {
    let _guard = runtime.operation_lock.lock().await;
    let locator = Locator::parse(kind, url)?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let config_file = source_v1::read_sources_config(&config)?;
    let expected_source_id = match &repository_key {
        Some(repository_key) => {
            let repository = config_file
                .repositories
                .iter()
                .find(|repository| repository.repository_key == *repository_key)
                .ok_or_else(|| "That source repository is no longer configured.".to_string())?;
            let snapshot = source_v1::load_current_repository(&cache, repository)?
                .ok_or_else(|| "That source repository has no validated revision.".to_string())?;
            let listed = snapshot.manifest.canonical_sources()?;
            let listing = listed
                .iter()
                .find(|source| source.locator.same_identity(&locator))
                .ok_or_else(|| {
                    "That source is no longer listed by the source repository.".to_string()
                })?;
            listing.source_id.clone()
        }
        None => None,
    };
    let repository_key_for_prep = repository_key.clone();
    let candidate = run_blocking("Source preparation", move || {
        let candidate = source_v1::prepare_new_source(
            &locator,
            &cache,
            repository_key_for_prep,
            expected_source_id.as_deref(),
        )?;
        if candidate.definition.is_built_in()
            || (matches!(candidate.definition.locator, Locator::Git { .. })
                && repository_url_key(candidate.definition.url())
                    == repository_url_key(CATALOG_SOURCE))
        {
            source_v1::discard_candidate(&candidate);
            return Err("Use Add default source for the built-in Skillbook source.".to_string());
        }
        Ok(candidate)
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
        locator_kind: candidate.definition.locator.kind(),
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

pub(crate) async fn add_default_source(runtime: &RuntimeState) -> Result<AppState, String> {
    let _guard = runtime.operation_lock.lock().await;
    let config = config_base_dir()?;
    let mut config_file = source_v1::read_sources_config(&config)?;
    if config_file
        .sources
        .iter()
        .any(ConfiguredSource::is_built_in)
    {
        return Err("The default Skillbook source is already configured.".to_string());
    }
    config_file.sources.push(ConfiguredSource::built_in());
    source_v1::write_sources_config(&config, &config_file)?;
    drop(_guard);
    sync_app_state(runtime).await
}

pub(crate) async fn prepare_source_repository(
    runtime: &RuntimeState,
    kind: LocatorKind,
    url: &str,
) -> Result<PreparedRepository, String> {
    let _guard = runtime.operation_lock.lock().await;
    let locator = Locator::parse(kind, url)?;
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
        locator_kind: candidate.definition.locator.kind(),
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
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let (paths, source, snapshot, item) = item_context(source_id, local_id)?;
    install_v1::install_item_approved(&paths, &source, &snapshot, &item, trust_approved)
}

pub(crate) async fn replace_item(
    runtime: &RuntimeState,
    source_id: &str,
    local_id: &str,
    trust_approved: bool,
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let (paths, source, snapshot, item) = item_context(source_id, local_id)?;
    install_v1::replace_item_approved(&paths, &source, &snapshot, &item, trust_approved)
}

pub(crate) async fn preview_install(
    runtime: &RuntimeState,
    source_id: &str,
    local_id: &str,
) -> Result<crate::planner::InstallPreview, String> {
    let _guard = runtime.operation_lock.lock().await;
    let (paths, _source, snapshot, item) = item_context(source_id, local_id)?;
    let plan = crate::planner::plan_install(&paths, &snapshot, &item)?;
    Ok(crate::planner::preview(&item, &plan))
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
    let packages = installed_v2_contexts(&paths)?
        .iter()
        .map(|(_, snapshot, item)| {
            let plan =
                crate::planner::plan_install_with_profiles(&paths, snapshot, item, &profiles)?;
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
    let installed = paths
        .read_ledger()?
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
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let paths = SystemPaths::from_system()?;
    let config = config_base_dir()?;
    let source = source_v1::configured_source(&config, source_id)?;
    install_v1::uninstall_item(&paths, &source, &format!("{source_id}/{local_id}"), false)
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
    let ledger_state = paths.read_ledger()?;
    let entries = snapshot
        .catalog
        .items
        .values()
        .map(|item| {
            let mut status = install_v1::item_status(&paths, &ledger_state, Some(item), &item.id);
            if status == ItemStatus::Installed
                && crate::planner::plan_install(&paths, &snapshot, item)
                    .ok()
                    .is_some_and(|plan| {
                        !crate::executor::plan_satisfied(&ledger_state, &plan).unwrap_or(false)
                    })
            {
                status = ItemStatus::PartiallyInstalled;
            }
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
    let records = paths
        .read_ledger()?
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
        && !paths
            .read_ledger()?
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
            Ok(state) => crate::ipc_v1::ScheduledSync::Updated {
                state: Box::new(state),
            },
            Err(message) => crate::ipc_v1::ScheduledSync::Failed { message },
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
                crate::ipc_v1::ScheduledSync::Updated {
                    state: Box::new(state),
                },
            );
        }
    });
}
