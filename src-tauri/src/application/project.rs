use super::{current_epoch_seconds, LoadedRepository, LoadedSource};
use crate::agent_profiles;
use crate::app_state::{
    AppState, AutoUpdateReport, CatalogItemState, ComponentState, ListedSourceState,
    RepositoryState, SourceState, SourceStatus,
};
use crate::catalog::{CatalogComponentKind, CatalogItem};
use crate::ledger::{self, InstallationRecord};
use crate::locator::Locator;
use crate::paths::SystemPaths;
use crate::source::{self, ConfiguredSource, SourceSnapshot};
use crate::sources::{cache_base_dir, config_base_dir};
use std::collections::BTreeSet;

pub(super) fn build_app_state(
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
        if current_ids.contains(id) || super::sync::is_unsupported_legacy_install(record) {
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

pub(super) fn repository_state(
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

pub(super) fn current_item_state(
    paths: &SystemPaths,
    ledger_state: &ledger::InstallationLedger,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
) -> Result<CatalogItemState, String> {
    let plan = crate::planner::plan(paths, snapshot, item, None, None).ok();
    let compatibility = plan
        .as_ref()
        .map(|plan| plan.compatibility.clone())
        .unwrap_or_default();
    let record = ledger_state.items.get(&item.id);
    let status =
        super::status::refined_item_status(paths, ledger_state, snapshot, item, plan.as_ref());
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
                description: component.description.clone(),
                manual_invocation: component.disable_model_invocation,
                status: super::status::component_status(
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

pub(super) fn removed_item_state(
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
            description: record.description.clone(),
            manual_invocation: record.disable_model_invocation,
            status: super::status::item_status(paths, ledger_state, None, id),
        }],
        compatibility: Vec::new(),
        destination: Some(
            paths
                .resolve_owned(&record.destination)?
                .display()
                .to_string(),
        ),
        status: super::status::item_status(paths, ledger_state, None, id),
    })
}

pub(super) fn component_kind_label(kind: CatalogComponentKind) -> &'static str {
    match kind {
        CatalogComponentKind::Skill => "skill",
        CatalogComponentKind::McpServer => "mcpServer",
    }
}

pub(super) fn cached_state_now() -> Result<AppState, String> {
    let paths = SystemPaths::from_system()?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let checked = current_epoch_seconds();
    let config_file = source::read_sources_config(&config)?;
    let repositories = config_file
        .repositories
        .into_iter()
        .map(|definition| LoadedRepository {
            snapshot: source::load_current_repository(&cache, &definition)
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
            snapshot: source::load_current(&cache, &definition).ok().flatten(),
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
