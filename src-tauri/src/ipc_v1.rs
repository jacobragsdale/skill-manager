//! Thin Tauri command surface for the desktop UI.

use crate::agent_profiles::{AgentProfileState, TargetId};
use crate::app_state::{
    AgentEnablePreview, AppState, BulkAction, BulkPlan, BulkResult, PreparedRepository,
    PreparedSource,
};
use crate::application_v1::{self, RuntimeState};
use crate::executor::TargetCleanupPreview;
use crate::install_v1::{OperationOutcome, SourceRemovalPlan};
use crate::planner::InstallPreview;
use tauri::State;

#[tauri::command]
pub(crate) async fn load_cached_manifest_state(
    runtime: State<'_, RuntimeState>,
) -> Result<Option<AppState>, String> {
    application_v1::load_cached_app_state(runtime.inner()).await
}

#[tauri::command]
pub(crate) async fn sync_manifest_state(
    runtime: State<'_, RuntimeState>,
) -> Result<AppState, String> {
    application_v1::sync_app_state(runtime.inner()).await
}

#[tauri::command]
pub(crate) async fn prepare_source(
    runtime: State<'_, RuntimeState>,
    url: &str,
    repository_key: &str,
) -> Result<PreparedSource, String> {
    application_v1::prepare_source(runtime.inner(), url, repository_key.to_string()).await
}

#[tauri::command]
pub(crate) async fn prepare_source_repository(
    runtime: State<'_, RuntimeState>,
    url: &str,
) -> Result<PreparedRepository, String> {
    application_v1::prepare_source_repository(runtime.inner(), url).await
}

#[tauri::command]
pub(crate) async fn confirm_source_repository(
    runtime: State<'_, RuntimeState>,
    token: &str,
) -> Result<AppState, String> {
    application_v1::confirm_source_repository(runtime.inner(), token).await
}

#[tauri::command]
pub(crate) async fn cancel_prepared_source_repository(
    runtime: State<'_, RuntimeState>,
    token: &str,
) -> Result<(), String> {
    application_v1::cancel_prepared_source_repository(runtime.inner(), token).await
}

#[tauri::command]
pub(crate) async fn remove_source_repository(
    runtime: State<'_, RuntimeState>,
    repository_key: &str,
) -> Result<AppState, String> {
    application_v1::remove_source_repository(runtime.inner(), repository_key).await
}

#[tauri::command]
pub(crate) async fn confirm_source(
    runtime: State<'_, RuntimeState>,
    token: &str,
) -> Result<AppState, String> {
    application_v1::confirm_source(runtime.inner(), token).await
}

#[tauri::command]
pub(crate) async fn cancel_prepared_source(
    runtime: State<'_, RuntimeState>,
    token: &str,
) -> Result<(), String> {
    application_v1::cancel_prepared_source(runtime.inner(), token).await
}

#[tauri::command]
pub(crate) async fn install_item(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    local_id: &str,
    trust_approved: bool,
    component_id: Option<String>,
) -> Result<OperationOutcome, String> {
    application_v1::install_item(
        runtime.inner(),
        source_id,
        local_id,
        trust_approved,
        component_id.as_deref(),
    )
    .await
}

#[tauri::command]
pub(crate) async fn replace_item(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    local_id: &str,
    trust_approved: bool,
    component_id: Option<String>,
) -> Result<OperationOutcome, String> {
    application_v1::replace_item(
        runtime.inner(),
        source_id,
        local_id,
        trust_approved,
        component_id.as_deref(),
    )
    .await
}

#[tauri::command]
pub(crate) async fn preview_install_item(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    local_id: &str,
    component_id: Option<String>,
) -> Result<InstallPreview, String> {
    application_v1::preview_install(
        runtime.inner(),
        source_id,
        local_id,
        component_id.as_deref(),
    )
    .await
}

#[tauri::command]
pub(crate) async fn list_agent_profiles(
    runtime: State<'_, RuntimeState>,
) -> Result<Vec<AgentProfileState>, String> {
    application_v1::list_agent_profiles(runtime.inner()).await
}

#[tauri::command]
pub(crate) async fn preview_agent_cleanup(
    runtime: State<'_, RuntimeState>,
    target_id: TargetId,
) -> Result<TargetCleanupPreview, String> {
    application_v1::preview_agent_cleanup(runtime.inner(), target_id).await
}

#[tauri::command]
pub(crate) async fn preview_agent_enable(
    runtime: State<'_, RuntimeState>,
    target_id: TargetId,
) -> Result<AgentEnablePreview, String> {
    application_v1::preview_agent_enable(runtime.inner(), target_id).await
}

#[tauri::command]
pub(crate) async fn set_agent_enabled(
    runtime: State<'_, RuntimeState>,
    target_id: TargetId,
    enabled: bool,
    acknowledge_modified_resources: bool,
    trust_approved: bool,
) -> Result<Vec<AgentProfileState>, String> {
    application_v1::set_agent_enabled(
        runtime.inner(),
        target_id,
        enabled,
        acknowledge_modified_resources,
        trust_approved,
    )
    .await
}

#[tauri::command]
pub(crate) async fn uninstall_item(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    local_id: &str,
    component_id: Option<String>,
) -> Result<OperationOutcome, String> {
    application_v1::uninstall_item(
        runtime.inner(),
        source_id,
        local_id,
        component_id.as_deref(),
    )
    .await
}

#[tauri::command]
pub(crate) async fn plan_bulk_items(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    action: BulkAction,
) -> Result<BulkPlan, String> {
    application_v1::bulk_plan(runtime.inner(), source_id, action).await
}

#[tauri::command]
pub(crate) async fn run_bulk_items(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    action: BulkAction,
    trust_approved: bool,
) -> Result<BulkResult, String> {
    application_v1::bulk_run(runtime.inner(), source_id, action, trust_approved).await
}

#[tauri::command]
pub(crate) async fn plan_source_removal(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
) -> Result<SourceRemovalPlan, String> {
    application_v1::plan_source_removal(runtime.inner(), source_id).await
}

#[tauri::command]
pub(crate) async fn remove_manifest_source(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    acknowledge_modified_paths: bool,
) -> Result<BulkResult, String> {
    application_v1::remove_source(runtime.inner(), source_id, acknowledge_modified_paths).await
}

#[tauri::command]
pub(crate) async fn reset_source(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
) -> Result<BulkResult, String> {
    application_v1::reset_source(runtime.inner(), source_id).await
}
