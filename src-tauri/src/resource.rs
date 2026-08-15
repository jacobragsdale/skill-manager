//! Declarative resources shared by target adapters and the central executor.

use crate::ledger::OwnedPathKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "level")]
pub(crate) enum CapabilityResult {
    Native,
    LosslessTranslation,
    LossyTranslation {
        losses: Vec<String>,
    },
    Unsupported {
        reason: String,
    },
    Blocked {
        reason: String,
        required_action: String,
    },
}

impl CapabilityResult {
    pub(crate) fn is_supported(&self) -> bool {
        matches!(
            self,
            Self::Native | Self::LosslessTranslation | Self::LossyTranslation { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum StructuredFormat {
    Json,
    Jsonc,
    Toml,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PathMaterialization {
    Copy,
    AgentSkill { effective_name: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DesiredPath {
    pub(crate) path: PathBuf,
    pub(crate) kind: OwnedPathKind,
    pub(crate) source: PathBuf,
    pub(crate) source_digest: String,
    pub(crate) materialization: PathMaterialization,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DesiredStructuredEntry {
    pub(crate) document_path: PathBuf,
    pub(crate) format: StructuredFormat,
    pub(crate) key_path: Vec<String>,
    pub(crate) value: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DesiredTextBlock {
    pub(crate) document_path: PathBuf,
    pub(crate) marker_id: String,
    pub(crate) body: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DesiredResource {
    Path(DesiredPath),
    StructuredEntry(DesiredStructuredEntry),
    #[allow(dead_code)]
    TextBlock(DesiredTextBlock),
}

impl DesiredResource {
    pub(crate) fn identity(&self) -> String {
        match self {
            Self::Path(resource) => format!("path:{}", normalized_path(&resource.path)),
            Self::StructuredEntry(resource) => format!(
                "entry:{}:{}",
                normalized_path(&resource.document_path),
                resource.key_path.join(".")
            ),
            Self::TextBlock(resource) => format!(
                "block:{}:{}",
                normalized_path(&resource.document_path),
                resource.marker_id
            ),
        }
    }

    pub(crate) fn desired_digest(&self) -> Result<String, String> {
        let mut hasher = Sha256::new();
        match self {
            Self::Path(resource) => {
                hash_field(&mut hasher, resource.source_digest.as_bytes());
                match &resource.materialization {
                    PathMaterialization::Copy => hash_field(&mut hasher, b"copy"),
                    PathMaterialization::AgentSkill { effective_name } => {
                        hash_field(&mut hasher, b"skill");
                        hash_field(&mut hasher, effective_name.as_bytes());
                    }
                }
            }
            Self::StructuredEntry(resource) => {
                let bytes = serde_json::to_vec(&resource.value).map_err(|error| {
                    format!("Could not serialize a desired config entry: {error}")
                })?;
                hash_field(&mut hasher, &bytes);
            }
            Self::TextBlock(resource) => hash_field(&mut hasher, resource.body.as_bytes()),
        }
        Ok(hex_digest(hasher.finalize()))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BindingPlan {
    pub(crate) id: String,
    pub(crate) installation_id: String,
    pub(crate) component_id: String,
    pub(crate) target_id: String,
    pub(crate) dialect_id: String,
    pub(crate) scope: String,
    pub(crate) capability: CapabilityResult,
    pub(crate) resource_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompatibilityReport {
    pub(crate) component_id: String,
    pub(crate) target_id: String,
    pub(crate) capability: CapabilityResult,
}

#[derive(Clone, Debug)]
pub(crate) struct PlannedResource {
    pub(crate) id: String,
    pub(crate) desired: DesiredResource,
    pub(crate) consumer_binding_ids: Vec<String>,
    pub(crate) adapter_id: String,
    pub(crate) dialect_id: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct OperationPlan {
    pub(crate) bindings: BTreeMap<String, BindingPlan>,
    pub(crate) resources: BTreeMap<String, PlannedResource>,
    pub(crate) compatibility: Vec<CompatibilityReport>,
    pub(crate) warnings: Vec<String>,
}

impl OperationPlan {
    pub(crate) fn add_binding(&mut self, binding: BindingPlan) -> Result<(), String> {
        if self.bindings.insert(binding.id.clone(), binding).is_some() {
            return Err("An adapter produced a duplicate binding id.".to_string());
        }
        Ok(())
    }

    pub(crate) fn add_resource(
        &mut self,
        desired: DesiredResource,
        binding_id: &str,
        adapter_id: &str,
        dialect_id: &str,
    ) -> Result<String, String> {
        let identity = desired.identity();
        let desired_digest = desired.desired_digest()?;
        let resource_id = stable_id("resource", &identity);
        if let Some(existing) = self.resources.get_mut(&resource_id) {
            if existing.desired.identity() != identity
                || existing.desired.desired_digest()? != desired_digest
            {
                return Err(format!(
                    "Conflicting desired content targets physical resource {identity}."
                ));
            }
            if !existing
                .consumer_binding_ids
                .iter()
                .any(|consumer| consumer == binding_id)
            {
                existing.consumer_binding_ids.push(binding_id.to_string());
                existing.consumer_binding_ids.sort();
            }
            return Ok(resource_id);
        }
        self.resources.insert(
            resource_id.clone(),
            PlannedResource {
                id: resource_id.clone(),
                desired,
                consumer_binding_ids: vec![binding_id.to_string()],
                adapter_id: adapter_id.to_string(),
                dialect_id: dialect_id.to_string(),
            },
        );
        Ok(resource_id)
    }
}

pub(crate) fn stable_id(prefix: &str, value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{prefix}-{}", &hex_digest(digest)[..24])
}

fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn normalized_path(path: &std::path::Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
        .to_lowercase()
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let mut output = String::with_capacity(64);
    for byte in digest.as_ref() {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(source_digest: &str) -> DesiredResource {
        DesiredResource::Path(DesiredPath {
            path: PathBuf::from("/tmp/home/.agents/skills/acme-review"),
            kind: OwnedPathKind::Directory,
            source: PathBuf::from("/tmp/source/review"),
            source_digest: source_digest.to_string(),
            materialization: PathMaterialization::Copy,
        })
    }

    #[test]
    fn identical_physical_resources_coalesce_consumers() {
        let mut plan = OperationPlan::default();
        let first = plan
            .add_resource(path(&"a".repeat(64)), "cursor", "cursor", "cursor-current")
            .expect("first");
        let second = plan
            .add_resource(path(&"a".repeat(64)), "codex", "codex", "codex-current")
            .expect("second");
        assert_eq!(first, second);
        assert_eq!(
            plan.resources[&first].consumer_binding_ids,
            vec!["codex", "cursor"]
        );
    }

    #[test]
    fn different_content_at_one_identity_is_rejected() {
        let mut plan = OperationPlan::default();
        plan.add_resource(path(&"a".repeat(64)), "cursor", "cursor", "cursor-current")
            .expect("first");
        assert!(plan
            .add_resource(path(&"b".repeat(64)), "codex", "codex", "codex-current")
            .expect_err("collision")
            .contains("Conflicting desired content"));
    }
}
