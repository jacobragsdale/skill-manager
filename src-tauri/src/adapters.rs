//! Compile-time target registry. Adapters translate components and never mutate the machine.

use crate::agent_plugin::McpServer;
use crate::agent_profiles::{AgentProfile, TargetId};
use crate::catalog_v1::{CatalogComponent, CatalogComponentKind};
use crate::install_v1::SystemPaths;
use crate::ledger::OwnedPathKind;
use crate::resource::{
    CapabilityResult, DesiredPath, DesiredResource, DesiredStructuredEntry, DesiredTextBlock,
    PathMaterialization, StructuredFormat,
};
use serde_json::{json, Map, Value};
use std::path::Path;

pub(crate) struct PlanningContext<'a> {
    pub(crate) paths: &'a SystemPaths,
    pub(crate) source_root: &'a Path,
    pub(crate) package_name: &'a str,
    pub(crate) package_is_plugin: bool,
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

struct BuiltInAdapter {
    target_id: TargetId,
}

impl TargetAdapter for BuiltInAdapter {
    fn target_id(&self) -> TargetId {
        self.target_id
    }

    fn plan(
        &self,
        component: &CatalogComponent,
        profile: &AgentProfile,
        context: &PlanningContext<'_>,
    ) -> Result<TargetPlan, String> {
        if profile.target_id != self.target_id {
            return Err("An adapter received a profile for a different target.".to_string());
        }
        if profile.dialect_id != self.target_id.current_dialect() {
            let shared_skill_is_stable = component.kind == CatalogComponentKind::Skill
                && matches!(
                    self.target_id,
                    TargetId::Cursor
                        | TargetId::Codex
                        | TargetId::OpenCode
                        | TargetId::GithubCopilot
                );
            if !shared_skill_is_stable {
                return Ok(TargetPlan::blocked(
                    format!(
                        "Dialect {} is not recognized by the built-in {} adapter.",
                        profile.dialect_id,
                        self.target_id.display_name()
                    ),
                    "Select a supported dialect after its configuration contract has been verified.",
                ));
            }
        }
        match component.kind {
            CatalogComponentKind::Skill => self.plan_skill(component, context),
            CatalogComponentKind::McpServer => self.plan_mcp(component, context),
            CatalogComponentKind::InstructionSet => self.plan_instructions(component, context),
            CatalogComponentKind::AgentPlugin => self.plan_plugin(component, context),
            CatalogComponentKind::LegacyFileTree => Ok(TargetPlan::unsupported(
                "Legacy v1 installs use their explicit destination.",
            )),
        }
    }
}

