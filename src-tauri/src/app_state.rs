//! Application and IPC DTOs. Commands serialize these; they do not own use-case logic.

use crate::agent_profiles::{AgentProfileState, TargetId};
use crate::catalog::CatalogError;
use crate::install::ItemStatus;
use crate::planner::InstallPreview;
use crate::resource::CompatibilityReport;
use serde::{Deserialize, Serialize};

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
    pub(crate) manifest_version: u8,
    pub(crate) components: Vec<ComponentState>,
    pub(crate) compatibility: Vec<CompatibilityReport>,
    pub(crate) destination: Option<String>,
    pub(crate) status: ItemStatus,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComponentState {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) status: ItemStatus,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentEnablePreview {
    pub(crate) target_id: TargetId,
    pub(crate) packages: Vec<InstallPreview>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceState {
    pub(crate) source_id: String,
    pub(crate) source_key: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) url: String,
    pub(crate) repository_key: Option<String>,
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
    pub(crate) catalog_message: Option<String>,
    pub(crate) repositories: Vec<RepositoryState>,
    pub(crate) sources: Vec<SourceState>,
    pub(crate) items: Vec<CatalogItemState>,
    pub(crate) agent_profiles: Vec<AgentProfileState>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListedSourceState {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) url: String,
    pub(crate) source_id: Option<String>,
    pub(crate) already_added: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RepositoryState {
    pub(crate) repository_id: String,
    pub(crate) repository_key: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) url: String,
    pub(crate) status: SourceStatus,
    pub(crate) refresh_failed: bool,
    pub(crate) message: Option<String>,
    pub(crate) revision: Option<String>,
    pub(crate) checked_at_epoch_seconds: u64,
    pub(crate) sources: Vec<ListedSourceState>,
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
pub(crate) struct PreparedRepository {
    pub(crate) token: String,
    pub(crate) repository_id: String,
    pub(crate) repository_key: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) url: String,
    pub(crate) revision: String,
    pub(crate) source_count: usize,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_state_json_includes_catalog_fields() {
        let state = AppState {
            checked_at_epoch_seconds: 1,
            auto_update_report: AutoUpdateReport::default(),
            catalog_message: None,
            repositories: Vec::new(),
            sources: vec![SourceState {
                source_id: "review".to_string(),
                source_key: "source-test".to_string(),
                name: "Review".to_string(),
                description: "Skills".to_string(),
                url: "https://nexus.example.com/repository/raw/sources/review-latest.zip"
                    .to_string(),
                repository_key: None,
                status: SourceStatus::Cached,
                refresh_failed: false,
                message: None,
                commit: None,
                checked_at_epoch_seconds: 1,
                catalog_errors: Vec::new(),
            }],
            items: vec![CatalogItemState {
                id: "review/python-standards".to_string(),
                local_id: "python-standards".to_string(),
                source_id: "review".to_string(),
                source_key: "source-test".to_string(),
                source_name: "Review".to_string(),
                source_url: "https://nexus.example.com/repository/raw/sources/review-latest.zip"
                    .to_string(),
                name: "Python standards".to_string(),
                description: "Python".to_string(),
                manual_invocation: false,
                source: "skills/python-standards".to_string(),
                source_is_directory: true,
                manifest_version: 2,
                components: vec![ComponentState {
                    id: "python-standards".to_string(),
                    kind: "skill".to_string(),
                    status: ItemStatus::Available,
                }],
                compatibility: Vec::new(),
                destination: None,
                status: ItemStatus::Available,
            }],
            agent_profiles: Vec::new(),
        };
        let value = serde_json::to_value(&state).expect("json");
        assert!(value
            .get("repositories")
            .and_then(serde_json::Value::as_array)
            .is_some());
        assert!(value["catalogMessage"].is_null());
        assert!(value["sources"][0]["repositoryKey"].is_null());
        assert!(value["sources"][0].get("locatorKind").is_none());
        assert!(value["items"][0].get("locatorKind").is_none());
        assert_eq!(value["items"][0]["components"][0]["status"], "available");
        assert_eq!(value["items"][0]["status"], "available");
    }
}
