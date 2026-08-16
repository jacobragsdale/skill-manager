use super::{current_epoch_seconds, run_blocking, RuntimeState};
use crate::app_state::{AppState, PreparedRepository, PreparedSource};
use crate::locator::Locator;
use crate::source::{self, RepositoryCandidate, SourceCandidate};
use crate::sources::{cache_base_dir, config_base_dir};
use sha2::{Digest, Sha256};

pub(crate) async fn prepare_source(
    runtime: &RuntimeState,
    url: &str,
    repository_key: String,
) -> Result<PreparedSource, String> {
    let _guard = runtime.operation_lock.lock().await;
    let locator = Locator::parse(url)?;
    let cache = cache_base_dir()?;
    let config = config_base_dir()?;
    let config_file = source::read_sources_config(&config)?;
    let repository = config_file
        .repositories
        .iter()
        .find(|repository| repository.repository_key == repository_key)
        .ok_or_else(|| "That source catalog is no longer configured.".to_string())?;
    let snapshot = source::load_current_repository(&cache, repository)?
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
        source::prepare_new_source(
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
        source::discard_candidate(&candidate);
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
        source::discard_candidate(&candidate);
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
        source::activate_candidate(&cache, candidate)
    })
    .await?;
    let mut config_file = source::read_sources_config(&config)?;
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
    source::write_sources_config(&config, &config_file)?;
    super::project::cached_state_now()
}

pub(crate) async fn cancel_prepared_source(
    runtime: &RuntimeState,
    token: &str,
) -> Result<(), String> {
    if let Some(candidate) = runtime.pending_sources.lock().await.remove(token) {
        source::discard_candidate(&candidate);
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
    let configured = source::read_sources_config(&config)?;
    let candidate = run_blocking("Source repository preparation", move || {
        source::prepare_new_repository(&locator, &cache)
    })
    .await?;
    if configured.repositories.iter().any(|repository| {
        repository.repository_key == candidate.definition.repository_key
            || repository.repository_id == candidate.definition.repository_id
            || repository
                .locator
                .same_identity(&candidate.definition.locator)
    }) {
        source::discard_repository(&candidate);
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
        source::activate_repository(&cache, candidate)
    })
    .await?;
    let mut config_file = source::read_sources_config(&config)?;
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
    source::write_sources_config(&config, &config_file)?;
    super::project::cached_state_now()
}

pub(crate) async fn cancel_prepared_source_repository(
    runtime: &RuntimeState,
    token: &str,
) -> Result<(), String> {
    if let Some(candidate) = runtime.pending_repositories.lock().await.remove(token) {
        source::discard_repository(&candidate);
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
    let mut config_file = source::read_sources_config(&config)?;
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
    source::write_sources_config(&config, &config_file)?;
    source::remove_repository_cache(&cache, repository_key)?;
    super::project::cached_state_now()
}

pub(super) fn prepared_token(candidate: &SourceCandidate) -> String {
    hash_token(&candidate.definition.source_key, &candidate.commit)
}

pub(super) fn prepared_repository_token(candidate: &RepositoryCandidate) -> String {
    hash_token(&candidate.definition.repository_key, &candidate.revision)
}

pub(super) fn hash_token(key: &str, revision: &str) -> String {
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