impl BuiltInAdapter {
    fn plan_skill(
        &self,
        component: &CatalogComponent,
        context: &PlanningContext<'_>,
    ) -> Result<TargetPlan, String> {
        let target_root = match self.target_id {
            TargetId::Cursor | TargetId::Codex | TargetId::OpenCode | TargetId::GithubCopilot => {
                context.paths.home.join(".agents/skills")
            }
            TargetId::ClaudeCode => context.paths.home.join(".claude/skills"),
            TargetId::GrokBuild => context.paths.home.join(".grok/skills"),
        };
        let capability = match self.target_id {
            TargetId::Cursor | TargetId::Codex | TargetId::OpenCode | TargetId::GithubCopilot => {
                CapabilityResult::LosslessTranslation
            }
            TargetId::ClaudeCode | TargetId::GrokBuild => CapabilityResult::Native,
        };
        Ok(TargetPlan {
            capability,
            resources: vec![DesiredResource::Path(DesiredPath {
                path: target_root.join(&component.effective_name),
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

    fn plan_plugin(
        &self,
        component: &CatalogComponent,
        context: &PlanningContext<'_>,
    ) -> Result<TargetPlan, String> {
        let destination = match self.target_id {
            TargetId::Cursor => context.paths.cursor_plugin_dir(context.package_name),
            TargetId::GithubCopilot => context.paths.copilot_plugin_dir(context.package_name),
            TargetId::ClaudeCode | TargetId::Codex | TargetId::OpenCode | TargetId::GrokBuild => {
                return Ok(TargetPlan {
                    capability: CapabilityResult::LosslessTranslation,
                    resources: Vec::new(),
                    warnings: vec![
                        "The portable package is projected as its skill and MCP components for this target."
                            .to_string(),
                    ],
                });
            }
        };
        Ok(TargetPlan {
            capability: CapabilityResult::Native,
            resources: vec![DesiredResource::Path(DesiredPath {
                path: destination,
                kind: OwnedPathKind::Directory,
                source: context.source_root.join(&component.source),
                source_digest: component.digest.clone(),
                materialization: PathMaterialization::AgentPlugin {
                    plugin_data: context.paths.plugin_data_dir(context.package_name),
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
        let server = if context.package_is_plugin {
            let plugin_root = context
                .paths
                .home
                .join(".agents/plugins")
                .join(context.package_name);
            server.expand_placeholders(
                &plugin_root,
                &context.paths.plugin_data_dir(context.package_name),
            )
        } else {
            server.clone()
        };
        if matches!(server, McpServer::Sse { .. })
            && matches!(
                self.target_id,
                TargetId::Codex | TargetId::OpenCode | TargetId::GrokBuild
            )
        {
            return Ok(TargetPlan::unsupported(
                "This target dialect does not expose a distinct legacy SSE transport.",
            ));
        }
        let (document_path, format, key_root, value) = match self.target_id {
            TargetId::Cursor => (
                context.paths.home.join(".cursor/mcp.json"),
                StructuredFormat::Json,
                "mcpServers",
                standard_mcp_value(&server)?,
            ),
            TargetId::ClaudeCode => (
                context.paths.home.join(".claude.json"),
                StructuredFormat::Json,
                "mcpServers",
                standard_mcp_value(&server)?,
            ),
            TargetId::Codex => (
                context.paths.home.join(".codex/config.toml"),
                StructuredFormat::Toml,
                "mcp_servers",
                toml_mcp_value(&server)?,
            ),
            TargetId::OpenCode => (
                context.paths.home.join(".config/opencode/opencode.jsonc"),
                StructuredFormat::Jsonc,
                "mcp",
                opencode_mcp_value(&server),
            ),
            TargetId::GrokBuild => (
                context.paths.home.join(".grok/config.toml"),
                StructuredFormat::Toml,
                "mcp_servers",
                toml_mcp_value(&server)?,
            ),
            TargetId::GithubCopilot => (
                context.paths.home.join(".copilot/mcp-config.json"),
                StructuredFormat::Json,
                "mcpServers",
                standard_mcp_value(&server)?,
            ),
        };
        Ok(TargetPlan {
            capability: CapabilityResult::LosslessTranslation,
            resources: vec![DesiredResource::StructuredEntry(
                DesiredStructuredEntry {
                    document_path,
                    format,
                    key_path: vec![key_root.to_string(), component.effective_name.clone()],
                    value,
                },
            )],
            warnings: vec![
                "This MCP server may start a local process or access a remote service when the target uses it."
                    .to_string(),
            ],
        })
    }

    fn plan_instructions(
        &self,
        component: &CatalogComponent,
        context: &PlanningContext<'_>,
    ) -> Result<TargetPlan, String> {
        let document_path = match self.target_id {
            TargetId::ClaudeCode => context.paths.home.join(".claude/CLAUDE.md"),
            TargetId::Codex => context.paths.home.join(".codex/AGENTS.md"),
            TargetId::OpenCode => context.paths.home.join(".config/opencode/AGENTS.md"),
            TargetId::Cursor => {
                return Ok(TargetPlan::unsupported(
                    "Cursor user rules are managed through Customize and have no documented writable user file.",
                ));
            }
            TargetId::GrokBuild => {
                return Ok(TargetPlan::unsupported(
                    "The pinned Grok Build dialect documents project instructions, not a user-scoped instruction file.",
                ));
            }
            TargetId::GithubCopilot => {
                return Ok(TargetPlan::unsupported(
                    "The pinned Copilot dialect has no portable user-scoped always-on instruction mapping.",
                ));
            }
        };
        let body = component.instruction_body.clone().ok_or_else(|| {
            format!(
                "Instruction component {} has no Markdown body.",
                component.id
            )
        })?;
        Ok(TargetPlan {
            capability: CapabilityResult::LosslessTranslation,
            resources: vec![DesiredResource::TextBlock(DesiredTextBlock {
                document_path,
                marker_id: component.effective_name.clone(),
                body,
            })],
            warnings: vec![
                "This always-on instruction is appended as a marked contribution after existing user text."
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
    BuiltInAdapter {
        target_id: TargetId::Cursor,
    },
    BuiltInAdapter {
        target_id: TargetId::ClaudeCode,
    },
    BuiltInAdapter {
        target_id: TargetId::Codex,
    },
    BuiltInAdapter {
        target_id: TargetId::OpenCode,
    },
    BuiltInAdapter {
        target_id: TargetId::GrokBuild,
    },
    BuiltInAdapter {
        target_id: TargetId::GithubCopilot,
    },
];

pub(crate) fn adapter(target_id: TargetId) -> &'static dyn TargetAdapter {
    ADAPTERS
        .iter()
        .find(|adapter| adapter.target_id == target_id)
        .expect("every stable target has a built-in adapter")
}

pub(crate) fn plugin_storage_resource(
    context: &PlanningContext<'_>,
    component: &CatalogComponent,
) -> DesiredResource {
    DesiredResource::Path(DesiredPath {
        path: context
            .paths
            .home
            .join(".agents/plugins")
            .join(context.package_name),
        kind: OwnedPathKind::Directory,
        source: context.source_root.join(&component.source),
        source_digest: component.digest.clone(),
        materialization: PathMaterialization::AgentPlugin {
            plugin_data: context.paths.plugin_data_dir(context.package_name),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_v1::{CatalogComponent, CatalogComponentKind};
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
            package_name: "acme-tools",
            package_is_plugin: false,
        };
        let skill = CatalogComponent {
            id: "review".to_string(),
            kind: CatalogComponentKind::Skill,
            source: "skills/review".to_string(),
            source_is_directory: true,
            digest: "skill-digest".to_string(),
            effective_name: "acme-review".to_string(),
            mcp_server: None,
            instruction_body: None,
            topics: Vec::new(),
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
            instruction_body: None,
            topics: Vec::new(),
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
}
