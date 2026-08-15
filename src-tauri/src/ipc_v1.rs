//! Small serialized command surface for the desktop UI.

use crate::application_v1::{self, RuntimeState};
use crate::catalog_v1::CatalogError;
use crate::install_v1::{ItemStatus, OperationOutcome, SourceRemovalPlan};
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SourceStatus {
    Fresh,
    Cached,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogItemState {
    pub(crate) id: String,
    pub(crate) local_id: String,
    pub(crate) source_id: String,
    pub(crate) source_key: String,
    pub(crate) source_name: String,
    pub(crate) source_url: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) manual_invocation: bool,
    pub(crate) source: String,
    pub(crate) source_is_directory: bool,
    pub(crate) is_agent_plugin: bool,
    pub(crate) destination: String,
    pub(crate) status: ItemStatus,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceState {
    pub(crate) source_id: String,
    pub(crate) source_key: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) url: String,
    pub(crate) built_in: bool,
    pub(crate) status: SourceStatus,
    pub(crate) refresh_failed: bool,
    pub(crate) message: Option<String>,
    pub(crate) commit: Option<String>,
    pub(crate) checked_at_epoch_seconds: u64,
    pub(crate) catalog_errors: Vec<CatalogError>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ItemReference {
    pub(crate) id: String,
    pub(crate) source_id: String,
    pub(crate) local_id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ItemFailure {
    pub(crate) id: String,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AutoUpdateReport {
    pub(crate) updated_items: Vec<ItemReference>,
    pub(crate) failed_items: Vec<ItemFailure>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppState {
    pub(crate) checked_at_epoch_seconds: u64,
    pub(crate) auto_update_report: AutoUpdateReport,
    pub(crate) sources: Vec<SourceState>,
    pub(crate) items: Vec<CatalogItemState>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreparedSource {
    pub(crate) token: String,
    pub(crate) source_id: String,
    pub(crate) source_key: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) url: String,
    pub(crate) commit: String,
    pub(crate) item_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BulkPlanEntry {
    pub(crate) id: String,
    pub(crate) local_id: String,
    pub(crate) status: ItemStatus,
    pub(crate) will_run: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum BulkAction {
    Install,
    Replace,
    Uninstall,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BulkPlan {
    pub(crate) source_id: String,
    pub(crate) action: BulkAction,
    pub(crate) entries: Vec<BulkPlanEntry>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BulkFailure {
    pub(crate) id: String,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BulkResult {
    pub(crate) completed: Vec<String>,
    pub(crate) failures: Vec<BulkFailure>,
    pub(crate) backup_paths: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum ScheduledSync {
    Updated { state: Box<AppState> },
    Failed { message: String },
}

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
) -> Result<PreparedSource, String> {
    application_v1::prepare_source(runtime.inner(), url).await
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
pub(crate) async fn add_default_manifest_source(
    runtime: State<'_, RuntimeState>,
) -> Result<AppState, String> {
    application_v1::add_default_source(runtime.inner()).await
}

#[tauri::command]
pub(crate) async fn install_item(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    local_id: &str,
) -> Result<OperationOutcome, String> {
    application_v1::install_item(runtime.inner(), source_id, local_id).await
}

#[tauri::command]
pub(crate) async fn replace_item(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    local_id: &str,
) -> Result<OperationOutcome, String> {
    application_v1::replace_item(runtime.inner(), source_id, local_id).await
}

#[tauri::command]
pub(crate) async fn uninstall_item(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    local_id: &str,
) -> Result<OperationOutcome, String> {
    application_v1::uninstall_item(runtime.inner(), source_id, local_id).await
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
) -> Result<BulkResult, String> {
    application_v1::bulk_run(runtime.inner(), source_id, action).await
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
