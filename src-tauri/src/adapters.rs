//! Compile-time target registry. Adapters translate components and never mutate the machine.

use crate::agent_profiles::{AgentProfile, TargetId};
use crate::catalog::{CatalogComponent, CatalogComponentKind};
use crate::ledger::OwnedPathKind;
use crate::mcp::McpServer;
use crate::paths::SystemPaths;
use crate::resource::{
    CapabilityResult, DesiredPath, DesiredResource, DesiredStructuredEntry, PathMaterialization,
    StructuredFormat,
};
use serde_json::{json, Map, Value};
use std::path::Path;

pub(crate) struct PlanningContext<'a> {
    pub(crate) paths: &'a SystemPaths,
    pub(crate) source_root: &'a Path,
}

pub(crate) struct TargetPlan {
    pub(crate) capability: CapabilityResult,
    pub(crate) resources: Vec<DesiredResource>,
    pub(crate) warnings: Vec<String>,
}

impl TargetPlan {
    fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            capability: CapabilityResult::Unsupported {
                reason: reason.into(),
            },
            resources: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn blocked(reason: impl Into<String>, required_action: impl Into<String>) -> Self {
        Self {
            capability: CapabilityResult::Blocked {
                reason: reason.into(),
                required_action: required_action.into(),
            },
            resources: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

pub(crate) trait TargetAdapter: Sync {
    fn target_id(&self) -> TargetId;

    fn plan(
        &self,
        component: &CatalogComponent,
        profile: &AgentProfile,
        context: &PlanningContext<'_>,
    ) -> Result<TargetPlan, String>;
}

#[derive(Clone, Copy)]
enum SkillProjection {
    NativeClaude,
    SharedAgents,
}

#[derive(Clone, Copy)]
enum McpMapping {
    StandardJson {
        relative: &'static str,
        key: &'static str,
    },
    Toml {
        relative: &'static str,
        key: &'static str,
    },
    OpenCode,
}

#[derive(Clone, Copy)]
struct TargetSpec {
    target_id: TargetId,
    skill: SkillProjection,
    unknown_dialect_allows_shared_skills: bool,
    mcp: McpMapping,
    sse_unsupported: bool,
}

impl TargetSpec {
    fn skill_root(self, home: &Path) -> std::path::PathBuf {
        match self.skill {
            SkillProjection::NativeClaude => home.join(".claude/skills"),
            SkillProjection::SharedAgents => home.join(".agents/skills"),
        }
    }

    fn skill_capability(self) -> CapabilityResult {
        match self.skill {
            SkillProjection::NativeClaude => CapabilityResult::Native,
            SkillProjection::SharedAgents => CapabilityResult::LosslessTranslation,
        }
    }

    fn mcp_document(self, home: &Path) -> (std::path::PathBuf, StructuredFormat, &'static str) {
        match self.mcp {
            McpMapping::StandardJson { relative, key } => {
                (home.join(relative), StructuredFormat::Json, key)
            }
            McpMapping::Toml { relative, key } => {
                (home.join(relative), StructuredFormat::Toml, key)
            }
            McpMapping::OpenCode => (
                home.join(".config/opencode/opencode.jsonc"),
                StructuredFormat::Jsonc,
                "mcp",
            ),
        }
    }

    fn mcp_value(self, server: &McpServer) -> Result<Value, String> {
        match self.mcp {
            McpMapping::StandardJson { .. } => standard_mcp_value(server),
            McpMapping::Toml { .. } => toml_mcp_value(server),
            McpMapping::OpenCode => Ok(opencode_mcp_value(server)),
        }
    }
}

const SPECS: [TargetSpec; 6] = [
    TargetSpec {
        target_id: TargetId::Cursor,
        skill: SkillProjection::SharedAgents,
        unknown_dialect_allows_shared_skills: true,
        mcp: McpMapping::StandardJson {
            relative: ".cursor/mcp.json",
            key: "mcpServers",
        },
        sse_unsupported: false,
    },
    TargetSpec {
        target_id: TargetId::ClaudeCode,
        skill: SkillProjection::NativeClaude,
        unknown_dialect_allows_shared_skills: false,
        mcp: McpMapping::StandardJson {
            relative: ".claude.json",
            key: "mcpServers",
        },
        sse_unsupported: false,
    },
    TargetSpec {
        target_id: TargetId::Codex,
        skill: SkillProjection::SharedAgents,
        unknown_dialect_allows_shared_skills: true,
        mcp: McpMapping::Toml {
            relative: ".codex/config.toml",
            key: "mcp_servers",
        },
        sse_unsupported: true,
    },
    TargetSpec {
        target_id: TargetId::OpenCode,
        skill: SkillProjection::SharedAgents,
        unknown_dialect_allows_shared_skills: true,
        mcp: McpMapping::OpenCode,
        sse_unsupported: true,
    },
    TargetSpec {
        target_id: TargetId::GrokBuild,
        skill: SkillProjection::SharedAgents,
        unknown_dialect_allows_shared_skills: true,
        mcp: McpMapping::Toml {
            relative: ".grok/config.toml",
            key: "mcp_servers",
        },
        sse_unsupported: true,
    },
    TargetSpec {
        target_id: TargetId::GithubCopilot,
        skill: SkillProjection::SharedAgents,
        unknown_dialect_allows_shared_skills: true,
        mcp: McpMapping::StandardJson {
            relative: ".copilot/mcp-config.json",
            key: "mcpServers",
        },
        sse_unsupported: false,
    },
];

struct BuiltInAdapter {
    spec: TargetSpec,
}

impl TargetAdapter for BuiltInAdapter {
    fn target_id(&self) -> TargetId {
        self.spec.target_id
    }

    fn plan(
        &self,
        component: &CatalogComponent,
        profile: &AgentProfile,
        context: &PlanningContext<'_>,
    ) -> Result<TargetPlan, String> {
        if profile.target_id != self.spec.target_id {
            return Err("An adapter received a profile for a different target.".to_string());
        }
        if profile.dialect_id != self.spec.target_id.current_dialect() {
            let shared_skill_is_stable = component.kind == CatalogComponentKind::Skill
                && self.spec.unknown_dialect_allows_shared_skills;
            if !shared_skill_is_stable {
                return Ok(TargetPlan::blocked(
                    format!(
                        "Dialect {} is not recognized by the built-in {} adapter.",
                        profile.dialect_id,
                        self.spec.target_id.display_name()
                    ),
                    "Select a supported dialect after its configuration contract has been verified.",
                ));
            }
        }
        match component.kind {
            CatalogComponentKind::Skill => self.plan_skill(component, context),
            CatalogComponentKind::McpServer => self.plan_mcp(component, context),
        }
    }
}

impl BuiltInAdapter {
    fn plan_skill(
        &self,
        component: &CatalogComponent,
        context: &PlanningContext<'_>,
    ) -> Result<TargetPlan, String> {
        Ok(TargetPlan {
            capability: self.spec.skill_capability(),
            resources: vec![DesiredResource::Path(DesiredPath {
                path: self
                    .spec
                    .skill_root(&context.paths.home)
                    .join(&component.effective_name),
                kind: OwnedPathKind::Directory,
                source: context.source_root.join(&component.source),
                source_digest: component.digest.clone(),
                materialization: PathMaterialization::AgentSkill {
                    effective_name: component.effective_name.clone(),
                },
            })],
            warnings: Vec::new(),
        })
    }

    fn plan_mcp(
        &self,
        component: &CatalogComponent,
        context: &PlanningContext<'_>,
    ) -> Result<TargetPlan, String> {
        let server = component
            .mcp_server
            .as_ref()
            .ok_or_else(|| format!("MCP component {} has no server definition.", component.id))?;
        if matches!(server, McpServer::Sse { .. }) && self.spec.sse_unsupported {
            return Ok(TargetPlan::unsupported(
                "This target dialect does not expose a distinct legacy SSE transport.",
            ));
        }
        let (document_path, format, key_root) = self.spec.mcp_document(&context.paths.home);
        let value = self.spec.mcp_value(server)?;
        Ok(TargetPlan {
            capability: CapabilityResult::LosslessTranslation,
            resources: vec![DesiredResource::StructuredEntry(DesiredStructuredEntry {
                document_path,
                format,
                key_path: vec![key_root.to_string(), component.effective_name.clone()],
                value,
            })],
            warnings: vec![
                "This MCP server may start a local process or access a remote service when the target uses it."
                    .to_string(),
            ],
        })
    }
}

fn standard_mcp_value(server: &McpServer) -> Result<Value, String> {
    serde_json::to_value(server)
        .map_err(|error| format!("Could not serialize a portable MCP server: {error}"))
}

fn toml_mcp_value(server: &McpServer) -> Result<Value, String> {
    let value = standard_mcp_value(server)?;
    let mut object = value
        .as_object()
        .cloned()
        .ok_or_else(|| "A portable MCP server did not serialize as an object.".to_string())?;
    object.remove("type");
    Ok(Value::Object(object))
}

fn opencode_mcp_value(server: &McpServer) -> Value {
    match server {
        McpServer::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let mut object = Map::from_iter([
                ("type".to_string(), Value::String("local".to_string())),
                (
                    "command".to_string(),
                    Value::Array(
                        std::iter::once(command)
                            .chain(args)
                            .cloned()
                            .map(Value::String)
                            .collect(),
                    ),
                ),
                ("enabled".to_string(), Value::Bool(true)),
            ]);
            if !env.is_empty() {
                object.insert(
                    "environment".to_string(),
                    serde_json::to_value(env).unwrap_or_else(|_| json!({})),
                );
            }
            if let Some(cwd) = cwd {
                object.insert("cwd".to_string(), Value::String(cwd.clone()));
            }
            Value::Object(object)
        }
        McpServer::StreamableHttp { url, headers } | McpServer::Sse { url, headers } => {
            let mut object = Map::from_iter([
                ("type".to_string(), Value::String("remote".to_string())),
                ("url".to_string(), Value::String(url.clone())),
                ("enabled".to_string(), Value::Bool(true)),
            ]);
            if !headers.is_empty() {
                object.insert(
                    "headers".to_string(),
                    serde_json::to_value(headers).unwrap_or_else(|_| json!({})),
                );
            }
            Value::Object(object)
        }
    }
}

static ADAPTERS: [BuiltInAdapter; 6] = [
    BuiltInAdapter { spec: SPECS[0] },
    BuiltInAdapter { spec: SPECS[1] },
    BuiltInAdapter { spec: SPECS[2] },
    BuiltInAdapter { spec: SPECS[3] },
    BuiltInAdapter { spec: SPECS[4] },
    BuiltInAdapter { spec: SPECS[5] },
];

pub(crate) fn adapter(target_id: TargetId) -> &'static dyn TargetAdapter {
    ADAPTERS
        .iter()
        .find(|adapter| adapter.spec.target_id == target_id)
        .expect("every stable target has a built-in adapter")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CatalogComponent, CatalogComponentKind};
    use std::collections::BTreeMap;

