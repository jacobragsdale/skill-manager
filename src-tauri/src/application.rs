use crate::catalog::{catalog_contents, validate_skill_name};
#[cfg(test)]
use crate::catalog::{catalog_skills, skill_frontmatter, validate_portable_path_component};
#[cfg(test)]
use crate::digest::directory_digest;
use crate::domain::{
    CatalogContents, InstallOwnership, SourceDefinition, SourceStatus, BUILT_IN_SOURCE_ID,
    CATALOG_SOURCE,
};
#[cfg(test)]
use crate::domain::{CatalogMetadata, InstallMarker, SkillStatus, SourcesConfig, MARKER_FILE};
use crate::install::*;
use crate::ipc::{
    AppState, AutoUpdateReport, BulkInstallFailure, BulkInstallResult, BulkPlan, BulkPlanAction,
    BulkPlanEntry, ReplaceUnmanagedResult, ScheduledSync, Skill,
};
#[cfg(test)]
use crate::ipc::{SkillReference, SourceState};
use crate::sources::*;
use crate::{fs_retry, parallel};
use futures_util::future;
use std::collections::BTreeSet;
#[cfg(test)]
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{
    async_runtime::{self, Mutex},
    AppHandle, Emitter, Manager, Runtime,
};
use tokio::time::{self, MissedTickBehavior};

const SCHEDULED_SYNC_EVENT: &str = "scheduled-sync";
const SCHEDULED_SYNC_INTERVAL: Duration = Duration::from_secs(15 * 60);

type BulkMode = InstallationMode;

pub(crate) struct RuntimeState {
    catalog_lock: Mutex<()>,
    sync_lock: Mutex<()>,
    github_client: reqwest::Client,
}

