//! Generic manifest item contracts and thin Tauri command adapters.

use crate::application_v1::{self, OutputCallback, RuntimeState};
use crate::catalog_v1::AgentSkillMetadata;
use crate::domain::{CatalogError, SourceStatus};
use crate::install_v1::{ItemStatus, OperationOutcome, SourceRemovalPlan};
use crate::manifest::DestinationAnchor;
use crate::process::OutputStream;
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

const OPERATION_OUTPUT_EVENT: &str = "operation-output";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActionState {
    pub(crate) id: String,
    pub(crate) local_id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) supported: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentSkillState {
    pub(crate) local_name: String,
    pub(crate) license: Option<String>,
    pub(crate) compatibility: Option<String>,
    pub(crate) metadata: std::collections::BTreeMap<String, String>,
    pub(crate) allowed_tools: Option<String>,
    pub(crate) manual_only: bool,
}

impl From<&AgentSkillMetadata> for AgentSkillState {
    fn from(metadata: &AgentSkillMetadata) -> Self {
        Self {
            local_name: metadata.local_name.clone(),
            license: metadata.license.clone(),
            compatibility: metadata.compatibility.clone(),
            metadata: metadata.metadata.clone(),
            allowed_tools: metadata.allowed_tools.clone(),
            manual_only: metadata.manual_only,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DestinationState {
    pub(crate) anchor: DestinationAnchor,
    pub(crate) path: String,
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
    pub(crate) kind: String,
    pub(crate) materialized_skill_name: Option<String>,
    pub(crate) agent_skill: Option<AgentSkillState>,
    pub(crate) destinations: Vec<DestinationState>,
    pub(crate) status: ItemStatus,
    pub(crate) executable: bool,
    pub(crate) actions: Vec<ActionState>,
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
    pub(crate) executable: bool,
    pub(crate) trusted: bool,
    pub(crate) trust_required: bool,
    pub(crate) actions: Vec<ActionState>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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
    pub(crate) skipped_untrusted_items: Vec<ItemReference>,
    pub(crate) migration_attention: Vec<ItemFailure>,
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
    pub(crate) executable: bool,
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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BulkPlan {
    pub(crate) source_id: String,
    pub(crate) uninstall: bool,
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
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum ScheduledSync {
    Updated { state: Box<AppState> },
    Failed { message: String },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationOutput {
    operation_id: String,
    stream: &'static str,
    text: String,
}

fn output_callback(app: AppHandle, operation_id: String) -> OutputCallback {
    Arc::new(move |stream, bytes| {
        let stream = match stream {
            OutputStream::Stdout => "stdout",
            OutputStream::Stderr => "stderr",
        };
        let event = OperationOutput {
            operation_id: operation_id.clone(),
            stream,
            text: String::from_utf8_lossy(bytes).into_owned(),
        };
        let _ = app.emit(OPERATION_OUTPUT_EVENT, event);
    })
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
    accept_executable_trust: bool,
) -> Result<AppState, String> {
    application_v1::confirm_source(runtime.inner(), token, accept_executable_trust).await
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
pub(crate) async fn set_source_trust(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    trusted: bool,
) -> Result<AppState, String> {
    application_v1::set_source_trust(runtime.inner(), source_id, trusted).await
}

#[tauri::command]
pub(crate) async fn install_item(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    local_id: &str,
    operation_id: String,
) -> Result<OperationOutcome, String> {
    application_v1::install_item(
        runtime.inner(),
        source_id,
        local_id,
        output_callback(app, operation_id),
    )
    .await
}

#[tauri::command]
pub(crate) async fn replace_item(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    local_id: &str,
    operation_id: String,
) -> Result<OperationOutcome, String> {
    application_v1::replace_item(
        runtime.inner(),
        source_id,
        local_id,
        output_callback(app, operation_id),
    )
    .await
}

#[tauri::command]
pub(crate) async fn uninstall_item(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    local_id: &str,
    operation_id: String,
) -> Result<OperationOutcome, String> {
    application_v1::uninstall_item(
        runtime.inner(),
        source_id,
        local_id,
        output_callback(app, operation_id),
    )
    .await
}

#[tauri::command]
pub(crate) async fn run_item_action(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    local_id: &str,
    action_id: &str,
    operation_id: String,
) -> Result<OperationOutcome, String> {
    application_v1::run_item_action(
        runtime.inner(),
        source_id,
        local_id,
        action_id,
        output_callback(app, operation_id),
    )
    .await
}

#[tauri::command]
pub(crate) async fn run_source_action(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    action_id: &str,
    operation_id: String,
) -> Result<OperationOutcome, String> {
    application_v1::run_source_action(
        runtime.inner(),
        source_id,
        action_id,
        output_callback(app, operation_id),
    )
    .await
}

#[tauri::command]
pub(crate) async fn plan_bulk_items(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    uninstall: bool,
) -> Result<BulkPlan, String> {
    application_v1::bulk_plan(runtime.inner(), source_id, uninstall).await
}

#[tauri::command]
pub(crate) async fn run_bulk_items(
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    uninstall: bool,
    operation_id: String,
) -> Result<BulkResult, String> {
    application_v1::bulk_run(
        runtime.inner(),
        source_id,
        uninstall,
        output_callback(app, operation_id),
    )
    .await
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
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    acknowledge_modified_paths: bool,
    approve_cleanup_execution: bool,
    operation_id: String,
) -> Result<BulkResult, String> {
    application_v1::remove_source(
        runtime.inner(),
        source_id,
        acknowledge_modified_paths,
        approve_cleanup_execution,
        output_callback(app, operation_id),
    )
    .await
}