    #[test]
    fn opencode_stdio_mapping_preserves_command_cwd_and_environment() {
        let server = McpServer::Stdio {
            command: "node".to_string(),
            args: vec!["server.js".to_string()],
            env: BTreeMap::from([("MODE".to_string(), "safe".to_string())]),
            cwd: Some("/tmp/plugin".to_string()),
        };
        assert_eq!(
            opencode_mcp_value(&server),
            json!({
                "type": "local",
                "command": ["node", "server.js"],
                "enabled": true,
                "environment": {"MODE": "safe"},
                "cwd": "/tmp/plugin"
            })
        );
    }

    #[test]
    fn unknown_dialect_blocks_shared_config_but_allows_shared_skills() {
        let root = tempfile::tempdir().expect("root");
        let paths = SystemPaths {
            home: root.path().join("home"),
            config: root.path().join("config"),
            data: root.path().join("data"),
            local_data: root.path().join("local-data"),
            cache: root.path().join("cache"),
        };
        let profile = AgentProfile {
            target_id: TargetId::Codex,
            enabled: true,
            scopes: vec!["user".to_string()],
            dialect_id: "codex-future".to_string(),
        };
        let context = PlanningContext {
            paths: &paths,
            source_root: root.path(),
        };
        let skill = CatalogComponent {
            id: "review".to_string(),
            kind: CatalogComponentKind::Skill,
            source: "skills/review".to_string(),
            source_is_directory: true,
            digest: "skill-digest".to_string(),
            effective_name: "acme-review".to_string(),
            mcp_server: None,
        };
        let skill_plan = adapter(TargetId::Codex)
            .plan(&skill, &profile, &context)
            .expect("skill plan");
        assert!(skill_plan.capability.is_supported());
        assert_eq!(skill_plan.resources.len(), 1);

        let mcp = CatalogComponent {
            id: "database".to_string(),
            kind: CatalogComponentKind::McpServer,
            source: "mcp/database.json".to_string(),
            source_is_directory: false,
            digest: "mcp-digest".to_string(),
            effective_name: "acme-database".to_string(),
            mcp_server: Some(McpServer::Stdio {
                command: "node".to_string(),
                args: vec!["server.js".to_string()],
                env: BTreeMap::new(),
                cwd: None,
            }),
        };
        let mcp_plan = adapter(TargetId::Codex)
            .plan(&mcp, &profile, &context)
            .expect("MCP plan");
        assert!(matches!(
            mcp_plan.capability,
            CapabilityResult::Blocked { .. }
        ));
        assert!(mcp_plan.resources.is_empty());
    }

