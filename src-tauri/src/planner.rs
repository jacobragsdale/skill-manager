//! Pure package planning, adapter fan-out, coalescing, and structural preflight.

use crate::adapters::{adapter, PlanningContext};
use crate::agent_profiles::{self, AgentProfile};
use crate::catalog::{CatalogComponent, CatalogComponentKind, CatalogItem};
use crate::ledger::{InstallationLedger, InstallationRecord};
use crate::paths::SystemPaths;
use crate::resource::{
    stable_id, BindingPlan, CompatibilityReport, DesiredResource, OperationPlan,
};
use crate::source::{ConfiguredSource, SourceSnapshot};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResourcePreview {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) identity: String,
    pub(crate) consumers: Vec<String>,
    pub(crate) shared: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallPreview {
    pub(crate) installation_id: String,
    pub(crate) compatibility: Vec<CompatibilityReport>,
    pub(crate) resources: Vec<ResourcePreview>,
    pub(crate) warnings: Vec<String>,
    pub(crate) trust_tier: u8,
    pub(crate) requires_approval: bool,
    pub(crate) risk_details: Vec<String>,
}

pub(crate) fn plan(
    paths: &SystemPaths,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    profiles: Option<&[AgentProfile]>,
    component_ids: Option<&[String]>,
) -> Result<OperationPlan, String> {
    if let Some(profiles) = profiles {
        return plan_portable(paths, snapshot, item, profiles, component_ids);
    }
    let profiles = agent_profiles::read(paths)?;
    plan_portable(paths, snapshot, item, &profiles, component_ids)
}

pub(crate) fn package_component_ids(item: &CatalogItem) -> Vec<String> {
    item.components
        .iter()
        .map(|component| component.id.clone())
        .collect()
}

pub(crate) fn selected_component_ids(
    record: &InstallationRecord,
    item: &CatalogItem,
) -> Vec<String> {
    if record.selected_component_ids.is_empty() {
        return package_component_ids(item);
    }
    record
        .selected_component_ids
        .iter()
        .filter(|id| item.components.iter().any(|component| component.id == **id))
        .cloned()
        .collect()
}

pub(crate) fn validate_component_id(item: &CatalogItem, component_id: &str) -> Result<(), String> {
    if item
        .components
        .iter()
        .any(|component| component.id == component_id)
    {
        Ok(())
    } else {
        Err(format!("{} has no component {component_id}.", item.id))
    }
}

fn selected_components<'a>(
    item: &'a CatalogItem,
    component_ids: Option<&[String]>,
) -> Result<Vec<&'a CatalogComponent>, String> {
    let Some(component_ids) = component_ids else {
        return Ok(item.components.iter().collect());
    };
    if component_ids.is_empty() {
        return Ok(item.components.iter().collect());
    }
    let mut selected = Vec::new();
    for component_id in component_ids {
        validate_component_id(item, component_id)?;
        let component = item
            .components
            .iter()
            .find(|component| component.id == *component_id)
            .expect("validated");
        selected.push(component);
    }
    Ok(selected)
}

fn plan_portable(
    paths: &SystemPaths,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    profiles: &[AgentProfile],
    component_ids: Option<&[String]>,
) -> Result<OperationPlan, String> {
    let enabled = profiles
        .iter()
        .filter(|profile| profile.enabled)
        .collect::<Vec<_>>();
    if enabled.is_empty() {
        return Err("Enable at least one agent before installing a portable package.".to_string());
    }
    let context = PlanningContext {
        paths,
        source_root: &snapshot.path,
    };
    let components = selected_components(item, component_ids)?;
    if components.is_empty() {
        return Err(format!("{} has no components to install.", item.id));
    }
    let mut plan = OperationPlan::default();

    for profile in enabled {
        let target_adapter = adapter(profile.target_id);
        if target_adapter.target_id() != profile.target_id {
            return Err("The target registry returned the wrong adapter.".to_string());
        }
        for component in &components {
            let target_plan = target_adapter.plan(component, profile, &context)?;
            let target_id = profile.target_id.as_str().to_string();
            plan.compatibility.push(CompatibilityReport {
                component_id: component.id.clone(),
                target_id: target_id.clone(),
                capability: target_plan.capability.clone(),
            });
            plan.warnings.extend(target_plan.warnings);
            if !target_plan.capability.is_supported() {
                continue;
            }
            let binding_id = stable_id(
                "binding",
                &format!("{}:{}:{}:user", item.id, component.id, target_id),
            );
            let mut resource_ids = Vec::new();
            for resource in target_plan.resources {
                resource_ids.push(plan.add_resource(
                    resource,
                    &binding_id,
                    profile.target_id.as_str(),
                    &profile.dialect_id,
                )?);
            }
            plan.add_binding(BindingPlan {
                id: binding_id,
                installation_id: item.id.clone(),
                component_id: component.id.clone(),
                target_id,
                dialect_id: profile.dialect_id.clone(),
                scope: "user".to_string(),
                capability: target_plan.capability,
                resource_ids,
            })?;
        }
    }
    plan.warnings.sort();
    plan.warnings.dedup();
    validate_document_identity_collisions(&plan)?;
    Ok(plan)
}

