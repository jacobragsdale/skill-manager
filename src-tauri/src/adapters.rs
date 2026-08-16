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
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(crate) struct PlanningContext<'a> {
    pub(crate) paths: &'a SystemPaths,
    pub(crate) source_root: &'a Path,
    pub(crate) share_agents_skills: bool,
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
enum ExclusiveSkillRoot {
    SharedAgentsOnly,
    Relative(&'static str),
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
    exclusive_skill: ExclusiveSkillRoot,
    reads_shared_agents: bool,
    unknown_dialect_allows_shared_skills: bool,
    mcp: McpMapping,
    sse_unsupported: bool,
}

impl TargetSpec {
    fn requires_shared_skills(self) -> bool {
        matches!(self.exclusive_skill, ExclusiveSkillRoot::SharedAgentsOnly)
    }

    fn skill_root(self, home: &Path, share_agents_skills: bool) -> PathBuf {
        if self.uses_shared_agents(share_agents_skills) {
            return home.join(".agents/skills");
        }
        match self.exclusive_skill {
            ExclusiveSkillRoot::SharedAgentsOnly => home.join(".agents/skills"),
            ExclusiveSkillRoot::Relative(relative) => home.join(relative),
        }
    }

    fn uses_shared_agents(self, share_agents_skills: bool) -> bool {
        share_agents_skills && self.reads_shared_agents
    }

    fn skill_capability(self, share_agents_skills: bool) -> CapabilityResult {
        if self.uses_shared_agents(share_agents_skills) && !self.requires_shared_skills() {
            CapabilityResult::LosslessTranslation
        } else {
            CapabilityResult::Native
        }
    }

