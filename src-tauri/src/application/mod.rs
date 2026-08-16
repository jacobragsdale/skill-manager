//! Application service for source synchronization and file installation.

mod agents;
mod items;
mod project;
mod sources;
pub(crate) mod status;
mod sync;

use crate::app_state::SourceStatus;
use crate::source::{ConfiguredRepository, ConfiguredSource, RepositorySnapshot, SourceSnapshot};
use crate::source::{RepositoryCandidate, SourceCandidate};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{
    async_runtime::{self, Mutex},
    AppHandle, Emitter, Manager, Runtime,
};
use tokio::time::{self, MissedTickBehavior};

pub(crate) use agents::{
    list_agent_profiles, preview_agent_cleanup, preview_agent_enable, set_agent_enabled,
};
pub(crate) use items::{
    bulk_plan, bulk_run, install_item, plan_source_removal, preview_install, remove_source,
    replace_item, reset_app, uninstall_item,
};
pub(crate) use sources::{
    cancel_prepared_source, cancel_prepared_source_repository, confirm_source,
    confirm_source_repository, prepare_source, prepare_source_repository, remove_source_repository,
};
pub(crate) use sync::{load_cached_app_state, sync_app_state};

const SCHEDULED_SYNC_EVENT: &str = "scheduled-sync";
const SCHEDULED_SYNC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

pub(crate) struct RuntimeState {
    pub(super) operation_lock: Mutex<()>,
    pub(super) sync_lock: Mutex<()>,
    pub(super) pending_sources: Mutex<BTreeMap<String, SourceCandidate>>,
    pub(super) pending_repositories: Mutex<BTreeMap<String, RepositoryCandidate>>,
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

pub(super) struct LoadedSource {
    pub(super) definition: ConfiguredSource,
    pub(super) snapshot: Option<SourceSnapshot>,
    pub(super) status: SourceStatus,
    pub(super) refresh_failed: bool,
    pub(super) message: Option<String>,
}

pub(super) struct LoadedRepository {
    pub(super) definition: ConfiguredRepository,
    pub(super) snapshot: Option<RepositorySnapshot>,
    pub(super) status: SourceStatus,
    pub(super) refresh_failed: bool,
    pub(super) message: Option<String>,
}

pub(super) async fn run_blocking<T, F>(context: &'static str, task: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    async_runtime::spawn_blocking(task)
        .await
        .map_err(|error| format!("{context} worker failed: {error}"))?
}

pub(super) fn current_epoch_seconds() -> u64 {
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
    use crate::agent_profiles::TargetId;
    use crate::app_state::AppState;
    use crate::install::ItemStatus;

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