impl RuntimeState {
    pub(crate) fn new() -> Result<Self, String> {
        Ok(Self {
            catalog_lock: Mutex::new(()),
            sync_lock: Mutex::new(()),
            github_client: github_client()?,
        })
    }
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

fn home_dir() -> Result<PathBuf, String> {
    dirs::home_dir().ok_or_else(|| "Could not find your home directory.".to_string())
}

fn collect_source_skill_state(home: &Path, catalog: &SourceCatalog) -> Vec<Skill> {
    let root = install_root(home);
    let source = &catalog.definition;
    let catalog_skills = catalog.skills.values().collect::<Vec<_>>();
    let statuses = parallel::map(&catalog_skills, |skill| {
        installation_status(&root.join(&skill.name), Some(&skill.digest), &source.id)
    });

    catalog_skills
        .iter()
        .zip(&statuses)
        .map(|(skill, status)| Skill {
            source_id: source.id.clone(),
            source_name: source.name.clone(),
            source_url: source.url.clone(),
            name: skill.name.clone(),
            description: skill.description.clone(),
            status: *status,
        })
        .collect()
}

fn source_name_from_url(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("removed source")
        .trim_end_matches(".git")
        .to_string()
}

/// An installed skill that no source still publishes, or `None` when the
/// directory is not a managed skill at all.
fn removed_skill_at(
    entry: &fs::DirEntry,
    catalogs: &[SourceCatalog],
    available: &BTreeSet<(String, String)>,
) -> Result<Option<Skill>, String> {
    let path = entry.path();
    if !entry
        .file_type()
        .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?
        .is_dir()
    {
        return Ok(None);
    }
    let Some(name) = entry.file_name().to_str().map(str::to_string) else {
        return Ok(None);
    };
    if validate_skill_name(&name).is_err() {
        return Ok(None);
    }

    let (source_id, source_url) = match install_ownership(&path) {
        InstallOwnership::Unmanaged => return Ok(None),
        InstallOwnership::Legacy => (BUILT_IN_SOURCE_ID.to_string(), CATALOG_SOURCE.to_string()),
        InstallOwnership::Managed(marker) => {
            let Some(source_id) = marker_source_id(&marker).map(str::to_string) else {
                return Ok(None);
            };
            (source_id, marker.source)
        }
    };
    if available.contains(&(source_id.clone(), name.clone())) {
        return Ok(None);
    }

    let configured = catalogs
        .iter()
        .find(|catalog| catalog.definition.id == source_id);
    let source_name = configured
        .map(|catalog| catalog.definition.name.clone())
        .unwrap_or_else(|| source_name_from_url(&source_url));
    let (description, status) = if configured.is_some_and(|catalog| catalog.path.is_none()) {
        (
            format!(
                "{source_name} is currently unavailable. The installed copy remains protected."
            ),
            installation_status_without_catalog(&path, &source_id),
        )
    } else {
        (
            format!("This skill is no longer available from {source_name}."),
            installation_status(&path, None, &source_id),
        )
    };

    Ok(Some(Skill {
        source_id,
        source_name,
        source_url,
        name,
        description,
        status,
    }))
}

fn append_removed_skills(
    home: &Path,
    catalogs: &[SourceCatalog],
    skills: &mut Vec<Skill>,
) -> Result<(), String> {
    let root = install_root(home);
    if !root.is_dir() {
        return Ok(());
    }
    let available = catalogs
        .iter()
        .flat_map(|catalog| {
            catalog
                .skills
                .keys()
                .map(|name| (catalog.definition.id.clone(), name.clone()))
        })
        .collect::<BTreeSet<_>>();
    let entries = fs::read_dir(&root)
        .map_err(|error| format!("Could not read {}: {error}", root.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Could not read {}: {error}", root.display()))?;

    // Classifying an installed directory reads its marker and can hash it, so
    // the whole install root is inspected at once.
    let removed = parallel::try_map(&entries, |entry| {
        removed_skill_at(entry, catalogs, &available)
    })?;
    skills.extend(removed.into_iter().flatten());
    Ok(())
}

fn app_state_from_catalogs(
    home: &Path,
    catalogs: &[SourceCatalog],
    checked_at_epoch_seconds: u64,
    auto_update_report: AutoUpdateReport,
) -> Result<AppState, String> {
    let mut skills = Vec::new();
    for catalog in catalogs {
        skills.extend(collect_source_skill_state(home, catalog));
    }
    append_removed_skills(home, catalogs, &mut skills)?;
    skills.sort_by(|left, right| {
        left.source_name
            .cmp(&right.source_name)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.source_id.cmp(&right.source_id))
    });
    Ok(AppState {
        install_root: install_root(home).display().to_string(),
        checked_at_epoch_seconds,
        auto_update_report,
        sources: catalogs
            .iter()
            .map(|catalog| catalog.state.clone())
            .collect(),
        skills,
    })
}

#[cfg(test)]
fn state_at(
    home: &Path,
    catalog: &Path,
    catalog_status: SourceStatus,
    catalog_message: Option<String>,
    catalog_commit: Option<String>,
    checked_at_epoch_seconds: u64,
    auto_update_report: AutoUpdateReport,
) -> Result<AppState, String> {
    let contents = catalog_contents(catalog)?;
    let definition = SourceDefinition::built_in();
    let refresh_failed = catalog_status != SourceStatus::Fresh && catalog_message.is_some();
    let source = SourceState {
        id: definition.id.clone(),
        name: definition.name.clone(),
        url: definition.url.clone(),
        built_in: true,
        status: catalog_status,
        refresh_failed,
        message: catalog_message,
        commit: catalog_commit,
        checked_at_epoch_seconds,
        catalog_errors: contents.errors.clone(),
    };
    app_state_from_catalogs(
        home,
        &[SourceCatalog {
            definition,
            state: source,
            path: Some(catalog.to_path_buf()),
            skills: contents.skills,
        }],
        checked_at_epoch_seconds,
        auto_update_report,
    )
}

async fn prepare_source(
    runtime: &RuntimeState,
    source: &SourceDefinition,
    cache_base: &Path,
) -> Result<PreparedCatalog, String> {
    let source_cache = source_cache_base(cache_base, &source.id);
    let catalog = catalog_dir(&source_cache);
    let source_for_metadata = source.clone();
    let current_metadata = {
        let _catalog_guard = runtime.catalog_lock.lock().await;
        run_blocking("Catalog metadata load", move || {
            Ok(read_catalog_metadata(&catalog, &source_for_metadata))
        })
        .await?
    };

    if source.is_built_in() {
        prepare_catalog_from_github(&runtime.github_client, current_metadata, source_cache).await
    } else {
        let source = source.clone();
        run_blocking("Git catalog refresh", move || {
            prepare_catalog_from_git(&source, current_metadata, &source_cache)
        })
        .await
    }
}

async fn cached_app_state(runtime: &RuntimeState) -> Result<Option<AppState>, String> {
    let _guard = runtime.catalog_lock.lock().await;
    let home = home_dir()?;
    let cache_base = cache_base_dir()?;
    let config_base = config_base_dir()?;
    run_blocking("Cached catalog load", move || {
        migrate_legacy_catalog(&cache_base)?;
        let checked_at = current_epoch_seconds();
        let catalogs = source_definitions(&config_base)
            .into_iter()
            .map(|source| {
                source_catalog_from_disk(
                    source,
                    &cache_base,
                    SourceStatus::Cached,
                    None,
                    None,
                    checked_at,
                )
            })
            .collect::<Vec<_>>();
        app_state_from_catalogs(&home, &catalogs, checked_at, AutoUpdateReport::default()).map(Some)
    })
    .await
}

pub(crate) async fn load_cached_app_state(
    runtime: &RuntimeState,
) -> Result<Option<AppState>, String> {
    cached_app_state(runtime).await
}

async fn synchronize_app_state(runtime: &RuntimeState) -> Result<AppState, String> {
    let _sync_guard = runtime.sync_lock.lock().await;
    let home = home_dir()?;
    let cache_base = cache_base_dir()?;
    let config_base = config_base_dir()?;
    {
        let _catalog_guard = runtime.catalog_lock.lock().await;
        let cache_base = cache_base.clone();
        run_blocking("Catalog cache migration", move || {
            migrate_legacy_catalog(&cache_base)
        })
        .await?;
    }
    let definitions = source_definitions(&config_base);
    let checked_at = current_epoch_seconds();
    // Every source is checked over the network, and a custom source also runs
    // Git. Preparing them together means a slow or unreachable remote delays
    // only itself instead of every source queued behind it. `join_all` keeps
    // the results in configuration order, so the catalog does not reshuffle
    // according to which source answered first.
    let prepared_sources = future::join_all(
        definitions
            .iter()
            .map(|source| prepare_source(runtime, source, &cache_base)),
    )
    .await;

    let mut catalogs = Vec::with_capacity(definitions.len());
    for (source, prepared) in definitions.into_iter().zip(prepared_sources) {
        let _catalog_guard = runtime.catalog_lock.lock().await;
        let cache_for_finalize = cache_base.clone();
        catalogs.push(
            run_blocking("Catalog activation", move || {
                Ok(finalize_prepared_source(
                    source,
                    &cache_for_finalize,
                    prepared,
                    checked_at,
                ))
            })
            .await?,
        );
    }

    let _catalog_guard = runtime.catalog_lock.lock().await;
    run_blocking("Catalog reconciliation", move || {
        let report = reconcile_catalogs(&home, &catalogs)?;
        app_state_from_catalogs(&home, &catalogs, checked_at, report)
    })
    .await
}

pub(crate) async fn sync_app_state(runtime: &RuntimeState) -> Result<AppState, String> {
    synchronize_app_state(runtime).await
}

impl From<PlanAction> for BulkPlanAction {
    fn from(action: PlanAction) -> Self {
        match action {
            PlanAction::Install => Self::Install,
            PlanAction::Update => Self::Update,
            PlanAction::Installed => Self::Installed,
            PlanAction::Uninstall => Self::Uninstall,
            PlanAction::NotInstalled => Self::NotInstalled,
            PlanAction::Adopt => Self::Adopt,
            PlanAction::Conflict => Self::Conflict,
            PlanAction::Modified => Self::Modified,
            PlanAction::SourceConflict => Self::SourceConflict,
        }
    }
}

fn build_bulk_plan(
    home: &Path,
    catalog_path: &Path,
    source: &SourceDefinition,
    mode: BulkMode,
) -> Result<BulkPlan, String> {
    let catalog = catalog_contents(catalog_path)?;
    let plan = plan_from_catalog(home, &catalog, source, mode)?;
    Ok(BulkPlan {
        source_id: plan.source_id,
        has_conflicts: plan.has_conflicts,
        entries: plan
            .changes
            .into_iter()
            .map(|change| BulkPlanEntry {
                name: change.name,
                action: change.action.into(),
            })
            .collect(),
    })
}

fn plan_from_catalog(
    home: &Path,
    catalog: &CatalogContents,
    source: &SourceDefinition,
    mode: BulkMode,
) -> Result<crate::install::InstallationPlan, String> {
    plan_source(catalog, source, mode, |skill, destination| {
        installation_status(&destination.resolve(home), Some(&skill.digest), &source.id)
    })
}

fn execute_bulk_plan(
    home: &Path,
    catalog_path: &Path,
    source: &SourceDefinition,
    mode: BulkMode,
) -> Result<BulkInstallResult, String> {
    let catalog = catalog_contents(catalog_path)?;
    let plan = plan_from_catalog(home, &catalog, source, mode)?;
    let result = execute_plan(plan, mode, |change| match mode {
        BulkMode::Install => catalog
            .skills
            .get(&change.name)
            .ok_or_else(|| format!("Unknown skill: {}", change.name))
            .and_then(|skill| install_catalog_skill_at(home, catalog_path, source, skill)),
        BulkMode::Uninstall => uninstall_at_source(home, &source.id, &change.name),
    })?;

    Ok(BulkInstallResult {
        completed: result
            .completed
            .into_iter()
            .map(|change| BulkPlanEntry {
                name: change.name,
                action: change.action.into(),
            })
            .collect(),
        failures: result
            .failures
            .into_iter()
            .map(|failure| BulkInstallFailure {
                name: failure.name,
                message: failure.message,
            })
            .collect(),
    })
}

fn bulk_context(source_id: &str) -> Result<(PathBuf, SourceDefinition, PathBuf), String> {
    let home = home_dir()?;
    let cache_base = cache_base_dir()?;
    let source = configured_source(&config_base_dir()?, source_id)?;
    let catalog = catalog_dir(&source_cache_base(&cache_base, source_id));
    Ok((home, source, catalog))
}

pub(crate) async fn plan_install_all(
    runtime: &RuntimeState,
    source_id: &str,
) -> Result<BulkPlan, String> {
    let _guard = runtime.catalog_lock.lock().await;
    let (home, source, catalog) = bulk_context(source_id)?;
    run_blocking("Bulk installation plan", move || {
        build_bulk_plan(&home, &catalog, &source, BulkMode::Install)
    })
    .await
}

pub(crate) async fn install_all(
    runtime: &RuntimeState,
    source_id: &str,
) -> Result<BulkInstallResult, String> {
    let _guard = runtime.catalog_lock.lock().await;
    let (home, source, catalog) = bulk_context(source_id)?;
    run_blocking("Bulk installation", move || {
        execute_bulk_plan(&home, &catalog, &source, BulkMode::Install)
    })
    .await
}

pub(crate) async fn plan_uninstall_all(
    runtime: &RuntimeState,
    source_id: &str,
) -> Result<BulkPlan, String> {
    let _guard = runtime.catalog_lock.lock().await;
    let (home, source, catalog) = bulk_context(source_id)?;
    run_blocking("Bulk removal plan", move || {
        build_bulk_plan(&home, &catalog, &source, BulkMode::Uninstall)
    })
    .await
}

pub(crate) async fn uninstall_all(
    runtime: &RuntimeState,
    source_id: &str,
) -> Result<BulkInstallResult, String> {
    let _guard = runtime.catalog_lock.lock().await;
    let (home, source, catalog) = bulk_context(source_id)?;
    run_blocking("Bulk removal", move || {
        execute_bulk_plan(&home, &catalog, &source, BulkMode::Uninstall)
    })
    .await
}

pub(crate) async fn install_skill(
    runtime: &RuntimeState,
    source_id: &str,
    name: &str,
) -> Result<(), String> {
    let _guard = runtime.catalog_lock.lock().await;
    let home = home_dir()?;
    let cache_base = cache_base_dir()?;
    let source = configured_source(&config_base_dir()?, source_id)?;
    let catalog = catalog_dir(&source_cache_base(&cache_base, source_id));
    let name = name.to_string();
    run_blocking("Skill installation", move || {
        install_at_source(&home, &catalog, &source, &name)
    })
    .await
}

pub(crate) async fn adopt_skill(
    runtime: &RuntimeState,
    source_id: &str,
    name: &str,
) -> Result<(), String> {
    let _guard = runtime.catalog_lock.lock().await;
    let home = home_dir()?;
    let cache_base = cache_base_dir()?;
    let source = configured_source(&config_base_dir()?, source_id)?;
    let catalog = catalog_dir(&source_cache_base(&cache_base, source_id));
    let name = name.to_string();
    run_blocking("Skill adoption", move || {
        adopt_at_source(&home, &catalog, &source, &name)
    })
    .await
}

pub(crate) async fn replace_unmanaged_skill(
    runtime: &RuntimeState,
    source_id: &str,
    name: &str,
) -> Result<ReplaceUnmanagedResult, String> {
    let _guard = runtime.catalog_lock.lock().await;
    let home = home_dir()?;
    let cache_base = cache_base_dir()?;
    let source = configured_source(&config_base_dir()?, source_id)?;
    let catalog = catalog_dir(&source_cache_base(&cache_base, source_id));
    let name = name.to_string();
    run_blocking("Unmanaged skill replacement", move || {
        replace_unmanaged_at_source(&home, &catalog, &source, &name)
    })
    .await
}

pub(crate) async fn uninstall_skill(
    runtime: &RuntimeState,
    source_id: &str,
    name: &str,
) -> Result<(), String> {
    let _guard = runtime.catalog_lock.lock().await;
    let home = home_dir()?;
    let source_id = source_id.to_string();
    let name = name.to_string();
    run_blocking("Skill removal", move || {
        uninstall_at_source(&home, &source_id, &name)
    })
    .await
}

pub(crate) async fn add_source(runtime: &RuntimeState, url: &str) -> Result<AppState, String> {
    let identity = validate_repository_url(url)?;
    let source = SourceDefinition {
        id: identity.source_key,
        name: identity.display_name,
        url: identity.canonical_url,
    };

    let (config_base, previous_sources, source_cache) = {
        let _sync_guard = runtime.sync_lock.lock().await;
        let config_base = config_base_dir()?;
        let cache_base = cache_base_dir()?;
        let mut sources = read_sources_config(&config_base)?;
        let previous_sources = sources.clone();
        if repository_url_key(&source.url) == repository_url_key(CATALOG_SOURCE) {
            return Err(
                "Use Add default source to configure the built-in skillbook source.".to_string(),
            );
        }
        if sources.iter().any(|existing| existing.url == source.url) {
            return Err(format!("{} is already configured.", source.url));
        }
        if sources.iter().any(|existing| existing.id == source.id) {
            return Err("The repository conflicts with an existing source identifier.".to_string());
        }

        let source_cache = source_cache_base(&cache_base, &source.id);
        let source_for_prepare = source.clone();
        let cache_for_prepare = source_cache.clone();
        let prepared = run_blocking("Git source validation", move || {
            prepare_catalog_from_git(&source_for_prepare, None, &cache_for_prepare)
        })
        .await?;

        {
            let _catalog_guard = runtime.catalog_lock.lock().await;
            let activation_result = run_blocking("Git source activation", {
                let source_cache = source_cache.clone();
                move || match prepared {
                    PreparedCatalog::Current { .. } => Ok(()),
                    PreparedCatalog::Staged { path, .. } => {
                        let result = activate_catalog(&path, &source_cache);
                        if path.exists() {
                            let _ = fs_retry::remove_dir_all(&path);
                        }
                        result
                    }
                }
            })
            .await;
            activation_result?;
        }

        sources.push(source.clone());
        sources.sort_by(|left, right| {
            right
                .is_built_in()
                .cmp(&left.is_built_in())
                .then_with(|| left.id.cmp(&right.id))
        });
        if let Err(error) = write_sources_config(&config_base, &sources) {
            let _catalog_guard = runtime.catalog_lock.lock().await;
            let _ = fs_retry::remove_dir_all(&source_cache);
            return Err(error);
        }
        (config_base, previous_sources, source_cache)
    };

    match synchronize_app_state(runtime).await {
        Ok(state) => Ok(state),
        Err(sync_error) => {
            let _sync_guard = runtime.sync_lock.lock().await;
            if let Err(rollback_error) = write_sources_config(&config_base, &previous_sources) {
                return Err(format!(
                    "The source was added, but the refreshed app state failed ({sync_error}) and the source registration could not be rolled back ({rollback_error})."
                ));
            }
            let _catalog_guard = runtime.catalog_lock.lock().await;
            if let Err(error) = fs_retry::remove_dir_all(&source_cache) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "The failed source addition was rolled back, but {} could not be removed: {error}",
                        source_cache.display()
                    );
                }
            }
            Err(format!(
                "The source could not be added because the refreshed app state failed: {sync_error}"
            ))
        }
    }
}