    fn skill_display_root(self, share_agents_skills: bool) -> &'static str {
        if self.uses_shared_agents(share_agents_skills) {
            return "~/.agents/skills";
        }
        match self.exclusive_skill {
            ExclusiveSkillRoot::SharedAgentsOnly => "~/.agents/skills",
            ExclusiveSkillRoot::Relative(relative) => match relative {
                ".claude/skills" => "~/.claude/skills",
                ".cursor/skills" => "~/.cursor/skills",
                ".copilot/skills" => "~/.copilot/skills",
                ".grok/skills" => "~/.grok/skills",
                ".config/opencode/skills" => "~/.config/opencode/skills",
                _ => "~/.agents/skills",
            },
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
        exclusive_skill: ExclusiveSkillRoot::Relative(".cursor/skills"),
        reads_shared_agents: true,
        unknown_dialect_allows_shared_skills: true,
        mcp: McpMapping::StandardJson {
            relative: ".cursor/mcp.json",
            key: "mcpServers",
        },
        sse_unsupported: false,
    },
    TargetSpec {
        target_id: TargetId::ClaudeCode,
        exclusive_skill: ExclusiveSkillRoot::Relative(".claude/skills"),
        reads_shared_agents: false,
        unknown_dialect_allows_shared_skills: false,
        mcp: McpMapping::StandardJson {
            relative: ".claude.json",
            key: "mcpServers",
        },
        sse_unsupported: false,
    },
    TargetSpec {
        target_id: TargetId::Codex,
        exclusive_skill: ExclusiveSkillRoot::SharedAgentsOnly,
        reads_shared_agents: true,
        unknown_dialect_allows_shared_skills: true,
        mcp: McpMapping::Toml {
            relative: ".codex/config.toml",
            key: "mcp_servers",
        },
        sse_unsupported: true,
    },
    TargetSpec {
        target_id: TargetId::OpenCode,
        exclusive_skill: ExclusiveSkillRoot::Relative(".config/opencode/skills"),
        reads_shared_agents: true,
        unknown_dialect_allows_shared_skills: true,
        mcp: McpMapping::OpenCode,
        sse_unsupported: true,
    },
    TargetSpec {
        target_id: TargetId::GrokBuild,
        exclusive_skill: ExclusiveSkillRoot::Relative(".grok/skills"),
        reads_shared_agents: true,
        unknown_dialect_allows_shared_skills: true,
        mcp: McpMapping::Toml {
            relative: ".grok/config.toml",
            key: "mcp_servers",
        },
        sse_unsupported: true,
    },
    TargetSpec {
        target_id: TargetId::GithubCopilot,
        exclusive_skill: ExclusiveSkillRoot::Relative(".copilot/skills"),
        reads_shared_agents: true,
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
                && self.spec.unknown_dialect_allows_shared_skills
                && self.spec.uses_shared_agents(context.share_agents_skills);
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
            capability: self.spec.skill_capability(context.share_agents_skills),
            resources: vec![DesiredResource::Path(DesiredPath {
                path: self
                    .spec
                    .skill_root(&context.paths.home, context.share_agents_skills)
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

fn spec(target_id: TargetId) -> TargetSpec {
    SPECS
        .into_iter()
        .find(|candidate| candidate.target_id == target_id)
        .expect("every stable target has a built-in spec")
}

pub(crate) fn share_agents_skills(profiles: &[AgentProfile]) -> bool {
    let enabled = profiles
        .iter()
        .filter(|profile| profile.enabled)
        .map(|profile| profile.target_id)
        .collect::<BTreeSet<_>>();
    if enabled
        .iter()
        .any(|&target| spec(target).requires_shared_skills())
    {
        return true;
    }
    let readers = TargetId::ALL
        .into_iter()
        .filter(|&target| spec(target).reads_shared_agents)
        .collect::<BTreeSet<_>>();
    !readers.is_empty() && readers.iter().all(|target| enabled.contains(target))
}

pub(crate) fn shared_skill_readers() -> impl Iterator<Item = TargetId> {
    TargetId::ALL
        .into_iter()
        .filter(|&target| spec(target).reads_shared_agents)
}

pub(crate) fn reads_shared_agents(target_id: TargetId) -> bool {
    spec(target_id).reads_shared_agents
}

pub(crate) fn skill_display_root(target_id: TargetId, share: bool) -> &'static str {
    spec(target_id).skill_display_root(share)
}

pub(crate) fn managed_skill_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![home.join(".agents/skills")];
    for target in TargetId::ALL {
        let root = spec(target).skill_root(home, false);
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

pub(crate) fn shared_skill_leak_warning(profiles: &[AgentProfile]) -> Option<String> {
    if !share_agents_skills(profiles) {
        return None;
    }
    let enabled = profiles
        .iter()
        .filter(|profile| profile.enabled)
        .map(|profile| profile.target_id)
        .collect::<BTreeSet<_>>();
    let disabled = shared_skill_readers()
        .filter(|target| !enabled.contains(target))
        .map(TargetId::display_name)
        .collect::<Vec<_>>();
    if disabled.is_empty() {
        return None;
    }
    Some(format!(
        "Skills will be installed under ~/.agents/skills, which {} also read. Disabling those agents does not hide these skills from them.",
        join_names(&disabled)
    ))
}

pub(crate) fn claude_opencode_leak_warning(profiles: &[AgentProfile]) -> Option<String> {
    let claude_enabled = profiles
        .iter()
        .any(|profile| profile.target_id == TargetId::ClaudeCode && profile.enabled);
    let opencode_enabled = profiles
        .iter()
        .any(|profile| profile.target_id == TargetId::OpenCode && profile.enabled);
    if claude_enabled && !opencode_enabled {
        Some(
            "OpenCode also scans ~/.claude/skills, so disabling OpenCode does not hide Claude Code skills from it."
                .to_string(),
        )
    } else {
        None
    }
}

fn join_names(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [name] => (*name).to_string(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
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
            share_agents_skills: true,
        };
        let skill = CatalogComponent {
            id: "review".to_string(),
            kind: CatalogComponentKind::Skill,
            source: "skills/review".to_string(),
            source_is_directory: true,
            digest: "skill-digest".to_string(),
            effective_name: "acme-review".to_string(),
            description: "Review code.".to_string(),
            disable_model_invocation: false,
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
            description: "Runs node.".to_string(),
            disable_model_invocation: false,
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
    fn share_agents_skills_only_when_required_or_universal() {
        let cursor = AgentProfile {
            target_id: TargetId::Cursor,
            enabled: true,
            scopes: vec!["user".to_string()],
            dialect_id: TargetId::Cursor.current_dialect(),
        };
        assert!(!share_agents_skills(std::slice::from_ref(&cursor)));
        let codex = AgentProfile {
            target_id: TargetId::Codex,
            enabled: true,
            scopes: vec!["user".to_string()],
            dialect_id: TargetId::Codex.current_dialect(),
        };
        assert!(share_agents_skills(&[cursor, codex]));
        let all_readers = shared_skill_readers()
            .map(|target_id| AgentProfile {
                target_id,
                enabled: true,
                scopes: vec!["user".to_string()],
                dialect_id: target_id.current_dialect(),
            })
            .collect::<Vec<_>>();
        assert!(share_agents_skills(&all_readers));
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
            share_agents_skills: false,
        };
        let skill = CatalogComponent {
            id: "review".to_string(),
            kind: CatalogComponentKind::Skill,
            source: "skills/review".to_string(),
            source_is_directory: true,
            digest: "skill-digest".to_string(),
            effective_name: "acme-review".to_string(),
            description: "Review code.".to_string(),
            disable_model_invocation: false,
            mcp_server: None,
        };
        let mcp = CatalogComponent {
            id: "database".to_string(),
            kind: CatalogComponentKind::McpServer,
            source: "mcp/database.json".to_string(),
            source_is_directory: false,
            digest: "mcp-digest".to_string(),
            effective_name: "acme-database".to_string(),
            description: "Runs node.".to_string(),
            disable_model_invocation: false,
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
                paths.home.join(".cursor/skills/acme-review"),
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
                paths.home.join(".config/opencode/skills/acme-review"),
            ),
            (
                TargetId::GrokBuild,
                paths.home.join(".grok/skills/acme-review"),
            ),
            (
                TargetId::GithubCopilot,
                paths.home.join(".copilot/skills/acme-review"),
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
