//! Application service for source synchronization and file installation.

use crate::catalog_v1::CatalogItem;
use crate::install_v1::{self, AnchorPaths, ItemStatus, OperationOutcome, SourceRemovalPlan};
use crate::ipc_v1::{
    AppState, AutoUpdateReport, BulkFailure, BulkPlan, BulkPlanEntry, BulkResult, CatalogItemState,
    DestinationState, ItemFailure, ItemReference, PreparedSource, SourceState, SourceStatus,
};
use crate::ledger::{self, InstallationRecord};
use crate::source_v1::{
    self, ConfiguredSource, SourceCandidate, SourceSnapshot, BUILT_IN_SOURCE_KEY, CATALOG_SOURCE,
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
}

impl RuntimeState {
    pub(crate) fn new() -> Result<Self, String> {
        Ok(Self {
            operation_lock: Mutex::new(()),
            sync_lock: Mutex::new(()),
            pending_sources: Mutex::new(BTreeMap::new()),
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
        let anchors = AnchorPaths::from_system()?;
        let cache = cache_base_dir()?;
        let config = config_base_dir()?;
        let checked = current_epoch_seconds();
        let loaded = source_v1::read_sources(&config)?
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
        build_app_state(&anchors, &loaded, checked, AutoUpdateReport::default()).map(Some)
    })
    .await
}

pub(crate) async fn sync_app_state(runtime: &RuntimeState) -> Result<AppState, String> {
    let _sync_guard = runtime.sync_lock.lock().await;
    let _operation_guard = runtime.operation_lock.lock().await;
    run_blocking("Source synchronization", synchronize).await
}

fn synchronize() -> Result<AppState, String> {
    let anchors = AnchorPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let checked = current_epoch_seconds();
    let definitions = source_v1::read_sources(&config)?;
    let mut claimed = definitions
        .iter()
        .map(|source| (source.source_id.clone(), source.source_key.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut updated_definitions = Vec::with_capacity(definitions.len());
    let mut loaded = Vec::with_capacity(definitions.len());

    for definition in definitions {
        match source_v1::prepare_refresh(&definition, &cache) {
            Ok(candidate) => {
                let source_id_changed = candidate.definition.source_id != definition.source_id;
                let duplicate_namespace = claimed
                    .get(&candidate.definition.source_id)
                    .is_some_and(|source_key| source_key != &definition.source_key);
                if source_id_changed || duplicate_namespace {
                    let message = if source_id_changed {
                        format!(
                            "The repository changed source.id from {} to {}. The last validated commit remains active.",
                            definition.source_id, candidate.definition.source_id
                        )
                    } else {
                        format!(
                            "The namespace {} is already claimed by another source.",
                            candidate.definition.source_id
                        )
                    };
                    source_v1::discard_candidate(&candidate);
                    let snapshot = source_v1::load_current(&cache, &definition).ok().flatten();
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
                match source_v1::activate_candidate(&cache, candidate) {
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
                        &cache,
                        definition,
                        message,
                        &mut updated_definitions,
                        &mut loaded,
                    ),
                }
            }
            Err(message) => push_refresh_error(
                &cache,
                definition,
                message,
                &mut updated_definitions,
                &mut loaded,
            ),
        }
    }
    source_v1::write_sources(&config, &updated_definitions)?;
    let report = reconcile_installed_items(&anchors, &loaded)?;
    build_app_state(&anchors, &loaded, checked, report)
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
    anchors: &AnchorPaths,
    loaded: &[LoadedSource],
) -> Result<AutoUpdateReport, String> {
    let mut report = AutoUpdateReport::default();
    for source in loaded {
        let Some(snapshot) = &source.snapshot else {
            continue;
        };
        for item in snapshot.catalog.items.values() {
            let ledger_state = ledger::read(&anchors.app_data())?;
            if install_v1::item_status(anchors, &ledger_state, Some(item), &item.id)
                != ItemStatus::UpdateAvailable
            {
                continue;
            }
            match install_v1::install_item(anchors, &source.definition, snapshot, item) {
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
    anchors: &AnchorPaths,
    loaded: &[LoadedSource],
    checked: u64,
    report: AutoUpdateReport,
) -> Result<AppState, String> {
    let ledger_state = ledger::read(&anchors.app_data())?;
    let mut current_ids = BTreeSet::new();
    let mut items = Vec::new();
    let mut sources = Vec::new();
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
            url: loaded_source.definition.url.clone(),
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
                    anchors,
                    &ledger_state,
                    &loaded_source.definition,
                    item,
                )?);
            }
        }
    }
    for (id, record) in &ledger_state.items {
        if current_ids.contains(id) {
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
                url: record.source_url.clone(),
            });
        items.push(removed_item_state(
            anchors,
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
    Ok(AppState {
        checked_at_epoch_seconds: checked,
        auto_update_report: report,
        sources,
        items,
    })
}

fn current_item_state(
    anchors: &AnchorPaths,
    ledger_state: &ledger::InstallationLedger,
    source: &ConfiguredSource,
    item: &CatalogItem,
) -> Result<CatalogItemState, String> {
    Ok(CatalogItemState {
        id: item.id.clone(),
        local_id: item.local_id.clone(),
        source_id: source.source_id.clone(),
        source_key: source.source_key.clone(),
        source_name: source.name.clone(),
        source_url: source.url.clone(),
        name: item.name.clone(),
        description: item.description.clone(),
        source: item.source.clone(),
        destination: DestinationState {
            anchor: item.destination.anchor,
            path: anchors.resolve(&item.destination)?.display().to_string(),
        },
        status: install_v1::item_status(anchors, ledger_state, Some(item), &item.id),
    })
}

fn removed_item_state(
    anchors: &AnchorPaths,
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
        source: record.source.clone(),
        destination: DestinationState {
            anchor: record.destination.anchor,
            path: anchors
                .resolve_owned(&record.destination)?
                .display()
                .to_string(),
        },
        status: install_v1::item_status(anchors, ledger_state, None, id),
    })
}

pub(crate) async fn prepare_source(
    runtime: &RuntimeState,
    url: &str,
) -> Result<PreparedSource, String> {
    let _guard = runtime.operation_lock.lock().await;
    let url = url.to_string();
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let configured = source_v1::read_sources(&config)?;
    let candidate = run_blocking("Source preparation", move || {
        let candidate = source_v1::prepare_new_source(&url, &cache)?;
        if repository_url_key(&candidate.definition.url) == repository_url_key(CATALOG_SOURCE) {
            source_v1::discard_candidate(&candidate);
            return Err("Use Add default source for the built-in Skillbook source.".to_string());
        }
        Ok(candidate)
    })
    .await?;
    if configured.iter().any(|source| {
        source.source_key == candidate.definition.source_key
            || repository_url_key(&source.url) == repository_url_key(&candidate.definition.url)
    }) {
        source_v1::discard_candidate(&candidate);
        return Err(format!(
            "{} is already configured.",
            candidate.definition.url
        ));
    }
    if configured
        .iter()
        .any(|source| source.source_id == candidate.definition.source_id)
    {
        source_v1::discard_candidate(&candidate);
        return Err(format!(
            "The namespace {} is already claimed by another URL.",
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
        url: candidate.definition.url.clone(),
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
    let mut sources = source_v1::read_sources(&config)?;
    if sources.iter().any(|source| {
        source.source_key == snapshot.definition.source_key
            || source.source_id == snapshot.definition.source_id
    }) {
        return Err("The source was configured while confirmation was open.".to_string());
    }
    sources.push(snapshot.definition);
    sources.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.source_id.cmp(&right.source_id))
    });
    source_v1::write_sources(&config, &sources)?;
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
    let mut sources = source_v1::read_sources(&config)?;
    if sources.iter().any(ConfiguredSource::is_built_in) {
        return Err("The default Skillbook source is already configured.".to_string());
    }
    sources.push(ConfiguredSource::built_in());
    source_v1::write_sources(&config, &sources)?;
    drop(_guard);
    sync_app_state(runtime).await
}

pub(crate) async fn install_item(
    runtime: &RuntimeState,
    source_id: &str,
    local_id: &str,
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let (anchors, source, snapshot, item) = item_context(source_id, local_id)?;
    install_v1::install_item(&anchors, &source, &snapshot, &item)
}

pub(crate) async fn replace_item(
    runtime: &RuntimeState,
    source_id: &str,
    local_id: &str,
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let (anchors, source, snapshot, item) = item_context(source_id, local_id)?;
    install_v1::replace_item(&anchors, &source, &snapshot, &item)
}

pub(crate) async fn uninstall_item(
    runtime: &RuntimeState,
    source_id: &str,
    local_id: &str,
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let anchors = AnchorPaths::from_system()?;
    let config = config_base_dir()?;
    let source = source_v1::configured_source(&config, source_id)?;
    install_v1::uninstall_item(&anchors, &source, &format!("{source_id}/{local_id}"), false)
}

pub(crate) async fn bulk_plan(
    runtime: &RuntimeState,
    source_id: &str,
    uninstall: bool,
) -> Result<BulkPlan, String> {
    let _guard = runtime.operation_lock.lock().await;
    let anchors = AnchorPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let source = source_v1::configured_source(&config, source_id)?;
    let snapshot = source_v1::load_current(&cache, &source)?
        .ok_or_else(|| format!("{} has no validated revision.", source.source_id))?;
    let ledger_state = ledger::read(&anchors.app_data())?;
    let entries = snapshot
        .catalog
        .items
        .values()
        .map(|item| {
            let status = install_v1::item_status(&anchors, &ledger_state, Some(item), &item.id);
            BulkPlanEntry {
                id: item.id.clone(),
                local_id: item.local_id.clone(),
                status,
                will_run: if uninstall {
                    matches!(status, ItemStatus::Installed | ItemStatus::UpdateAvailable)
                } else {
                    matches!(status, ItemStatus::Available | ItemStatus::UpdateAvailable)
                },
            }
        })
        .collect();
    Ok(BulkPlan {
        source_id: source.source_id,
        uninstall,
        entries,
    })
}

pub(crate) async fn bulk_run(
    runtime: &RuntimeState,
    source_id: &str,
    uninstall: bool,
) -> Result<BulkResult, String> {
    let plan = bulk_plan(runtime, source_id, uninstall).await?;
    let mut completed = Vec::new();
    let mut failures = Vec::new();
    for entry in plan.entries.into_iter().filter(|entry| entry.will_run) {
        let result = if uninstall {
            uninstall_item(runtime, source_id, &entry.local_id).await
        } else {
            install_item(runtime, source_id, &entry.local_id).await
        };
        match result {
            Ok(_) => completed.push(entry.id),
            Err(message) => failures.push(BulkFailure {
                id: entry.id,
                message,
            }),
        }
    }
    Ok(BulkResult {
        completed,
        failures,
    })
}

pub(crate) async fn plan_source_removal(
    runtime: &RuntimeState,
    source_id: &str,
) -> Result<SourceRemovalPlan, String> {
    let _guard = runtime.operation_lock.lock().await;
    let anchors = AnchorPaths::from_system()?;
    let config = config_base_dir()?;
    let source = source_v1::configured_source(&config, source_id)?;
    install_v1::source_removal_plan(&anchors, &source)
}

pub(crate) async fn remove_source(
    runtime: &RuntimeState,
    source_id: &str,
    acknowledge_modified_paths: bool,
) -> Result<BulkResult, String> {
    let _guard = runtime.operation_lock.lock().await;
    let anchors = AnchorPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let source = source_v1::configured_source(&config, source_id)?;
    let plan = install_v1::source_removal_plan(&anchors, &source)?;
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
    let records = ledger::read(&anchors.app_data())?
        .items
        .values()
        .filter(|record| record.source_key == source.source_key)
        .map(|record| format!("{}/{}", record.source_id, record.local_id))
        .collect::<Vec<_>>();
    let mut completed = Vec::new();
    let mut failures = Vec::new();
    for id in records {
        match install_v1::uninstall_item(&anchors, &source, &id, acknowledge_modified_paths) {
            Ok(_) => completed.push(id),
            Err(message) => failures.push(BulkFailure { id, message }),
        }
    }
    if !failures.is_empty() {
        return Ok(BulkResult {
            completed,
            failures,
        });
    }
    let mut sources = source_v1::read_sources(&config)?;
    sources.retain(|configured| configured.source_key != source.source_key);
    source_v1::write_sources(&config, &sources)?;
    source_v1::remove_source_cache(&cache, &source.source_key)?;
    Ok(BulkResult {
        completed,
        failures,
    })
}

fn item_context(
    source_id: &str,
    local_id: &str,
) -> Result<(AnchorPaths, ConfiguredSource, SourceSnapshot, CatalogItem), String> {
    let anchors = AnchorPaths::from_system()?;
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
    Ok((anchors, source, snapshot, item))
}

fn cached_state_now() -> Result<AppState, String> {
    let anchors = AnchorPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let checked = current_epoch_seconds();
    let loaded = source_v1::read_sources(&config)?
        .into_iter()
        .map(|definition| LoadedSource {
            snapshot: source_v1::load_current(&cache, &definition).ok().flatten(),
            definition,
            status: SourceStatus::Cached,
            refresh_failed: false,
            message: None,
        })
        .collect::<Vec<_>>();
    build_app_state(&anchors, &loaded, checked, AutoUpdateReport::default())
}

fn prepared_token(candidate: &SourceCandidate) -> String {
    let mut hasher = Sha256::new();
    hasher.update(candidate.definition.source_key.as_bytes());
    hasher.update(candidate.commit.as_bytes());
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
