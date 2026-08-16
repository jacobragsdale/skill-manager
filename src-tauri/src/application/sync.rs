use super::{current_epoch_seconds, run_blocking, LoadedRepository, LoadedSource, RuntimeState};
use crate::agent_profiles;
use crate::app_state::{AppState, AutoUpdateReport, ItemFailure, ItemReference, SourceStatus};
use crate::install::{self, ItemStatus};
use crate::ledger::InstallationRecord;
use crate::locator::{default_catalog_locator, Locator};
use crate::paths::SystemPaths;
use crate::source::{self, ConfiguredRepository, ConfiguredSource, SourcesConfig};
use crate::sources::{cache_base_dir, config_base_dir};
use std::collections::BTreeMap;

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
        let config_file = source::read_sources_config(&config)?;
        let repositories = config_file
            .repositories
            .into_iter()
            .map(
                |definition| match source::load_current_repository(&cache, &definition) {
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
                |definition| match source::load_current(&cache, &definition) {
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
        super::project::build_app_state(
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

pub(super) fn synchronize() -> Result<AppState, String> {
    let paths = SystemPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let checked = current_epoch_seconds();
    let mut config_file = source::read_sources_config(&config)?;
    let catalog_message = ensure_default_catalog(&cache, &mut config_file.repositories);
    let (updated_repositories, loaded_repositories) =
        refresh_repositories(&cache, config_file.repositories);
    let (updated_sources, loaded_sources) = refresh_sources(&cache, config_file.sources);
    source::write_sources_config(
        &config,
        &SourcesConfig {
            repositories: updated_repositories,
            sources: updated_sources,
        },
    )?;
    retire_unsupported_legacy_installs(&paths)?;
    agent_profiles::apply_detected_defaults(&paths)?;
    let report = reconcile_installed_items(&paths, &loaded_sources)?;
    super::project::build_app_state(
        &paths,
        &loaded_repositories,
        &loaded_sources,
        checked,
        report,
        catalog_message,
    )
}

pub(super) fn ensure_default_catalog(
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
    match source::prepare_new_repository(&locator, cache) {
        Ok(candidate) => match source::activate_repository(cache, candidate) {
            Ok(snapshot) => {
                repositories.push(snapshot.definition);
                None
            }
            Err(message) => Some(message),
        },
        Err(message) => Some(message),
    }
}

pub(super) fn refresh_repositories(
    cache: &std::path::Path,
    definitions: Vec<ConfiguredRepository>,
) -> (Vec<ConfiguredRepository>, Vec<LoadedRepository>) {
    let mut updated = Vec::with_capacity(definitions.len());
    let mut loaded = Vec::with_capacity(definitions.len());
    for definition in definitions {
        match source::prepare_repository_refresh(&definition, cache) {
            Ok(candidate) => {
                if candidate.definition.repository_id != definition.repository_id {
                    source::discard_repository(&candidate);
                    let snapshot = source::load_current_repository(cache, &definition)
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
                match source::activate_repository(cache, candidate) {
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
                        let snapshot = source::load_current_repository(cache, &definition)
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
                let snapshot = source::load_current_repository(cache, &definition)
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

pub(super) fn refresh_sources(
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
        match source::prepare_refresh(&definition, cache) {
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
                    source::discard_candidate(&candidate);
                    let snapshot = source::load_current(cache, &definition).ok().flatten();
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
                match source::activate_candidate(cache, candidate) {
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

pub(super) fn is_unsupported_legacy_install(record: &InstallationRecord) -> bool {
    record.manifest_version == 1
        || matches!(
            record.component_kind.as_str(),
            "agentPlugin" | "legacyFileTree" | "fileTree"
        )
}

pub(super) fn retire_unsupported_legacy_installs(paths: &SystemPaths) -> Result<(), String> {
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

pub(super) fn push_refresh_error(
    cache: &std::path::Path,
    definition: ConfiguredSource,
    message: String,
    updated_definitions: &mut Vec<ConfiguredSource>,
    loaded: &mut Vec<LoadedSource>,
) {
    let snapshot = source::load_current(cache, &definition).ok().flatten();
    updated_definitions.push(definition.clone());
    loaded.push(LoadedSource {
        definition,
        snapshot,
        status: SourceStatus::Error,
        refresh_failed: true,
        message: Some(message),
    });
}

pub(super) fn reconcile_installed_items(
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
            if super::status::refined_item_status(paths, &ledger_state, snapshot, item, None)
                != ItemStatus::UpdateAvailable
            {
                continue;
            }
            let selected = ledger_state
                .items
                .get(&item.id)
                .map(|record| crate::planner::selected_component_ids(record, item));
            match install::install_item_components_approved(
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