    #[test]
    fn every_target_projects_the_current_skill_and_mcp_identities() {
        let root = tempfile::tempdir().expect("root");
        let paths = SystemPaths {
            home: root.path().join("home"),
            config: root.path().join("config"),
            data: root.path().join("data"),
            local_data: root.path().join("local-data"),
            cache: root.path().join("cache"),
        };
        let context = PlanningContext {
            paths: &paths,
            source_root: root.path(),
        };
        let skill = CatalogComponent {
            id: "review".to_string(),
            kind: CatalogComponentKind::Skill,
            source: "skills/review".to_string(),
            source_is_directory: true,
            digest: "skill-digest".to_string(),
            effective_name: "acme-review".to_string(),
            mcp_server: None,
        };
        let mcp = CatalogComponent {
            id: "database".to_string(),
            kind: CatalogComponentKind::McpServer,
            source: "mcp/database.json".to_string(),
            source_is_directory: false,
            digest: "mcp-digest".to_string(),
            effective_name: "acme-database".to_string(),
            mcp_server: Some(McpServer::Stdio {
                command: "node".to_string(),
                args: vec!["server.js".to_string()],
                env: BTreeMap::new(),
                cwd: None,
            }),
        };
        let expected_skill = [
            (
                TargetId::Cursor,
                paths.home.join(".agents/skills/acme-review"),
            ),
            (
                TargetId::ClaudeCode,
                paths.home.join(".claude/skills/acme-review"),
            ),
            (
                TargetId::Codex,
                paths.home.join(".agents/skills/acme-review"),
            ),
            (
                TargetId::OpenCode,
                paths.home.join(".agents/skills/acme-review"),
            ),
            (
                TargetId::GrokBuild,
                paths.home.join(".agents/skills/acme-review"),
            ),
            (
                TargetId::GithubCopilot,
                paths.home.join(".agents/skills/acme-review"),
            ),
        ];
        let expected_mcp = [
            (
                TargetId::Cursor,
                paths.home.join(".cursor/mcp.json"),
                StructuredFormat::Json,
                vec!["mcpServers".to_string(), "acme-database".to_string()],
            ),
            (
                TargetId::ClaudeCode,
                paths.home.join(".claude.json"),
                StructuredFormat::Json,
                vec!["mcpServers".to_string(), "acme-database".to_string()],
            ),
            (
                TargetId::Codex,
                paths.home.join(".codex/config.toml"),
                StructuredFormat::Toml,
                vec!["mcp_servers".to_string(), "acme-database".to_string()],
            ),
            (
                TargetId::OpenCode,
                paths.home.join(".config/opencode/opencode.jsonc"),
                StructuredFormat::Jsonc,
                vec!["mcp".to_string(), "acme-database".to_string()],
            ),
            (
                TargetId::GrokBuild,
                paths.home.join(".grok/config.toml"),
                StructuredFormat::Toml,
                vec!["mcp_servers".to_string(), "acme-database".to_string()],
            ),
            (
                TargetId::GithubCopilot,
                paths.home.join(".copilot/mcp-config.json"),
                StructuredFormat::Json,
                vec!["mcpServers".to_string(), "acme-database".to_string()],
            ),
        ];
        assert_eq!(SPECS.len(), TargetId::ALL.len());
        for (target_id, skill_path) in expected_skill {
            let profile = AgentProfile {
                target_id,
                enabled: true,
                scopes: vec!["user".to_string()],
                dialect_id: target_id.current_dialect(),
            };
            let plan = adapter(target_id)
                .plan(&skill, &profile, &context)
                .expect("skill plan");
            match &plan.resources[0] {
                DesiredResource::Path(path) => assert_eq!(path.path, skill_path),
                other => panic!("expected skill path, got {other:?}"),
            }
        }
        for (target_id, document, format, key_path) in expected_mcp {
            let profile = AgentProfile {
                target_id,
                enabled: true,
                scopes: vec!["user".to_string()],
                dialect_id: target_id.current_dialect(),
            };
            let plan = adapter(target_id)
                .plan(&mcp, &profile, &context)
                .expect("mcp plan");
            match &plan.resources[0] {
                DesiredResource::StructuredEntry(entry) => {
                    assert_eq!(entry.document_path, document);
                    assert_eq!(entry.format, format);
                    assert_eq!(entry.key_path, key_path);
                }
                other => panic!("expected MCP entry, got {other:?}"),
            }
        }
    }
}