pub(crate) async fn add_default_source(runtime: &RuntimeState) -> Result<AppState, String> {
    let (config_base, previous_sources) = {
        let _sync_guard = runtime.sync_lock.lock().await;
        let config_base = config_base_dir()?;
        let mut sources = read_sources_config(&config_base)?;
        if sources.iter().any(SourceDefinition::is_built_in) {
            return Err("The default skillbook source is already configured.".to_string());
        }
        let previous_sources = sources.clone();
        sources.insert(0, SourceDefinition::built_in());
        write_sources_config(&config_base, &sources)?;
        (config_base, previous_sources)
    };

    match synchronize_app_state(runtime).await {
        Ok(state) => Ok(state),
        Err(sync_error) => {
            let _sync_guard = runtime.sync_lock.lock().await;
            if let Err(rollback_error) = write_sources_config(&config_base, &previous_sources) {
                return Err(format!(
                    "The default source was added, but the refreshed app state failed ({sync_error}) and the source registration could not be rolled back ({rollback_error})."
                ));
            }
            Err(format!(
                "The default source could not be added because the refreshed app state failed: {sync_error}"
            ))
        }
    }
}

pub(crate) async fn remove_source(
    runtime: &RuntimeState,
    source_id: &str,
) -> Result<AppState, String> {
    let (config_base, previous_sources, source_cache, cache_backup) = {
        let _sync_guard = runtime.sync_lock.lock().await;
        let config_base = config_base_dir()?;
        let cache_base = cache_base_dir()?;
        let mut sources = read_sources_config(&config_base)?;
        let previous_sources = sources.clone();
        let previous_length = sources.len();
        sources.retain(|source| source.id != source_id);
        if sources.len() == previous_length {
            return Err(format!("Unknown source: {source_id}"));
        }

        let _catalog_guard = runtime.catalog_lock.lock().await;
        let source_cache = source_cache_base(&cache_base, source_id);
        let cache_backup = source_cache
            .parent()
            .map(|parent| temporary_path(parent, &format!("{source_id}-removing")));
        if source_cache.exists() {
            let cache_backup = cache_backup
                .as_ref()
                .ok_or_else(|| "Could not determine the source cache parent.".to_string())?;
            fs_retry::rename(&source_cache, cache_backup).map_err(|error| {
                format!(
                    "Could not stage {} for removal: {error}",
                    source_cache.display()
                )
            })?;
        }

        if let Err(error) = write_sources_config(&config_base, &sources) {
            if let Some(cache_backup) = cache_backup.as_ref().filter(|path| path.exists()) {
                if let Err(restore_error) = fs_retry::rename(cache_backup, &source_cache) {
                    return Err(format!(
                        "{error} The source cache could not be restored: {restore_error}"
                    ));
                }
            }
            return Err(error);
        }

        (
            config_base,
            previous_sources,
            source_cache,
            cache_backup.filter(|path| path.exists()),
        )
    };

    match synchronize_app_state(runtime).await {
        Ok(state) => {
            if let Some(cache_backup) = cache_backup {
                let _catalog_guard = runtime.catalog_lock.lock().await;
                if let Err(error) = fs_retry::remove_dir_all(&cache_backup) {
                    eprintln!(
                        "The source was removed, but its staged cache {} could not be deleted: {error}",
                        cache_backup.display()
                    );
                }
            }
            Ok(state)
        }
        Err(state_error) => {
            let _sync_guard = runtime.sync_lock.lock().await;
            let _catalog_guard = runtime.catalog_lock.lock().await;
            if let Some(cache_backup) = cache_backup.as_ref() {
                if let Err(restore_error) = fs_retry::rename(cache_backup, &source_cache) {
                    return Err(format!(
                        "The source removal could not produce an app state ({state_error}), and its cache could not be restored ({restore_error})."
                    ));
                }
            }
            if let Err(rollback_error) = write_sources_config(&config_base, &previous_sources) {
                return Err(format!(
                    "The source removal could not produce an app state ({state_error}), and its registration could not be restored ({rollback_error})."
                ));
            }
            Err(format!(
                "The source was not removed because the refreshed app state failed: {state_error}"
            ))
        }
    }
}

async fn sync_and_publish<R: Runtime>(app: &AppHandle<R>) {
    let outcome = {
        let runtime = app.state::<RuntimeState>();
        synchronize_app_state(runtime.inner()).await
    };
    let event = match outcome {
        Ok(state) => ScheduledSync::Updated {
            state: Box::new(state),
        },
        Err(message) => ScheduledSync::Failed { message },
    };

    if let Err(error) = app.emit_to("main", SCHEDULED_SYNC_EVENT, event) {
        eprintln!("Could not report background catalog sync: {error}");
    }
}

#[cfg(desktop)]
pub(crate) fn spawn_app_sync<R: Runtime>(app: AppHandle<R>) {
    let _sync_task = async_runtime::spawn(async move {
        sync_and_publish(&app).await;
    });
}