fn validate_document_identity_collisions(plan: &OperationPlan) -> Result<(), String> {
    let mut path_resources = BTreeSet::new();
    let mut documents = BTreeSet::new();
    for resource in plan.resources.values() {
        match &resource.desired {
            DesiredResource::Path(path) => {
                path_resources.insert(normalize(&path.path));
            }
            DesiredResource::StructuredEntry(entry) => {
                documents.insert(normalize(&entry.document_path));
            }
            DesiredResource::TextBlock(block) => {
                documents.insert(normalize(&block.document_path));
            }
        }
    }
    if let Some(conflict) = path_resources.intersection(&documents).next() {
        return Err(format!(
            "A plan cannot own both a whole path and an entry inside {conflict}."
        ));
    }
    Ok(())
}

pub(crate) fn preflight_installed_conflicts(
    ledger: &InstallationLedger,
    source: &ConfiguredSource,
    item: &CatalogItem,
    plan: &OperationPlan,
) -> Result<(), String> {
    for conflict in &item.conflicts_with {
        if ledger.items.contains_key(conflict) {
            return Err(format!(
                "{} declares an incompatibility with installed package {conflict}.",
                item.id
            ));
        }
    }
    if let Some((installed_id, _)) = ledger
        .items
        .iter()
        .find(|(_, record)| record.conflicts_with.contains(&item.id))
    {
        return Err(format!(
            "Installed package {installed_id} declares an incompatibility with {}.",
            item.id
        ));
    }
    for resource in plan.resources.values() {
        if let Some(existing) = ledger.resource_by_identity(&resource.desired.identity()) {
            let desired = resource.desired.desired_digest()?;
            if existing.desired_digest != desired
                && ledger
                    .items
                    .get(&item.id)
                    .is_none_or(|record| record.source_key != source.source_key)
            {
                return Err(format!(
                    "{} is already owned by another installation.",
                    resource.desired.identity()
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn preview(item: &CatalogItem, plan: &OperationPlan) -> InstallPreview {
    let planned_ids = plan
        .compatibility
        .iter()
        .map(|entry| entry.component_id.as_str())
        .collect::<BTreeSet<_>>();
    let components = item
        .components
        .iter()
        .filter(|component| planned_ids.contains(component.id.as_str()))
        .collect::<Vec<_>>();
    let trust_tier = if components
        .iter()
        .any(|component| component.kind == CatalogComponentKind::McpServer)
    {
        3
    } else if components
        .iter()
        .any(|component| component.kind == CatalogComponentKind::Skill)
    {
        2
    } else {
        1
    };
    let mut risk_details = Vec::new();
    for component in components {
        let Some(server) = &component.mcp_server else {
            continue;
        };
        risk_details.push(match server {
            crate::mcp::McpServer::Stdio {
                command,
                args,
                env,
                cwd,
            } => format!(
                "MCP {}: command {:?}, args {:?}, cwd {:?}, environment names {:?}",
                component.effective_name,
                command,
                args,
                cwd,
                env.keys().collect::<Vec<_>>()
            ),
            crate::mcp::McpServer::StreamableHttp { url, headers }
            | crate::mcp::McpServer::Sse { url, headers } => format!(
                "MCP {}: URL {:?}, header names {:?}",
                component.effective_name,
                url,
                headers.keys().collect::<Vec<_>>()
            ),
        });
    }
    InstallPreview {
        installation_id: item.id.clone(),
        compatibility: plan.compatibility.clone(),
        resources: plan
            .resources
            .values()
            .map(|resource| ResourcePreview {
                id: resource.id.clone(),
                kind: match resource.desired {
                    DesiredResource::Path(_) => "ownedPath",
                    DesiredResource::StructuredEntry(_) => "ownedStructuredEntry",
                    DesiredResource::TextBlock(_) => "ownedTextBlock",
                }
                .to_string(),
                identity: resource.desired.identity(),
                consumers: resource.consumer_binding_ids.clone(),
                shared: resource.consumer_binding_ids.len() > 1,
            })
            .collect(),
        warnings: plan.warnings.clone(),
        trust_tier,
        requires_approval: item.manifest_version == 2 && trust_tier >= 3,
        risk_details,
    }
}

fn normalize(path: &std::path::Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_profiles::{AgentProfile, TargetId};
    use crate::catalog::{read_manifest_catalog, CatalogComponentKind, CatalogItem};
    use crate::source::{ConfiguredSource, SourceSnapshot, TEST_SOURCE_KEY};
    use std::fs;
    use std::path::Path;

    fn paths(root: &Path) -> SystemPaths {
        SystemPaths {
            home: root.join("home"),
            config: root.join("config"),
            data: root.join("data"),
            local_data: root.join("local-data"),
            cache: root.join("cache"),
        }
    }

    fn review_snapshot(root: &Path) -> (SourceSnapshot, CatalogItem) {
        let source_root = root.join("source");
        fs::create_dir_all(source_root.join("skills/review")).expect("skill");
        fs::write(
            source_root.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review code\n---\nBody\n",
        )
        .expect("skill file");
        fs::write(
            source_root.join("skill-manager.json"),
            r#"{
              "version":2,
              "source":{"id":"acme","name":"Acme","description":"Shared config."},
              "packages":[{"id":"review","components":[{"kind":"skill","path":"skills/review"}]}]
            }"#,
        )
        .expect("manifest");
        let catalog = read_manifest_catalog(&source_root, TEST_SOURCE_KEY).expect("catalog");
        let item = catalog.items["review"].clone();
        let mut definition = ConfiguredSource::test_fixture(
            "acme",
            "https://nexus.example.com/repository/raw/sources/acme-latest.zip",
        );
        definition.source_key = TEST_SOURCE_KEY.to_string();
        (
            SourceSnapshot {
                definition,
                commit: "a".repeat(40),
                path: source_root,
                catalog,
            },
            item,
        )
    }

    #[test]
    fn shared_skill_projection_coalesces_five_agents_skills_root() {
        let root = tempfile::tempdir().expect("root");
        let (snapshot, item) = review_snapshot(root.path());
        assert_eq!(item.components[0].kind, CatalogComponentKind::Skill);
        let enabled = [
            TargetId::Cursor,
            TargetId::Codex,
            TargetId::OpenCode,
            TargetId::GrokBuild,
            TargetId::GithubCopilot,
        ]
        .into_iter()
        .map(|target_id| AgentProfile {
            target_id,
            enabled: true,
            scopes: vec!["user".to_string()],
            dialect_id: target_id.current_dialect(),
        })
        .collect::<Vec<_>>();
        let plan =
            plan_portable(&paths(root.path()), &snapshot, &item, &enabled, None).expect("plan");
        assert_eq!(plan.resources.len(), 1);
        assert_eq!(
            plan.resources
                .values()
                .next()
                .expect("resource")
                .consumer_binding_ids
                .len(),
            5
        );
    }

    #[test]
    fn portable_planning_requires_an_enabled_agent() {
        let root = tempfile::tempdir().expect("root");
        let (snapshot, item) = review_snapshot(root.path());
        assert!(
            plan_portable(&paths(root.path()), &snapshot, &item, &[], None)
                .expect_err("no agents")
                .contains("Enable at least one agent")
        );
    }

    #[test]
    fn planning_a_component_subset_only_binds_that_component() {
        let root = tempfile::tempdir().expect("root");
        let source_root = root.path().join("source");
        fs::create_dir_all(source_root.join("skills/review")).expect("skill");
        fs::write(
            source_root.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review code\n---\nBody\n",
        )
        .expect("skill");
        fs::create_dir_all(source_root.join("mcp")).expect("mcp");
        fs::write(
            source_root.join("mcp/database.json"),
            r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json","mcpServers":{"database":{"type":"stdio","command":"node","args":["server.js"]}}}"#,
        )
        .expect("mcp");
        fs::write(
            source_root.join("skill-manager.json"),
            r#"{
              "version":2,
              "source":{"id":"acme","name":"Acme","description":"Shared config."},
              "packages":[{
                "id":"tools",
                "components":[
                  {"kind":"skill","id":"review","path":"skills/review"},
                  {"kind":"mcpServer","id":"database","path":"mcp/database.json"}
                ]
              }]
            }"#,
        )
        .expect("manifest");
        let catalog = read_manifest_catalog(&source_root, TEST_SOURCE_KEY).expect("catalog");
        let item = catalog.items["tools"].clone();
        let snapshot = SourceSnapshot {
            definition: ConfiguredSource::test_fixture(
                "acme",
                "https://nexus.example.com/repository/raw/sources/acme-latest.zip",
            ),
            commit: "a".repeat(40),
            path: source_root,
            catalog,
        };
        let enabled = [AgentProfile {
            target_id: TargetId::Cursor,
            enabled: true,
            scopes: vec!["user".to_string()],
            dialect_id: TargetId::Cursor.current_dialect(),
        }];
        let skill_only = plan_portable(
            &paths(root.path()),
            &snapshot,
            &item,
            &enabled,
            Some(&["review".to_string()]),
        )
        .expect("plan");
        assert!(skill_only
            .bindings
            .values()
            .all(|binding| binding.component_id == "review"));
        assert_eq!(skill_only.bindings.len(), 1);
        let preview = preview(&item, &skill_only);
        assert_eq!(preview.trust_tier, 2);
        assert!(!preview.requires_approval);
    }
}
