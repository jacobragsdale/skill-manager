//! Manifest-driven application service used by the Tauri command layer.

use crate::catalog_v1::{read_manifest_catalog, CatalogItem, ManifestCatalog, AGENT_SKILL_KIND};
use crate::domain::{SourceStatus, CATALOG_SOURCE};
use crate::install::current_epoch_seconds;
use crate::install_v1::{
    self, AnchorPaths, ItemStatus, MigrationResult, OperationOutcome, SourceRemovalPlan,
};
use crate::ipc_v1::{
    ActionState, AgentSkillState, AppState, AutoUpdateReport, BulkFailure, BulkPlan, BulkPlanEntry,
    BulkResult, CatalogItemState, DestinationState, ItemReference, PreparedSource, SourceState,
};
use crate::ledger::{self, InstallationRecord};
pub(crate) use crate::process::OutputCallback;
use crate::source_v1::{
    self, ConfiguredSource, SourceCandidate, SourceSnapshot, BUILT_IN_SOURCE_KEY,
};
use crate::sources::{cache_base_dir, config_base_dir, repository_url_key};
use crate::trust;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
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
    pending_executable: bool,
    message: Option<String>,
}

pub(crate) async fn run_blocking<T, F>(context: &'static str, task: F) -> Result<T, String>
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
    run_blocking("Cached manifest catalog load", || {
        let anchors = AnchorPaths::from_system()?;
        let cache = cache_base_dir()?;
        let config = config_base_dir()?;
        let checked = current_epoch_seconds();
        let sources = source_v1::read_sources(&config, &cache)?;
        let loaded = sources
            .into_iter()
            .map(
                |definition| match source_v1::load_current(&cache, &definition) {
                    Ok(snapshot) => LoadedSource {
                        definition,
                        snapshot,
                        status: SourceStatus::Cached,
                        refresh_failed: false,
                        pending_executable: false,
                        message: None,
                    },
                    Err(message) => LoadedSource {
                        definition,
                        snapshot: None,
                        status: SourceStatus::Error,
                        refresh_failed: true,
                        pending_executable: false,
                        message: Some(message),
                    },
                },
            )
            .collect::<Vec<_>>();
        build_app_state(
            &anchors,
            &config,
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
    run_blocking("Manifest catalog synchronization", synchronize).await
}

fn synchronize() -> Result<AppState, String> {
    let anchors = AnchorPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let checked = current_epoch_seconds();
    let definitions = source_v1::read_sources(&config, &cache)?;
    let mut claimed = definitions
        .iter()
        .filter(|source| !source.source_id.starts_with("pending-"))
        .map(|source| (source.source_id.clone(), source.source_key.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut updated_definitions = Vec::with_capacity(definitions.len());
    let mut loaded = Vec::with_capacity(definitions.len());

    for definition in definitions {
        match source_v1::prepare_refresh(&definition, &cache) {
            Ok(candidate) => {
                let source_id_changed = !definition.source_id.starts_with("pending-")
                    && candidate.definition.source_id != definition.source_id;
                let duplicate_namespace = claimed
                    .get(&candidate.definition.source_id)
                    .is_some_and(|source_key| source_key != &definition.source_key);
                let introduced_execution = !definition.executable
                    && candidate.definition.executable
                    && !trust::is_trusted(&config, &definition.source_key, &definition.url);
                if source_id_changed || duplicate_namespace || introduced_execution {
                    let message = if source_id_changed {
                        format!(
                            "The repository changed source.id from {} to {}. The last validated commit remains active.",
                            definition.source_id, candidate.definition.source_id
                        )
                    } else if duplicate_namespace {
                        format!(
                            "The namespace {} is already claimed by another configured source.",
                            candidate.definition.source_id
                        )
                    } else {
                        "This source introduced executable hooks or actions. Grant executable trust before activating this revision."
                            .to_string()
                    };
                    source_v1::discard_candidate(&candidate);
                    let snapshot = source_v1::load_current(&cache, &definition).ok().flatten();
                    updated_definitions.push(definition.clone());
                    loaded.push(LoadedSource {
                        definition,
                        snapshot,
                        status: SourceStatus::Error,
                        refresh_failed: true,
                        pending_executable: introduced_execution,
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
                            pending_executable: false,
                            message: None,
                        });
                    }
                    Err(message) => {
                        let snapshot = source_v1::load_current(&cache, &definition).ok().flatten();
                        updated_definitions.push(definition.clone());
                        loaded.push(LoadedSource {
                            definition,
                            snapshot,
                            status: SourceStatus::Error,
                            refresh_failed: true,
                            pending_executable: false,
                            message: Some(message),
                        });
                    }
                }
            }
            Err(message) => {
                let snapshot = source_v1::load_current(&cache, &definition).ok().flatten();
                updated_definitions.push(definition.clone());
                loaded.push(LoadedSource {
                    definition,
                    snapshot,
                    status: SourceStatus::Error,
                    refresh_failed: true,
                    pending_executable: false,
                    message: Some(message),
                });
            }
        }
    }
    source_v1::write_sources(&config, &updated_definitions)?;
    let report = reconcile_installed_items(&anchors, &config, &loaded)?;
    build_app_state(&anchors, &config, &loaded, checked, report)
}

fn reconcile_installed_items(
    anchors: &AnchorPaths,
    config: &Path,
    loaded: &[LoadedSource],
) -> Result<AutoUpdateReport, String> {
    let mut report = AutoUpdateReport::default();
    let mut legacy_matches = BTreeMap::<(String, String), usize>::new();
    for source in loaded {
        let Some(snapshot) = &source.snapshot else {
            continue;
        };
        for item in snapshot
            .catalog
            .items
            .values()
            .filter(|item| item.kind == AGENT_SKILL_KIND)
        {
            let Some(mapping) = item.mappings.first() else {
                continue;
            };
            let digest = crate::digest::directory_digest(&snapshot.path.join(&mapping.source))?;
            let local_name = item
                .agent_skill
                .as_ref()
                .map(|metadata| metadata.local_name.clone())
                .unwrap_or_default();
            *legacy_matches.entry((local_name, digest)).or_default() += 1;
        }
    }

    for source in loaded {
        let Some(snapshot) = &source.snapshot else {
            continue;
        };
        let trusted = trust::is_trusted(
            config,
            &source.definition.source_key,
            &source.definition.url,
        );
        for item in snapshot.catalog.items.values() {
            if item.kind == AGENT_SKILL_KIND {
                if let (Some(mapping), Some(metadata)) =
                    (item.mappings.first(), item.agent_skill.as_ref())
                {
                    let digest =
                        crate::digest::directory_digest(&snapshot.path.join(&mapping.source))?;
                    let unique = legacy_matches
                        .get(&(metadata.local_name.clone(), digest))
                        .copied()
                        == Some(1);
                    match install_v1::migrate_legacy_agent_skill(
                        anchors,
                        &source.definition,
                        snapshot,
                        item,
                        unique,
                        trusted,
                    ) {
                        Ok(MigrationResult::Attention(message)) => {
                            report.migration_attention.push(crate::ipc_v1::ItemFailure {
                                id: item.id.clone(),
                                message,
                            })
                        }
                        Ok(MigrationResult::None | MigrationResult::Migrated) => {}
                        Err(message) => report.failed_items.push(crate::ipc_v1::ItemFailure {
                            id: item.id.clone(),
                            message,
                        }),
                    }
                }
            }
        }
        let ledger_state = ledger::read(&anchors.app_data())?;
        for item in snapshot.catalog.items.values() {
            if install_v1::item_status(anchors, &ledger_state, Some(item), &item.id)
                != ItemStatus::UpdateAvailable
            {
                continue;
            }
            if item.hooks.has_commands() && !trusted {
                report.skipped_untrusted_items.push(ItemReference {
                    id: item.id.clone(),
                    source_id: item.source_id.clone(),
                    local_id: item.local_id.clone(),
                });
                continue;
            }
            match install_v1::install_item(
                anchors,
                &source.definition,
                snapshot,
                item,
                trusted,
                Arc::new(|_, _| {}),
            ) {
                Ok(_) => report.updated_items.push(ItemReference {
                    id: item.id.clone(),
                    source_id: item.source_id.clone(),
                    local_id: item.local_id.clone(),
                }),
                Err(message) => report.failed_items.push(crate::ipc_v1::ItemFailure {
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
    config: &Path,
    loaded: &[LoadedSource],
    checked: u64,
    report: AutoUpdateReport,
) -> Result<AppState, String> {
    let ledger_state = ledger::read(&anchors.app_data())?;
    let mut current_ids = BTreeSet::new();
    let mut items = Vec::new();
    let mut sources = Vec::new();
    for loaded_source in loaded {
        let trusted = trust::is_trusted(
            config,
            &loaded_source.definition.source_key,
            &loaded_source.definition.url,
        );
        let catalog_errors = loaded_source
            .snapshot
            .as_ref()
            .map_or_else(Vec::new, |snapshot| snapshot.catalog.errors.clone());
        let source_actions = loaded_source
            .snapshot
            .as_ref()
            .map_or_else(Vec::new, |snapshot| {
                snapshot
                    .catalog
                    .manifest
                    .actions
                    .iter()
                    .map(|action| ActionState {
                        id: format!("{}/@{}", loaded_source.definition.source_id, action.id),
                        local_id: action.id.clone(),
                        name: action.name.clone(),
                        description: action.description.clone(),
                    })
                    .collect()
            });
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
            executable: loaded_source.definition.executable || loaded_source.pending_executable,
            trusted,
            trust_required: (loaded_source.definition.executable
                || loaded_source.pending_executable)
                && !trusted,
            actions: source_actions,
        });
        if let Some(snapshot) = &loaded_source.snapshot {
            for item in snapshot.catalog.items.values() {
                current_ids.insert(item.id.clone());
                items.push(item_state(
                    anchors,
                    &ledger_state,
                    &loaded_source.definition,
                    Some(item),
                    None,
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
                executable: false,
            });
        let retained = retained_item(record).ok();
        items.push(item_state(
            anchors,
            &ledger_state,
            &definition,
            retained.as_ref().map(|(_, item)| item),
            Some((id, record)),
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

fn item_state(
    anchors: &AnchorPaths,
    ledger_state: &ledger::InstallationLedger,
    source: &ConfiguredSource,
    item: Option<&CatalogItem>,
    removed: Option<(&String, &InstallationRecord)>,
) -> Result<CatalogItemState, String> {
    let (
        id,
        local_id,
        name,
        description,
        kind,
        materialized_name,
        agent_skill,
        actions,
        destinations,
    ) = if let Some(item) = item {
        let destinations = item
            .mappings
            .iter()
            .map(|mapping| {
                let path = anchors.resolve(&mapping.destination)?;
                Ok(DestinationState {
                    anchor: mapping.destination.anchor,
                    path: path.display().to_string(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        (
            item.id.clone(),
            item.local_id.clone(),
            item.name.clone(),
            item.description.clone(),
            item.kind.clone(),
            item.materialized_skill_name.clone(),
            item.agent_skill.as_ref().map(AgentSkillState::from),
            item.actions
                .iter()
                .map(|action| ActionState {
                    id: format!("{}@{}", item.id, action.id),
                    local_id: action.id.clone(),
                    name: action.name.clone(),
                    description: action.description.clone(),
                })
                .collect(),
            destinations,
        )
    } else {
        let (id, record) = removed.expect("removed record accompanies absent item");
        (
                id.clone(),
                record.local_id.clone(),
                record
                    .materialized_skill_name
                    .clone()
                    .unwrap_or_else(|| record.local_id.clone()),
                "This item is no longer published by its source. The retained revision remains available for uninstall."
                    .to_string(),
                if record.materialized_skill_name.is_some() {
                    AGENT_SKILL_KIND.to_string()
                } else {
                    "removed".to_string()
                },
                record.materialized_skill_name.clone(),
                None,
                Vec::new(),
                record
                    .destination_roots
                    .iter()
                    .map(|owned| {
                        Ok(DestinationState {
                            anchor: owned.anchor,
                            path: anchors.resolve_owned(owned)?.display().to_string(),
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?,
            )
    };
    let executable = item.is_some_and(|item| item.hooks.has_commands() || !item.actions.is_empty());
    Ok(CatalogItemState {
        status: install_v1::item_status(anchors, ledger_state, item, &id),
        id,
        local_id,
        source_id: source.source_id.clone(),
        source_key: source.source_key.clone(),
        source_name: source.name.clone(),
        source_url: source.url.clone(),
        name,
        description,
        kind,
        materialized_skill_name: materialized_name,
        agent_skill,
        destinations,
        executable,
        actions,
    })
}

fn retained_item(record: &InstallationRecord) -> Result<(ManifestCatalog, CatalogItem), String> {
    let catalog = read_manifest_catalog(Path::new(&record.retained_snapshot), &record.source_key)?;
    let item = catalog
        .items
        .get(&record.local_id)
        .cloned()
        .ok_or_else(|| {
            format!(
                "Retained revision no longer contains {}/{}.",
                record.source_id, record.local_id
            )
        })?;
    Ok((catalog, item))
}

pub(crate) async fn prepare_source(
    runtime: &RuntimeState,
    url: &str,
) -> Result<PreparedSource, String> {
    let _guard = runtime.operation_lock.lock().await;
    let url = url.to_string();
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let configured = source_v1::read_sources(&config, &cache)?;
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
            "The namespace {} is already claimed by another configured URL.",
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
        executable: candidate.definition.executable,
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
    accept_executable_trust: bool,
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
    if candidate.definition.executable && !accept_executable_trust {
        source_v1::discard_candidate(&candidate);
        return Err("Executable source addition was cancelled.".to_string());
    }
    let candidate_for_activation = candidate.clone();
    let snapshot = run_blocking("Prepared source activation", move || {
        source_v1::activate_candidate(&cache, candidate_for_activation)
    })
    .await?;
    if snapshot.definition.executable {
        trust::grant(
            &config,
            &snapshot.definition.source_key,
            &snapshot.definition.url,
        )?;
    }
    let cache = cache_base_dir()?;
    let mut sources = source_v1::read_sources(&config, &cache)?;
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
    let preview = prepare_source(runtime, CATALOG_SOURCE).await;
    match preview {
        Ok(preview) => confirm_source(runtime, &preview.token, false).await,
        Err(error) if error.contains("Add default source") => {
            let cache = cache_base_dir()?;
            let config = config_base_dir()?;
            let mut sources = source_v1::read_sources(&config, &cache)?;
            if sources.iter().any(ConfiguredSource::is_built_in) {
                return Err("The default Skillbook source is already configured.".to_string());
            }
            sources.push(ConfiguredSource::built_in());
            source_v1::write_sources(&config, &sources)?;
            sync_app_state(runtime).await
        }
        Err(error) => Err(error),
    }
}

pub(crate) async fn set_source_trust(
    runtime: &RuntimeState,
    source_id: &str,
    trusted: bool,
) -> Result<AppState, String> {
    let _guard = runtime.operation_lock.lock().await;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let source = source_v1::configured_source(&config, &cache, source_id)?;
    if trusted {
        trust::grant(&config, &source.source_key, &source.url)?;
    } else {
        trust::revoke(&config, &source.source_key)?;
    }
    cached_state_now()
}

pub(crate) async fn install_item(
    runtime: &RuntimeState,
    source_id: &str,
    local_id: &str,
    on_output: OutputCallback,
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let (anchors, config, source, snapshot, item) = item_context(source_id, local_id)?;
    let trusted = trust::is_trusted(&config, &source.source_key, &source.url);
    if item.hooks.has_commands() && !trusted {
        return Err(
            "Executable trust is required before this item can be installed or updated."
                .to_string(),
        );
    }
    install_v1::install_item(&anchors, &source, &snapshot, &item, trusted, on_output)
}

pub(crate) async fn replace_item(
    runtime: &RuntimeState,
    source_id: &str,
    local_id: &str,
    on_output: OutputCallback,
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let (anchors, config, source, snapshot, item) = item_context(source_id, local_id)?;
    let trusted = trust::is_trusted(&config, &source.source_key, &source.url);
    if item.hooks.has_commands() && !trusted {
        return Err("Executable trust is required before this item can be installed.".to_string());
    }
    install_v1::replace_item(&anchors, &source, &snapshot, &item, trusted, on_output)
}

pub(crate) async fn uninstall_item(
    runtime: &RuntimeState,
    source_id: &str,
    local_id: &str,
    on_output: OutputCallback,
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let (anchors, config, source, snapshot, item) = item_context(source_id, local_id)?;
    let trusted = trust::is_trusted(&config, &source.source_key, &source.url);
    if item.hooks.has_commands() && !trusted {
        return Err(
            "Executable trust is required before this item's uninstall hooks can run.".to_string(),
        );
    }
    install_v1::uninstall_item(
        &anchors, &source, &snapshot, &item, false, trusted, on_output,
    )
}

pub(crate) async fn run_item_action(
    runtime: &RuntimeState,
    source_id: &str,
    local_id: &str,
    action_id: &str,
    on_output: OutputCallback,
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let (anchors, config, source, snapshot, item) = item_context(source_id, local_id)?;
    let trusted = trust::is_trusted(&config, &source.source_key, &source.url);
    install_v1::run_item_action(
        &anchors, &source, &snapshot, &item, action_id, trusted, on_output,
    )
}

pub(crate) async fn run_source_action(
    runtime: &RuntimeState,
    source_id: &str,
    action_id: &str,
    on_output: OutputCallback,
) -> Result<OperationOutcome, String> {
    let _guard = runtime.operation_lock.lock().await;
    let anchors = AnchorPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let source = source_v1::configured_source(&config, &cache, source_id)?;
    let snapshot = source_v1::load_current(&cache, &source)?
        .ok_or_else(|| format!("{} has no validated source revision.", source.source_id))?;
    let trusted = trust::is_trusted(&config, &source.source_key, &source.url);
    install_v1::run_source_action(&anchors, &source, &snapshot, action_id, trusted, on_output)
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
    let source = source_v1::configured_source(&config, &cache, source_id)?;
    let snapshot = source_v1::load_current(&cache, &source)?
        .ok_or_else(|| format!("{} has no validated source revision.", source.source_id))?;
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
    on_output: OutputCallback,
) -> Result<BulkResult, String> {
    let plan = bulk_plan(runtime, source_id, uninstall).await?;
    let mut completed = Vec::new();
    let mut failures = Vec::new();
    for entry in plan.entries.into_iter().filter(|entry| entry.will_run) {
        let result = if uninstall {
            uninstall_item(runtime, source_id, &entry.local_id, Arc::clone(&on_output)).await
        } else {
            install_item(runtime, source_id, &entry.local_id, Arc::clone(&on_output)).await
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
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let source = source_v1::configured_source(&config, &cache, source_id)?;
    install_v1::source_removal_plan(&anchors, &source)
}

pub(crate) async fn remove_source(
    runtime: &RuntimeState,
    source_id: &str,
    acknowledge_modified_paths: bool,
    approve_cleanup_execution: bool,
    on_output: OutputCallback,
) -> Result<BulkResult, String> {
    let _guard = runtime.operation_lock.lock().await;
    let anchors = AnchorPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let source = source_v1::configured_source(&config, &cache, source_id)?;
    let plan = install_v1::source_removal_plan(&anchors, &source)?;
    if plan
        .items
        .iter()
        .flat_map(|item| &item.paths)
        .any(|path| path.modified)
        && !acknowledge_modified_paths
    {
        return Err("Source cleanup includes locally modified managed paths. Confirm the path-level warning before continuing.".to_string());
    }
    let trusted = trust::is_trusted(&config, &source.source_key, &source.url);
    let ledger_state = ledger::read(&anchors.app_data())?;
    let records = ledger_state
        .items
        .values()
        .filter(|record| record.source_key == source.source_key)
        .cloned()
        .collect::<Vec<_>>();
    let mut completed = Vec::new();
    let mut failures = Vec::new();
    for record in records {
        let id = format!("{}/{}", record.source_id, record.local_id);
        match retained_snapshot(&source, &record).and_then(|(snapshot, item)| {
            if item.hooks.has_commands() && !(trusted || approve_cleanup_execution) {
                return Err(
                    "Cleanup hooks require executable trust or one-time cleanup approval."
                        .to_string(),
                );
            }
            install_v1::uninstall_item(
                &anchors,
                &source,
                &snapshot,
                &item,
                true,
                trusted || approve_cleanup_execution,
                Arc::clone(&on_output),
            )
        }) {
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
    let mut sources = source_v1::read_sources(&config, &cache)?;
    sources.retain(|configured| configured.source_key != source.source_key);
    source_v1::write_sources(&config, &sources)?;
    trust::revoke(&config, &source.source_key)?;
    source_v1::remove_source_cache(&cache, &source.source_key)?;
    Ok(BulkResult {
        completed,
        failures,
    })
}

fn item_context(
    source_id: &str,
    local_id: &str,
) -> Result<
    (
        AnchorPaths,
        PathBuf,
        ConfiguredSource,
        SourceSnapshot,
        CatalogItem,
    ),
    String,
> {
    let anchors = AnchorPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let source = source_v1::configured_source(&config, &cache, source_id)?;
    if let Some(snapshot) = source_v1::load_current(&cache, &source)? {
        if let Some(item) = snapshot.catalog.items.get(local_id).cloned() {
            return Ok((anchors, config, source, snapshot, item));
        }
    }
    let ledger_state = ledger::read(&anchors.app_data())?;
    let id = format!("{source_id}/{local_id}");
    let record = ledger_state
        .items
        .get(&id)
        .ok_or_else(|| format!("Unknown catalog item: {id}"))?;
    let (snapshot, item) = retained_snapshot(&source, record)?;
    Ok((anchors, config, source, snapshot, item))
}

fn retained_snapshot(
    source: &ConfiguredSource,
    record: &InstallationRecord,
) -> Result<(SourceSnapshot, CatalogItem), String> {
    let path = PathBuf::from(&record.retained_snapshot);
    let catalog = read_manifest_catalog(&path, &record.source_key)?;
    let item = catalog
        .items
        .get(&record.local_id)
        .cloned()
        .ok_or_else(|| {
            format!(
                "Retained revision does not contain {}/{}.",
                record.source_id, record.local_id
            )
        })?;
    let snapshot = SourceSnapshot {
        definition: source.clone(),
        commit: record.commit.clone(),
        path,
        catalog,
    };
    Ok((snapshot, item))
}

fn cached_state_now() -> Result<AppState, String> {
    let anchors = AnchorPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let checked = current_epoch_seconds();
    let loaded = source_v1::read_sources(&config, &cache)?
        .into_iter()
        .map(|definition| {
            let snapshot = source_v1::load_current(&cache, &definition).ok().flatten();
            LoadedSource {
                definition,
                snapshot,
                status: SourceStatus::Cached,
                refresh_failed: false,
                pending_executable: false,
                message: None,
            }
        })
        .collect::<Vec<_>>();
    build_app_state(
        &anchors,
        &config,
        &loaded,
        checked,
        AutoUpdateReport::default(),
    )
}

fn prepared_token(candidate: &SourceCandidate) -> String {
    let mut hasher = Sha256::new();
    hasher.update(candidate.definition.source_key.as_bytes());
    hasher.update(candidate.commit.as_bytes());
    hasher.update(current_epoch_seconds().to_le_bytes());
    let digest = hasher.finalize();
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
