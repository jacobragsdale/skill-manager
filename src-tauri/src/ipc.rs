//! Serialized frontend contracts and thin Tauri command adapters.

use crate::application::{self, RuntimeState};
use crate::domain::{CatalogError, SkillStatus, SourceStatus};
use serde::Serialize;
use tauri::State;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Skill {
    pub(crate) source_id: String,
    pub(crate) source_name: String,
    pub(crate) source_url: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) status: SkillStatus,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceState {
    pub(crate) id: String,
    pub(crate) name: String,
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
pub(crate) struct AppState {
    pub(crate) install_root: String,
    pub(crate) checked_at_epoch_seconds: u64,
    pub(crate) auto_update_report: AutoUpdateReport,
    pub(crate) sources: Vec<SourceState>,
    pub(crate) skills: Vec<Skill>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillReference {
    pub(crate) source_id: String,
    pub(crate) name: String,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AutoUpdateReport {
    pub(crate) updated_skills: Vec<SkillReference>,
    pub(crate) skipped_modified_skills: Vec<SkillReference>,
    pub(crate) skipped_legacy_skills: Vec<SkillReference>,
    pub(crate) failed_skills: Vec<SkillUpdateFailure>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillUpdateFailure {
    pub(crate) source_id: String,
    pub(crate) name: String,
    pub(crate) message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReplaceUnmanagedResult {
    pub(crate) backup_path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum BulkPlanAction {
    Install,
    Update,
    Installed,
    Uninstall,
    NotInstalled,
    Adopt,
    Conflict,
    Modified,
    SourceConflict,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BulkPlanEntry {
    pub(crate) name: String,
    pub(crate) action: BulkPlanAction,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BulkPlan {
    pub(crate) source_id: String,
    pub(crate) has_conflicts: bool,
    pub(crate) entries: Vec<BulkPlanEntry>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BulkInstallFailure {
    pub(crate) name: String,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BulkInstallResult {
    pub(crate) completed: Vec<BulkPlanEntry>,
    pub(crate) failures: Vec<BulkInstallFailure>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum ScheduledSync {
    Updated { state: Box<AppState> },
    Failed { message: String },
}

#[tauri::command]
pub(crate) async fn load_cached_app_state(
    runtime: State<'_, RuntimeState>,
) -> Result<Option<AppState>, String> {
    application::load_cached_app_state(runtime.inner()).await
}

#[tauri::command]
pub(crate) async fn sync_app_state(runtime: State<'_, RuntimeState>) -> Result<AppState, String> {
    application::sync_app_state(runtime.inner()).await
}

#[tauri::command]
pub(crate) async fn plan_install_all(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
) -> Result<BulkPlan, String> {
    application::plan_install_all(runtime.inner(), source_id).await
}

#[tauri::command]
pub(crate) async fn install_all(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
) -> Result<BulkInstallResult, String> {
    application::install_all(runtime.inner(), source_id).await
}

#[tauri::command]
pub(crate) async fn plan_uninstall_all(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
) -> Result<BulkPlan, String> {
    application::plan_uninstall_all(runtime.inner(), source_id).await
}

#[tauri::command]
pub(crate) async fn uninstall_all(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
) -> Result<BulkInstallResult, String> {
    application::uninstall_all(runtime.inner(), source_id).await
}

#[tauri::command]
pub(crate) async fn install_skill(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    name: &str,
) -> Result<(), String> {
    application::install_skill(runtime.inner(), source_id, name).await
}

#[tauri::command]
pub(crate) async fn adopt_skill(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    name: &str,
) -> Result<(), String> {
    application::adopt_skill(runtime.inner(), source_id, name).await
}

#[tauri::command]
pub(crate) async fn replace_unmanaged_skill(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    name: &str,
) -> Result<ReplaceUnmanagedResult, String> {
    application::replace_unmanaged_skill(runtime.inner(), source_id, name).await
}

#[tauri::command]
pub(crate) async fn uninstall_skill(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
    name: &str,
) -> Result<(), String> {
    application::uninstall_skill(runtime.inner(), source_id, name).await
}

#[tauri::command]
pub(crate) async fn add_source(
    runtime: State<'_, RuntimeState>,
    url: &str,
) -> Result<AppState, String> {
    application::add_source(runtime.inner(), url).await
}

#[tauri::command]
pub(crate) async fn add_default_source(
    runtime: State<'_, RuntimeState>,
) -> Result<AppState, String> {
    application::add_default_source(runtime.inner()).await
}

#[tauri::command]
pub(crate) async fn remove_source(
    runtime: State<'_, RuntimeState>,
    source_id: &str,
) -> Result<AppState, String> {
    application::remove_source(runtime.inner(), source_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skills_only_state_and_bulk_plan_have_stable_ipc_shapes() {
        let state = AppState {
            install_root: "/home/test/.agents/skills".to_string(),
            checked_at_epoch_seconds: 1,
            auto_update_report: AutoUpdateReport::default(),
            sources: Vec::new(),
            skills: Vec::new(),
        };
        let value = serde_json::to_value(state).expect("state JSON");
        assert!(value.get("skills").is_some());
        assert!(value.get("bundles").is_none());
        assert!(value.get("recoveryAction").is_none());

        let plan = BulkPlan {
            source_id: "source-test".to_string(),
            has_conflicts: false,
            entries: Vec::new(),
        };
        let value = serde_json::to_value(plan).expect("plan JSON");
        assert!(value.get("sourceId").is_some());
        assert!(value.get("bundleName").is_none());
    }
}