pub(crate) async fn run_scheduled_sync<R: Runtime>(app: AppHandle<R>) {
    let mut interval = time::interval(SCHEDULED_SYNC_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval.tick().await;

    loop {
        interval.tick().await;
        sync_and_publish(&app).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::process::Command;
    use tar::{Builder, Header};

    const TEST_COMMIT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn write_skill(catalog: &Path, name: &str, description: &str) {
        let skill = catalog.join(name);
        fs::create_dir_all(&skill).expect("skill directory");
        fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: \"{description}\"\n---\n\n# {name}\n"),
        )
        .expect("skill contents");
    }

    fn test_state(home: &Path, catalog: &Path) -> AppState {
        state_at(
            home,
            catalog,
            SourceStatus::Fresh,
            None,
            None,
            0,
            AutoUpdateReport::default(),
        )
        .expect("state should load")
    }

    #[test]
    fn scheduled_sync_events_match_the_frontend_contract() {
        let failure = serde_json::to_value(ScheduledSync::Failed {
            message: "offline".to_string(),
        })
        .expect("scheduled failure event");
        assert_eq!(
            failure,
            serde_json::json!({ "kind": "failed", "message": "offline" })
        );

        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(catalog.path(), "hello-world", "Test skill");
        let update = serde_json::to_value(ScheduledSync::Updated {
            state: Box::new(test_state(home.path(), catalog.path())),
        })
        .expect("scheduled update event");
        assert_eq!(update["kind"], "updated");
        assert_eq!(update["state"]["sources"][0]["status"], "fresh");
        assert_eq!(update["state"]["skills"][0]["sourceId"], BUILT_IN_SOURCE_ID);
        assert!(update["state"]["skills"].is_array());
    }

    #[test]
    fn source_bulk_plan_blocks_every_change_when_one_skill_needs_attention() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(&catalog.path().join("skills"), "python-standards", "Python");
        write_skill(&catalog.path().join("skills"), "git-ops", "Git");
        let unmanaged = install_root(home.path()).join("python-standards");
        fs::create_dir_all(&unmanaged).expect("unmanaged skill");
        fs::write(unmanaged.join("SKILL.md"), "different").expect("unmanaged contents");

        let plan = build_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Install,
        )
        .expect("bulk plan");
        assert!(plan.has_conflicts);
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(
            plan.entries
                .iter()
                .find(|entry| entry.name == "python-standards")
                .map(|entry| entry.action),
            Some(BulkPlanAction::Conflict)
        );
        assert_eq!(
            plan.entries
                .iter()
                .find(|entry| entry.name == "git-ops")
                .map(|entry| entry.action),
            Some(BulkPlanAction::Install)
        );
        assert!(execute_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Install
        )
        .is_err());
        assert!(!install_root(home.path()).join("git-ops").exists());
    }

    #[test]
    fn source_bulk_install_applies_skills_independently() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(&catalog.path().join("skills"), "python-standards", "Python");
        write_skill(&catalog.path().join("skills"), "git-ops", "Git");
        write_catalog_metadata(
            catalog.path(),
            &CatalogMetadata {
                version: CATALOG_METADATA_VERSION,
                source_id: Some(BUILT_IN_SOURCE_ID.to_string()),
                source: CATALOG_SOURCE.to_string(),
                commit_sha: TEST_COMMIT_SHA.to_string(),
                etag: None,
            },
        )
        .expect("catalog metadata");

        let result = execute_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Install,
        )
        .expect("bulk install");
        assert_eq!(result.completed.len(), 2);
        assert!(result.failures.is_empty());
        assert!(install_root(home.path())
            .join("python-standards/SKILL.md")
            .is_file());
        assert!(install_root(home.path()).join("git-ops/SKILL.md").is_file());

        let installed_plan = build_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Install,
        )
        .expect("installed plan");
        assert!(installed_plan
            .entries
            .iter()
            .all(|entry| entry.action == BulkPlanAction::Installed));
    }

    #[test]
    fn bulk_uninstall_covers_every_skill_in_the_source() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(&catalog.path().join("skills"), "python-standards", "Python");
        write_skill(&catalog.path().join("skills"), "git-ops", "Git");
        write_skill(&catalog.path().join("skills"), "typescript-standards", "TS");
        write_catalog_metadata(
            catalog.path(),
            &CatalogMetadata {
                version: CATALOG_METADATA_VERSION,
                source_id: Some(BUILT_IN_SOURCE_ID.to_string()),
                source: CATALOG_SOURCE.to_string(),
                commit_sha: TEST_COMMIT_SHA.to_string(),
                etag: None,
            },
        )
        .expect("catalog metadata");
        execute_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Install,
        )
        .expect("bulk install");

        let plan = build_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Uninstall,
        )
        .expect("uninstall plan");
        assert!(!plan.has_conflicts);
        assert_eq!(plan.entries.len(), 3);
        assert!(plan
            .entries
            .iter()
            .all(|entry| entry.action == BulkPlanAction::Uninstall));

        let result = execute_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Uninstall,
        )
        .expect("bulk uninstall");
        assert_eq!(result.completed.len(), 3);
        assert!(result.failures.is_empty());
        for name in ["python-standards", "git-ops", "typescript-standards"] {
            assert!(!install_root(home.path()).join(name).exists());
        }
    }

    #[test]
    fn bulk_uninstall_leaves_skills_owned_by_another_source() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(&catalog.path().join("skills"), "python-standards", "Python");
        write_catalog_metadata(
            catalog.path(),
            &CatalogMetadata {
                version: CATALOG_METADATA_VERSION,
                source_id: Some(BUILT_IN_SOURCE_ID.to_string()),
                source: CATALOG_SOURCE.to_string(),
                commit_sha: TEST_COMMIT_SHA.to_string(),
                etag: None,
            },
        )
        .expect("catalog metadata");
        install_at_source(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            "python-standards",
        )
        .expect("install from the built-in source");

        let other = SourceDefinition {
            id: "source-other".to_string(),
            name: "other".to_string(),
            url: "https://github.com/example/other".to_string(),
        };
        let plan = build_bulk_plan(home.path(), catalog.path(), &other, BulkMode::Uninstall)
            .expect("uninstall plan");
        assert!(plan.has_conflicts);
        assert_eq!(plan.entries[0].action, BulkPlanAction::SourceConflict);
        assert!(
            execute_bulk_plan(home.path(), catalog.path(), &other, BulkMode::Uninstall).is_err()
        );
        assert!(install_root(home.path())
            .join("python-standards/SKILL.md")
            .is_file());
    }

    #[test]
    fn bulk_uninstall_removes_every_installed_skill() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(&catalog.path().join("skills"), "python-standards", "Python");
        write_skill(&catalog.path().join("skills"), "git-ops", "Git");
        write_catalog_metadata(
            catalog.path(),
            &CatalogMetadata {
                version: CATALOG_METADATA_VERSION,
                source_id: Some(BUILT_IN_SOURCE_ID.to_string()),
                source: CATALOG_SOURCE.to_string(),
                commit_sha: TEST_COMMIT_SHA.to_string(),
                etag: None,
            },
        )
        .expect("catalog metadata");
        execute_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Install,
        )
        .expect("bulk install");

        let plan = build_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Uninstall,
        )
        .expect("uninstall plan");
        assert!(!plan.has_conflicts);
        assert!(plan
            .entries
            .iter()
            .all(|entry| entry.action == BulkPlanAction::Uninstall));

        let result = execute_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Uninstall,
        )
        .expect("bulk uninstall");
        assert_eq!(result.completed.len(), 2);
        assert!(result.failures.is_empty());
        assert!(!install_root(home.path()).join("python-standards").exists());
        assert!(!install_root(home.path()).join("git-ops").exists());
    }

    #[test]
    fn bulk_uninstall_keeps_locally_modified_skills() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(&catalog.path().join("skills"), "python-standards", "Python");
        write_skill(&catalog.path().join("skills"), "git-ops", "Git");
        write_catalog_metadata(
            catalog.path(),
            &CatalogMetadata {
                version: CATALOG_METADATA_VERSION,
                source_id: Some(BUILT_IN_SOURCE_ID.to_string()),
                source: CATALOG_SOURCE.to_string(),
                commit_sha: TEST_COMMIT_SHA.to_string(),
                etag: None,
            },
        )
        .expect("catalog metadata");
        execute_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Install,
        )
        .expect("bulk install");
        fs::write(
            install_root(home.path()).join("git-ops/SKILL.md"),
            "local edit",
        )
        .expect("local edit");

        let plan = build_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Uninstall,
        )
        .expect("uninstall plan");
        assert!(plan.has_conflicts);
        assert!(execute_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Uninstall
        )
        .is_err());
        assert!(install_root(home.path())
            .join("python-standards/SKILL.md")
            .is_file());
        assert!(install_root(home.path()).join("git-ops/SKILL.md").is_file());
    }

    #[test]
    fn bulk_uninstall_skips_skills_that_are_not_installed() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(&catalog.path().join("skills"), "python-standards", "Python");
        write_skill(&catalog.path().join("skills"), "git-ops", "Git");
        write_catalog_metadata(
            catalog.path(),
            &CatalogMetadata {
                version: CATALOG_METADATA_VERSION,
                source_id: Some(BUILT_IN_SOURCE_ID.to_string()),
                source: CATALOG_SOURCE.to_string(),
                commit_sha: TEST_COMMIT_SHA.to_string(),
                etag: None,
            },
        )
        .expect("catalog metadata");
        install_at_source(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            "git-ops",
        )
        .expect("install one member");

        let plan = build_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Uninstall,
        )
        .expect("uninstall plan");
        assert!(!plan.has_conflicts);
        assert_eq!(
            plan.entries
                .iter()
                .find(|entry| entry.name == "python-standards")
                .map(|entry| entry.action),
            Some(BulkPlanAction::NotInstalled)
        );
        assert_eq!(
            plan.entries
                .iter()
                .find(|entry| entry.name == "git-ops")
                .map(|entry| entry.action),
            Some(BulkPlanAction::Uninstall)
        );

        let result = execute_bulk_plan(
            home.path(),
            catalog.path(),
            &SourceDefinition::built_in(),
            BulkMode::Uninstall,
        )
        .expect("bulk uninstall");
        assert_eq!(result.completed.len(), 1);
        assert!(result.failures.is_empty());
        assert!(!install_root(home.path()).join("git-ops").exists());
    }

    fn test_archive() -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = Builder::new(encoder);
        let contents =
            b"---\nname: remote-skill\ndescription: \"Downloaded skill\"\n---\n\n# Remote\n";
        let mut header = Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                "skillbook-main/skills/remote-skill/SKILL.md",
                &contents[..],
            )
            .expect("archive entry");
        builder
            .into_inner()
            .expect("archive")
            .finish()
            .expect("compressed archive")
    }

    #[test]
    fn accepts_windows_utf8_frontmatter_and_legacy_marker() {
        let catalog = tempfile::tempdir().expect("temporary catalog");
        let skill = catalog.path().join("hello-world");
        fs::create_dir_all(&skill).expect("skill directory");
        fs::write(
            skill.join("SKILL.md"),
            "\u{feff}---\r\nname: hello-world\r\ndescription: \"Résumé checks\"\r\n---\r\n",
        )
        .expect("skill contents");
        fs::write(
            skill.join(MARKER_FILE),
            "\u{feff}Managed by Skill Manager. Do not edit this file.\r\n",
        )
        .expect("legacy marker");

        assert_eq!(
            skill_frontmatter(&skill).expect("frontmatter"),
            ("hello-world".to_string(), "Résumé checks".to_string())
        );
        assert!(matches!(
            install_ownership(&skill),
            InstallOwnership::Legacy
        ));
    }

    #[test]
    fn rejects_non_utf8_skill_metadata() {
        let catalog = tempfile::tempdir().expect("temporary catalog");
        let skill = catalog.path().join("hello-world");
        fs::create_dir_all(&skill).expect("skill directory");
        fs::write(skill.join("SKILL.md"), [0xff]).expect("invalid skill contents");

        let error = skill_frontmatter(&skill).expect_err("invalid UTF-8 should be rejected");

        assert!(error.contains("must be valid UTF-8"));
    }

    #[test]
    fn rejects_paths_that_cannot_be_created_on_windows() {
        assert!(validate_skill_name("con").is_err());
        assert!(validate_skill_name("com1").is_err());
        assert!(validate_portable_path_component(
            OsStr::new("NUL.txt"),
            Path::new("skillbook-main/skills/hello-world/NUL.txt")
        )
        .is_err());
        assert!(validate_portable_path_component(
            OsStr::new("trailing."),
            Path::new("skillbook-main/skills/hello-world/trailing.")
        )
        .is_err());
        assert!(validate_portable_path_component(
            OsStr::new("colon:name"),
            Path::new("skillbook-main/skills/hello-world/colon:name")
        )
        .is_err());
    }

    #[test]
    fn detects_case_insensitive_archive_collisions() {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = Builder::new(encoder);
        for path in [
            "skillbook-main/skills/remote-skill/SKILL.md",
            "skillbook-main/skills/remote-skill/skill.md",
        ] {
            let contents = b"---\nname: remote-skill\ndescription: \"Downloaded skill\"\n---\n";
            let mut header = Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, &contents[..])
                .expect("archive entry");
        }
        let bytes = builder
            .into_inner()
            .expect("archive")
            .finish()
            .expect("compressed archive");
        let target = tempfile::tempdir().expect("temporary extraction");

        let error =
            extract_catalog_archive(&bytes, target.path()).expect_err("collision should fail");

        assert!(error.contains("collide on Windows"));
    }

    #[test]
    fn catalog_reads_skills_from_a_directory() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(catalog.path(), "python-standards", "Strict Python");
        write_skill(catalog.path(), "git-ops", "Safe Git");

        let state = test_state(home.path(), catalog.path());

        assert_eq!(state.skills.len(), 2);
        assert_eq!(state.skills[0].name, "git-ops");
        assert_eq!(state.skills[1].name, "python-standards");
        assert!(state
            .skills
            .iter()
            .all(|skill| skill.status == SkillStatus::Available));
    }

    #[test]
    fn install_and_uninstall_round_trip() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(catalog.path(), "hello-world", "Test skill");
        let target = install_root(home.path()).join("hello-world");

        install_at(home.path(), catalog.path(), "hello-world").expect("install should work");

        assert!(target.join("SKILL.md").is_file());
        assert!(target.join(MARKER_FILE).is_file());
        assert_eq!(
            test_state(home.path(), catalog.path()).skills[0].status,
            SkillStatus::Installed
        );

        uninstall_at(home.path(), "hello-world").expect("uninstall should work");
        assert!(!target.exists());
    }

    #[test]
    fn identical_unmanaged_skill_can_be_adopted() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(catalog.path(), "hello-world", "Test skill");
        let target = install_root(home.path()).join("hello-world");
        fs::create_dir_all(&target).expect("unmanaged directory");
        fs::copy(
            catalog.path().join("hello-world/SKILL.md"),
            target.join("SKILL.md"),
        )
        .expect("matching unmanaged skill");

        assert_eq!(
            test_state(home.path(), catalog.path()).skills[0].status,
            SkillStatus::UnmanagedMatch
        );
        let report =
            auto_update_at(home.path(), catalog.path()).expect("automatic update should finish");
        assert!(report.updated_skills.is_empty());
        assert!(!target.join(MARKER_FILE).exists());
        assert!(replace_unmanaged_at(home.path(), catalog.path(), "hello-world").is_err());
        assert!(!backup_root(home.path()).exists());

        adopt_at(home.path(), catalog.path(), "hello-world").expect("adoption should work");

        assert!(target.join(MARKER_FILE).is_file());
        assert_eq!(
            test_state(home.path(), catalog.path()).skills[0].status,
            SkillStatus::Installed
        );
    }

    #[test]
    fn adoption_refuses_a_different_unmanaged_skill() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(catalog.path(), "hello-world", "Catalog version");
        let target = install_root(home.path()).join("hello-world");
        fs::create_dir_all(&target).expect("unmanaged directory");
        fs::write(target.join("SKILL.md"), "local version").expect("unmanaged skill");

        let error =
            adopt_at(home.path(), catalog.path(), "hello-world").expect_err("adoption should fail");

        assert!(error.contains("does not exactly match"));
        assert_eq!(
            fs::read_to_string(target.join("SKILL.md")).expect("unmanaged skill remains"),
            "local version"
        );
        assert!(!target.join(MARKER_FILE).exists());
    }

    #[test]
    fn replacement_backs_up_a_different_unmanaged_skill() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(catalog.path(), "hello-world", "Catalog version");
        let target = install_root(home.path()).join("hello-world");
        fs::create_dir_all(&target).expect("unmanaged directory");
        fs::write(target.join("SKILL.md"), "local version").expect("unmanaged skill");
        fs::write(target.join("local-notes.md"), "keep me").expect("local notes");

        assert_eq!(
            test_state(home.path(), catalog.path()).skills[0].status,
            SkillStatus::Conflict
        );
        let report =
            auto_update_at(home.path(), catalog.path()).expect("automatic update should finish");
        assert!(report.updated_skills.is_empty());
        assert_eq!(
            fs::read_to_string(target.join("SKILL.md")).expect("unmanaged skill remains"),
            "local version"
        );

        let replacement = replace_unmanaged_at(home.path(), catalog.path(), "hello-world")
            .expect("replacement should work");
        let backup = PathBuf::from(replacement.backup_path);

        assert!(backup.starts_with(backup_root(home.path()).join("hello-world")));
        assert_eq!(
            fs::read_to_string(backup.join("SKILL.md")).expect("backed-up skill"),
            "local version"
        );
        assert_eq!(
            fs::read_to_string(backup.join("local-notes.md")).expect("backed-up notes"),
            "keep me"
        );
        assert!(target.join(MARKER_FILE).is_file());
        assert!(fs::read_to_string(target.join("SKILL.md"))
            .expect("catalog skill")
            .contains("Catalog version"));
        assert_eq!(
            test_state(home.path(), catalog.path()).skills[0].status,
            SkillStatus::Installed
        );
    }

    #[test]
    fn failed_replacement_restores_the_original_skill() {
        let home = tempfile::tempdir().expect("temporary home");
        let target = install_root(home.path()).join("hello-world");
        let backup = backup_root(home.path()).join("hello-world/123");
        let missing_staging = install_root(home.path()).join(".missing-staging");
        fs::create_dir_all(&target).expect("unmanaged directory");
        fs::create_dir_all(backup.parent().expect("backup parent")).expect("backup root");
        fs::write(target.join("SKILL.md"), "local version").expect("unmanaged skill");

        let error =
            activate_unmanaged_replacement("hello-world", &target, &missing_staging, &backup)
                .expect_err("activation should fail");

        assert!(error.contains("original skill was restored"));
        assert_eq!(
            fs::read_to_string(target.join("SKILL.md")).expect("restored skill"),
            "local version"
        );
        assert!(!backup.exists());
    }

    #[test]
    fn update_replaces_an_unmodified_managed_skill() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(catalog.path(), "hello-world", "Version one");
        install_at(home.path(), catalog.path(), "hello-world").expect("install should work");
        write_skill(catalog.path(), "hello-world", "Version two");

        assert_eq!(
            test_state(home.path(), catalog.path()).skills[0].status,
            SkillStatus::UpdateAvailable
        );

        install_at(home.path(), catalog.path(), "hello-world").expect("update should work");
        assert_eq!(
            test_state(home.path(), catalog.path()).skills[0].status,
            SkillStatus::Installed
        );
        assert!(
            fs::read_to_string(install_root(home.path()).join("hello-world/SKILL.md"))
                .expect("installed skill")
                .contains("Version two")
        );
    }

    #[test]
    fn auto_update_updates_only_installed_unmodified_skills() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(catalog.path(), "hello-world", "Version one");
        install_at(home.path(), catalog.path(), "hello-world").expect("install should work");
        write_skill(catalog.path(), "hello-world", "Version two");
        write_skill(catalog.path(), "not-installed", "Leave uninstalled");

        let report =
            auto_update_at(home.path(), catalog.path()).expect("automatic update should work");

        assert_eq!(
            report.updated_skills,
            [SkillReference {
                source_id: BUILT_IN_SOURCE_ID.to_string(),
                name: "hello-world".to_string()
            }]
        );
        assert!(report.failed_skills.is_empty());
        assert!(!install_root(home.path()).join("not-installed").exists());
        assert!(
            fs::read_to_string(install_root(home.path()).join("hello-world/SKILL.md"))
                .expect("installed skill")
                .contains("Version two")
        );
    }

    #[test]
    fn modified_install_is_not_overwritten_or_removed() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(catalog.path(), "hello-world", "Test skill");
        install_at(home.path(), catalog.path(), "hello-world").expect("install should work");
        let installed = install_root(home.path()).join("hello-world/SKILL.md");
        fs::write(&installed, "local changes").expect("local edit");

        assert_eq!(
            test_state(home.path(), catalog.path()).skills[0].status,
            SkillStatus::Modified
        );
        let report =
            auto_update_at(home.path(), catalog.path()).expect("automatic update should finish");
        assert_eq!(
            report.skipped_modified_skills,
            [SkillReference {
                source_id: BUILT_IN_SOURCE_ID.to_string(),
                name: "hello-world".to_string()
            }]
        );
        assert!(install_at(home.path(), catalog.path(), "hello-world").is_err());
        assert!(adopt_at(home.path(), catalog.path(), "hello-world").is_err());
        assert!(replace_unmanaged_at(home.path(), catalog.path(), "hello-world").is_err());
        assert!(uninstall_at(home.path(), "hello-world").is_err());
        assert_eq!(
            fs::read_to_string(installed).expect("local edit remains"),
            "local changes"
        );
    }

    #[test]
    fn automatic_update_skips_legacy_install_markers() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(catalog.path(), "hello-world", "Catalog version");
        let target = install_root(home.path()).join("hello-world");
        fs::create_dir_all(&target).expect("installed directory");
        fs::write(
            target.join("SKILL.md"),
            "---\nname: hello-world\ndescription: \"Legacy version\"\n---\n",
        )
        .expect("legacy skill");
        fs::write(target.join(MARKER_FILE), LEGACY_MARKER_CONTENTS).expect("legacy marker");

        let report =
            auto_update_at(home.path(), catalog.path()).expect("automatic update should finish");

        assert_eq!(
            report.skipped_legacy_skills,
            [SkillReference {
                source_id: BUILT_IN_SOURCE_ID.to_string(),
                name: "hello-world".to_string()
            }]
        );
        assert!(fs::read_to_string(target.join("SKILL.md"))
            .expect("legacy skill remains")
            .contains("Legacy version"));
    }

    #[test]
    fn removed_managed_skill_remains_available_for_uninstall() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(catalog.path(), "hello-world", "Test skill");
        install_at(home.path(), catalog.path(), "hello-world").expect("install should work");
        fs::remove_dir_all(catalog.path().join("hello-world")).expect("remove from catalog");
        write_skill(catalog.path(), "other-skill", "Keeps catalog valid");

        let state = test_state(home.path(), catalog.path());
        let removed = state
            .skills
            .iter()
            .find(|skill| skill.name == "hello-world")
            .expect("removed skill remains visible");

        assert_eq!(removed.status, SkillStatus::Removed);
        let report =
            auto_update_at(home.path(), catalog.path()).expect("automatic update should finish");
        assert!(report.updated_skills.is_empty());
        assert!(install_root(home.path()).join("hello-world").exists());
        uninstall_at(home.path(), "hello-world").expect("removed skill can be uninstalled");
    }

    #[test]
    fn extracts_and_validates_a_skillbook_archive() {
        let target = tempfile::tempdir().expect("temporary extraction");

        extract_catalog_archive(&test_archive(), target.path()).expect("archive should extract");

        let skills = catalog_skills(target.path()).expect("catalog should validate");
        assert!(skills.contains_key("remote-skill"));
    }

    #[test]
    fn refreshed_catalog_records_its_commit_metadata() {
        let cache = tempfile::tempdir().expect("temporary cache");
        let metadata = CatalogMetadata {
            version: CATALOG_METADATA_VERSION,
            source_id: Some(BUILT_IN_SOURCE_ID.to_string()),
            source: CATALOG_SOURCE.to_string(),
            commit_sha: TEST_COMMIT_SHA.to_string(),
            etag: Some("\"catalog-etag\"".to_string()),
        };

        refresh_catalog(cache.path(), &test_archive(), &metadata)
            .expect("catalog refresh should work");

        let stored =
            read_catalog_metadata(&catalog_dir(cache.path()), &SourceDefinition::built_in())
                .expect("catalog metadata should be readable");
        assert_eq!(stored.commit_sha, TEST_COMMIT_SHA);
        assert_eq!(stored.etag.as_deref(), Some("\"catalog-etag\""));
    }

    #[test]
    fn archive_urls_require_a_full_lowercase_commit_sha() {
        assert_eq!(
            catalog_archive_url(TEST_COMMIT_SHA).expect("valid commit"),
            format!("{CATALOG_SOURCE}/archive/{TEST_COMMIT_SHA}.tar.gz")
        );
        assert!(catalog_archive_url("main").is_err());
        assert!(catalog_archive_url("0123456789ABCDEF0123456789ABCDEF01234567").is_err());
    }

    #[test]
    fn uninstall_refuses_an_unmanaged_directory() {
        let home = tempfile::tempdir().expect("temporary home");
        let target = install_root(home.path()).join("hello-world");
        fs::create_dir_all(&target).expect("unmanaged directory");
        fs::write(target.join("SKILL.md"), "keep me").expect("unmanaged skill");

        let error =
            uninstall_at(home.path(), "hello-world").expect_err("uninstall should be refused");

        assert!(error.contains("not managed"));
        assert!(target.join("SKILL.md").is_file());
    }

    fn custom_source(url: &str) -> SourceDefinition {
        let identity = validate_repository_url(url).expect("valid custom source URL");
        SourceDefinition {
            id: identity.source_key,
            name: identity.display_name,
            url: identity.canonical_url,
        }
    }

    fn source_catalog_for(source: SourceDefinition, catalog: &Path) -> SourceCatalog {
        let contents = catalog_contents(catalog).expect("valid source catalog");
        SourceCatalog {
            state: source_state(
                &source,
                SourceStatus::Fresh,
                false,
                None,
                Some(TEST_COMMIT_SHA.to_string()),
                1,
                contents.errors.clone(),
            ),
            definition: source,
            path: Some(catalog.to_path_buf()),
            skills: contents.skills,
        }
    }

    #[test]
    fn duplicate_names_never_cross_source_ownership() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog_a = tempfile::tempdir().expect("source A catalog");
        let catalog_b = tempfile::tempdir().expect("source B catalog");
        let source_a = custom_source("https://example.com/acme/source-a.git");
        let source_b = custom_source("https://example.com/acme/source-b.git");
        write_skill(catalog_a.path(), "shared-skill", "Source A");
        write_skill(catalog_b.path(), "shared-skill", "Source B");

        install_at_source(home.path(), catalog_a.path(), &source_a, "shared-skill")
            .expect("source A install");
        let catalogs = [
            source_catalog_for(source_a.clone(), catalog_a.path()),
            source_catalog_for(source_b.clone(), catalog_b.path()),
        ];
        let state = app_state_from_catalogs(home.path(), &catalogs, 1, AutoUpdateReport::default())
            .expect("multi-source state");

        let source_a_skill = state
            .skills
            .iter()
            .find(|skill| skill.source_id == source_a.id)
            .expect("source A skill");
        let source_b_skill = state
            .skills
            .iter()
            .find(|skill| skill.source_id == source_b.id)
            .expect("source B skill");
        assert_eq!(source_a_skill.status, SkillStatus::Installed);
        assert_eq!(source_b_skill.status, SkillStatus::SourceConflict);
        assert!(
            install_at_source(home.path(), catalog_b.path(), &source_b, "shared-skill").is_err()
        );
        assert!(
            uninstall_at_source(home.path(), &source_b.id, "shared-skill")
                .expect_err("wrong source cannot uninstall")
                .contains("different source")
        );

        write_skill(catalog_b.path(), "shared-skill", "Source B changed");
        let report = reconcile_source_skills(
            home.path(),
            catalog_b.path(),
            &source_b,
            &catalog_skills(catalog_b.path()).expect("source B catalog"),
        )
        .expect("source B reconciliation");
        assert!(report.updated_skills.is_empty());
        assert!(
            fs::read_to_string(install_root(home.path()).join("shared-skill/SKILL.md"))
                .expect("installed source A skill")
                .contains("Source A")
        );
    }

    #[test]
    fn removed_source_install_remains_visible_and_uninstallable() {
        let home = tempfile::tempdir().expect("temporary home");
        let catalog = tempfile::tempdir().expect("custom catalog");
        let source = custom_source("https://example.com/acme/personal-skills.git");
        write_skill(catalog.path(), "personal-skill", "Personal");
        install_at_source(home.path(), catalog.path(), &source, "personal-skill")
            .expect("custom install");

        let state = app_state_from_catalogs(home.path(), &[], 1, AutoUpdateReport::default())
            .expect("orphan state");
        assert!(state.sources.is_empty());
        assert_eq!(state.skills.len(), 1);
        assert_eq!(state.skills[0].source_id, source.id);
        assert_eq!(state.skills[0].status, SkillStatus::Removed);

        uninstall_at_source(home.path(), &source.id, "personal-skill").expect("orphan uninstall");
        assert!(!install_root(home.path()).join("personal-skill").exists());
    }

    #[test]
    fn source_configuration_is_atomic_strict_and_stable() {
        let config = tempfile::tempdir().expect("temporary config");
        let source = custom_source("HTTPS://EXAMPLE.COM:443/acme/personal-skills.git/");
        write_sources_config(config.path(), std::slice::from_ref(&source))
            .expect("write source config");
        assert_eq!(
            read_sources_config(config.path()).expect("read source config"),
            std::slice::from_ref(&source)
        );
        fs::rename(
            sources_config_path(config.path()),
            sources_config_backup_path(config.path()),
        )
        .expect("simulate interrupted replacement");
        assert_eq!(
            read_sources_config(config.path()).expect("recover source config"),
            std::slice::from_ref(&source)
        );
        assert!(sources_config_path(config.path()).is_file());
        assert!(!sources_config_backup_path(config.path()).exists());

        let second_identity =
            validate_repository_url("https://example.com/acme/personal-skills.git")
                .expect("same canonical URL");
        assert_eq!(second_identity.source_key, source.id);
        let duplicate_config = SourcesConfig {
            version: SOURCES_CONFIG_VERSION,
            sources: vec![source.clone(), source],
        };
        fs::write(
            sources_config_path(config.path()),
            serde_json::to_vec(&duplicate_config).expect("duplicate config JSON"),
        )
        .expect("write duplicate config");
        assert!(read_sources_config(config.path()).is_err());
        assert!(source_definitions(config.path()).is_empty());

        let built_in_duplicate = custom_source("https://github.com/jacobragsdale/skillbook.git");
        write_sources_config(config.path(), std::slice::from_ref(&built_in_duplicate))
            .expect("write duplicate built-in config");
        assert!(read_sources_config(config.path()).is_err());
    }

    #[test]
    fn source_configuration_seeds_once_migrates_and_preserves_empty() {
        let first_run = tempfile::tempdir().expect("first-run config");
        assert_eq!(
            read_sources_config(first_run.path()).expect("seed first run"),
            [SourceDefinition::built_in()]
        );
        assert!(sources_config_path(first_run.path()).is_file());

        write_sources_config(first_run.path(), &[]).expect("persist empty sources");
        assert!(read_sources_config(first_run.path())
            .expect("reload empty sources")
            .is_empty());
        assert!(source_definitions(first_run.path()).is_empty());

        let legacy = tempfile::tempdir().expect("legacy config");
        let custom = custom_source("https://example.com/acme/legacy-skills.git");
        let version_one = SourcesConfig {
            version: 1,
            sources: vec![custom.clone()],
        };
        fs::write(
            sources_config_path(legacy.path()),
            serde_json::to_vec(&version_one).expect("version-one config"),
        )
        .expect("write version-one config");
        let migrated = read_sources_config(legacy.path()).expect("migrate config");
        assert_eq!(migrated, [SourceDefinition::built_in(), custom]);
        let stored = serde_json::from_slice::<SourcesConfig>(
            &fs::read(sources_config_path(legacy.path())).expect("stored config"),
        )
        .expect("parse stored config");
        assert_eq!(stored.version, SOURCES_CONFIG_VERSION);
    }

    #[test]
    fn cached_catalog_requires_matching_metadata() {
        let cache = tempfile::tempdir().expect("temporary cache");
        let source = custom_source("https://example.com/acme/personal-skills.git");
        let catalog = catalog_dir(&source_cache_base(cache.path(), &source.id));
        fs::create_dir_all(&catalog).expect("catalog");
        write_skill(&catalog, "personal-skill", "Personal");

        let missing_metadata = source_catalog_from_disk(
            source.clone(),
            cache.path(),
            SourceStatus::Cached,
            None,
            None,
            1,
        );
        assert!(missing_metadata.path.is_none());
        assert_eq!(missing_metadata.state.status, SourceStatus::Error);
        assert!(!missing_metadata.state.refresh_failed);

        write_catalog_metadata(
            &catalog,
            &CatalogMetadata {
                version: CATALOG_METADATA_VERSION,
                source_id: Some(source.id.clone()),
                source: "https://example.com/acme/different-skills".to_string(),
                commit_sha: TEST_COMMIT_SHA.to_string(),
                etag: None,
            },
        )
        .expect("wrong-source metadata");
        let wrong_metadata =
            source_catalog_from_disk(source, cache.path(), SourceStatus::Cached, None, None, 2);
        assert!(wrong_metadata.path.is_none());
        assert_eq!(wrong_metadata.state.status, SourceStatus::Error);
        assert!(!wrong_metadata.state.refresh_failed);
    }

    #[test]
    fn legacy_catalog_cache_migrates_idempotently() {
        let cache = tempfile::tempdir().expect("temporary cache");
        let legacy = legacy_catalog_dir(cache.path());
        fs::create_dir_all(&legacy).expect("legacy catalog");
        write_skill(&legacy, "legacy-skill", "Legacy cache");
        write_catalog_metadata(
            &legacy,
            &CatalogMetadata {
                version: 1,
                source_id: None,
                source: CATALOG_SOURCE.to_string(),
                commit_sha: TEST_COMMIT_SHA.to_string(),
                etag: None,
            },
        )
        .expect("legacy metadata");

        migrate_legacy_catalog(cache.path()).expect("first migration");
        migrate_legacy_catalog(cache.path()).expect("idempotent migration");
        let migrated = catalog_dir(&source_cache_base(cache.path(), BUILT_IN_SOURCE_ID));
        assert!(migrated.join("legacy-skill/SKILL.md").is_file());
        assert!(read_catalog_metadata(&migrated, &SourceDefinition::built_in()).is_some());
        assert!(!legacy.exists());
    }

    #[test]
    fn source_failure_without_cache_does_not_hide_healthy_sources() {
        let home = tempfile::tempdir().expect("temporary home");
        let cache = tempfile::tempdir().expect("temporary cache");
        let healthy_catalog = tempfile::tempdir().expect("healthy catalog");
        let healthy = SourceDefinition::built_in();
        let failing = custom_source("https://example.com/acme/offline.git");
        write_skill(healthy_catalog.path(), "healthy-skill", "Healthy");
        let catalogs = [
            source_catalog_for(healthy, healthy_catalog.path()),
            source_catalog_from_disk(
                failing,
                cache.path(),
                SourceStatus::Cached,
                Some("offline".to_string()),
                None,
                1,
            ),
        ];

        let state = app_state_from_catalogs(home.path(), &catalogs, 1, AutoUpdateReport::default())
            .expect("partial state");
        assert_eq!(state.skills.len(), 1);
        assert_eq!(state.sources[0].status, SourceStatus::Fresh);
        assert!(!state.sources[0].refresh_failed);
        assert_eq!(state.sources[1].status, SourceStatus::Error);
        assert!(state.sources[1].refresh_failed);
        assert_eq!(state.sources[1].message.as_deref(), Some("offline"));
    }

    #[test]
    fn configured_unavailable_source_does_not_mark_install_removed() {
        let home = tempfile::tempdir().expect("temporary home");
        let cache = tempfile::tempdir().expect("temporary cache");
        let catalog = tempfile::tempdir().expect("custom catalog");
        let source = custom_source("https://example.com/acme/unavailable.git");
        write_skill(catalog.path(), "offline-skill", "Offline");
        install_at_source(home.path(), catalog.path(), &source, "offline-skill")
            .expect("custom install");
        let unavailable = source_catalog_from_disk(
            source.clone(),
            cache.path(),
            SourceStatus::Cached,
            Some("offline".to_string()),
            None,
            1,
        );

        let state =
            app_state_from_catalogs(home.path(), &[unavailable], 1, AutoUpdateReport::default())
                .expect("unavailable source state");
        assert_eq!(state.skills[0].status, SkillStatus::Installed);
        assert!(state.skills[0].description.contains("unavailable"));

        fs::write(
            install_root(home.path()).join("offline-skill/SKILL.md"),
            "local edit",
        )
        .expect("local edit");
        let unavailable = source_catalog_from_disk(
            source,
            cache.path(),
            SourceStatus::Cached,
            Some("offline".to_string()),
            None,
            2,
        );
        let modified =
            app_state_from_catalogs(home.path(), &[unavailable], 2, AutoUpdateReport::default())
                .expect("modified unavailable state");
        assert_eq!(modified.skills[0].status, SkillStatus::Modified);
    }

    #[test]
    fn custom_catalog_tree_reuses_portability_checks() {
        let catalog = tempfile::tempdir().expect("temporary catalog");
        write_skill(catalog.path(), "portable-skill", "Portable");
        fs::write(
            catalog.path().join("portable-skill/NUL.txt"),
            "not portable",
        )
        .expect("reserved file");
        assert!(validate_catalog_tree(catalog.path())
            .expect_err("reserved Windows path should fail")
            .contains("not portable to Windows"));
    }

    #[test]
    fn custom_catalog_copy_enforces_size_before_copying() {
        let source = tempfile::tempdir().expect("source catalog");
        let target_root = tempfile::tempdir().expect("target root");
        let oversized = source.path().join("oversized.bin");
        fs::File::create(&oversized)
            .and_then(|file| file.set_len(MAX_EXTRACTED_BYTES + 1))
            .expect("oversized sparse file");
        let target = target_root.path().join("catalog");

        let error = copy_validated_catalog_directory(source.path(), &target)
            .expect_err("oversized catalog should be rejected");
        assert!(error.contains("expands beyond"));
        assert!(!target.join("oversized.bin").exists());
    }

    #[test]
    fn version_one_json_marker_maps_only_to_the_built_in_source() {
        let home = tempfile::tempdir().expect("temporary home");
        let target = install_root(home.path()).join("legacy-json");
        fs::create_dir_all(&target).expect("legacy install");
        fs::write(
            target.join("SKILL.md"),
            "---\nname: legacy-json\ndescription: Legacy JSON\n---\n",
        )
        .expect("legacy skill");
        let digest = directory_digest(&target).expect("legacy digest");
        let marker = InstallMarker {
            version: 1,
            source_id: None,
            source: CATALOG_SOURCE.to_string(),
            skill_digest: digest,
        };
        fs::write(
            target.join(MARKER_FILE),
            serde_json::to_vec_pretty(&marker).expect("legacy marker"),
        )
        .expect("write legacy marker");

        let InstallOwnership::Managed(marker) = install_ownership(&target) else {
            panic!("version one JSON marker should remain managed");
        };
        assert_eq!(marker_source_id(&marker), Some(BUILT_IN_SOURCE_ID));
        assert!(
            uninstall_at_source(home.path(), "another-source", "legacy-json")
                .expect_err("custom source cannot remove built-in install")
                .contains("different source")
        );
        uninstall_at_source(home.path(), BUILT_IN_SOURCE_ID, "legacy-json")
            .expect("built-in source can remove legacy install");
    }

    #[test]
    fn custom_git_catalog_is_cloned_validated_and_activated() {
        let repository = tempfile::tempdir().expect("temporary Git repository");
        let cache = tempfile::tempdir().expect("temporary source cache");
        run_test_git(repository.path(), ["init", "--quiet", "-b", "main"]);
        run_test_git(
            repository.path(),
            ["config", "user.email", "skill-manager@example.invalid"],
        );
        run_test_git(
            repository.path(),
            ["config", "user.name", "Skill Manager Tests"],
        );
        write_skill(
            &repository.path().join("skills"),
            "custom-skill",
            "Custom Git source",
        );
        run_test_git(repository.path(), ["add", "."]);
        run_test_git(
            repository.path(),
            ["commit", "--quiet", "-m", "Add custom skill"],
        );
        let source = SourceDefinition {
            id: "test-local-source".to_string(),
            name: "local-source".to_string(),
            url: repository.path().display().to_string(),
        };

        let prepared =
            prepare_catalog_from_git(&source, None, cache.path()).expect("prepare Git catalog");
        let PreparedCatalog::Staged {
            commit_sha,
            path,
            contents,
        } = prepared
        else {
            panic!("new Git source should stage a catalog");
        };
        assert!(valid_commit_sha(&commit_sha));
        assert!(contents.skills.contains_key("custom-skill"));
        activate_catalog(&path, cache.path()).expect("activate Git catalog");
        let current = catalog_dir(cache.path());
        assert!(current.join("skills/custom-skill/SKILL.md").is_file());
        let metadata = read_catalog_metadata(&current, &source).expect("custom catalog metadata");
        assert_eq!(metadata.commit_sha, commit_sha);

        fs::remove_file(current.join("skills/custom-skill/SKILL.md"))
            .expect("corrupt cached catalog");
        let repaired = prepare_catalog_from_git(&source, Some(metadata), cache.path())
            .expect("unchanged corrupt cache should be refreshed");
        assert!(matches!(repaired, PreparedCatalog::Staged { .. }));
    }

    #[test]
    fn custom_git_catalog_errors_identify_invalid_repository_content() {
        let repository = tempfile::tempdir().expect("temporary Git repository");
        let cache = tempfile::tempdir().expect("temporary source cache");
        run_test_git(repository.path(), ["init", "--quiet", "-b", "main"]);
        run_test_git(
            repository.path(),
            ["config", "user.email", "skill-manager@example.invalid"],
        );
        run_test_git(
            repository.path(),
            ["config", "user.name", "Skill Manager Tests"],
        );
        let skill = repository.path().join("skills/skillbook4-broken");
        fs::create_dir_all(&skill).expect("invalid skill directory");
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: skillbook4-broken\n---\n",
        )
        .expect("invalid skill metadata");
        run_test_git(repository.path(), ["add", "."]);
        run_test_git(
            repository.path(),
            ["commit", "--quiet", "-m", "Add invalid skill"],
        );
        let source = SourceDefinition {
            id: "test-invalid-source".to_string(),
            name: "invalid-source".to_string(),
            url: repository.path().display().to_string(),
        };

        let error = match prepare_catalog_from_git(&source, None, cache.path()) {
            Ok(_) => panic!("invalid Git source should be rejected"),
            Err(error) => error,
        };

        assert_eq!(
            error,
            "This Git repository is not properly formatted as a Skill Manager source: \
	The catalog does not contain any valid skills. skills/skillbook4-broken: \
	skills/skillbook4-broken/SKILL.md is missing a description"
        );
        assert!(!error.contains(&repository.path().display().to_string()));
        assert!(!error.contains(&cache.path().display().to_string()));
    }

    #[test]
    #[ignore = "requires network access and the system Git executable"]
    fn live_fixture_repositories_have_distinct_expected_catalog_shapes() {
        struct ExpectedShape {
            url: &'static str,
            skills: usize,
            errors: usize,
        }

        for expected in [
            ExpectedShape {
                url: "https://github.com/jacobragsdale/skillbook2.git",
                skills: 3,
                errors: 0,
            },
            ExpectedShape {
                url: "https://github.com/jacobragsdale/skillbook3.git",
                skills: 3,
                errors: 0,
            },
            ExpectedShape {
                url: "https://github.com/jacobragsdale/skillbook4.git",
                skills: 1,
                errors: 0,
            },
        ] {
            let cache = tempfile::tempdir().expect("temporary source cache");
            let source = custom_source(expected.url);
            let prepared = prepare_catalog_from_git(&source, None, cache.path())
                .expect("prepare live fixture catalog");
            let PreparedCatalog::Staged { path, .. } = prepared else {
                panic!("a new live fixture should stage a catalog");
            };
            let contents = catalog_contents(&path).expect("live fixture catalog");
            assert_eq!(contents.skills.len(), expected.skills, "{}", expected.url);
            assert_eq!(contents.errors.len(), expected.errors, "{}", expected.url);
        }
    }

    fn run_test_git<const N: usize>(working_directory: &Path, arguments: [&str; N]) {
        let output = Command::new("git")
            .current_dir(working_directory)
            .args(arguments)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("run test Git");
        assert!(
            output.status.success(),
            "Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
