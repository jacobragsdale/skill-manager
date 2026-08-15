//! Agent Plugins v1.0.0 parser, validator, and placeholder expansion.
//!
//! See https://agent-plugins.org/specification for normative requirements.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

pub const PLUGIN_MANIFEST_FILE: &str = "plugin.json";
pub const MCP_MANIFEST_FILE: &str = "mcp.json";
pub const PLUGIN_SCHEMA_V1: &str =
    "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
pub const MCP_SCHEMA_V1: &str =
    "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginAuthor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<PluginAuthor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum McpServer {
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    StreamableHttp {
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
    Sse {
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct McpConfig {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub mcp_servers: BTreeMap<String, McpServer>,
}

#[derive(Clone, Debug)]
pub struct ParsedAgentPlugin {
    pub manifest: PluginManifest,
    pub mcp_config: Option<McpConfig>,
    pub skill_names: Vec<String>,
}

impl PluginManifest {
    pub fn from_slice(contents: &[u8]) -> Result<Self, String> {
        let value = serde_json::from_slice::<serde_json::Value>(contents)
            .map_err(|e| format!("Could not parse plugin.json: {e}"))?;
        let manifest = serde_json::from_value::<PluginManifest>(value)
            .map_err(|e| format!("Invalid plugin.json: {e}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != PLUGIN_SCHEMA_V1 {
            return Err(format!(
                "plugin.json declared unsupported $schema {:?}; expected {PLUGIN_SCHEMA_V1}",
                self.schema
            ));
        }
        validate_plugin_name(&self.name)?;
        if let Some(desc) = &self.description {
            if desc.chars().count() > 1024 {
                return Err("plugin.json description exceeds 1024 characters.".to_string());
            }
        }
        if let Some(author) = &self.author {
            if let Some(name) = &author.name {
                if name.is_empty() {
                    return Err("plugin.json author.name cannot be empty.".to_string());
                }
            }
        }
        Ok(())
    }
}

pub fn validate_plugin_name(name: &str) -> Result<(), String> {
    let count = name.chars().count();
    if !(1..=64).contains(&count) {
        return Err(format!(
            "Plugin name must be between 1 and 64 characters: {name:?}"
        ));
    }
    let first = name.chars().next().unwrap();
    let last = name.chars().last().unwrap();
    if !first.is_ascii_alphanumeric() || !last.is_ascii_alphanumeric() {
        return Err(format!(
            "Plugin name must start and end with an alphanumeric character: {name:?}"
        ));
    }
    if name.contains("--") || name.contains("..") {
        return Err(format!(
            "Plugin name cannot contain consecutive hyphens or periods: {name:?}"
        ));
    }
    for c in name.chars() {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' && c != '.' {
            return Err(format!(
                "Plugin name can only contain lowercase alphanumeric characters, hyphens, and periods: {name:?}"
            ));
        }
    }
    Ok(())
}

impl McpConfig {
    pub fn from_slice(contents: &[u8]) -> Result<Self, String> {
        let value = serde_json::from_slice::<serde_json::Value>(contents)
            .map_err(|e| format!("Could not parse mcp.json: {e}"))?;
        let config = serde_json::from_value::<McpConfig>(value)
            .map_err(|e| format!("Invalid mcp.json: {e}"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != MCP_SCHEMA_V1 {
            return Err(format!(
                "mcp.json declared unsupported $schema {:?}; expected {MCP_SCHEMA_V1}",
                self.schema
            ));
        }
        for (server_name, server) in &self.mcp_servers {
            validate_server_name(server_name)?;
            server.validate(server_name)?;
        }
        Ok(())
    }
}

fn validate_server_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err(format!("Invalid MCP server name: {name:?}"));
    }
    Ok(())
}

impl McpServer {
    pub fn validate(&self, name: &str) -> Result<(), String> {
        match self {
            McpServer::Stdio {
                command,
                args: _,
                env,
                cwd,
            } => {
                if command.is_empty() {
                    return Err(format!("MCP server {name:?} command cannot be empty."));
                }
                if command.contains('\n') || command.contains('\r') {
                    return Err(format!(
                        "MCP server {name:?} command cannot contain newlines."
                    ));
                }
                if env.contains_key("PLUGIN_ROOT") || env.contains_key("PLUGIN_DATA") {
                    return Err(format!(
                        "MCP server {name:?} env cannot contain reserved PLUGIN_ROOT or PLUGIN_DATA keys."
                    ));
                }
                if let Some(cwd_val) = cwd {
                    if !cwd_val.starts_with("./")
                        && cwd_val != "${PLUGIN_ROOT}"
                        && !cwd_val.starts_with("${PLUGIN_ROOT}/")
                        && cwd_val != "${PLUGIN_DATA}"
                        && !cwd_val.starts_with("${PLUGIN_DATA}/")
                    {
                        return Err(format!(
                            "MCP server {name:?} cwd must start with ./, ${{PLUGIN_ROOT}}, or ${{PLUGIN_DATA}} (found {cwd_val:?})."
                        ));
                    }
                }
            }
            McpServer::StreamableHttp { url, headers } | McpServer::Sse { url, headers } => {
                if !url.starts_with("https://") && !url.starts_with("http://localhost") && !url.starts_with("http://127.0.0.1") {
                    return Err(format!(
                        "MCP server {name:?} remote URL must use HTTPS or localhost: {url:?}"
                    ));
                }
                let mut seen_headers = BTreeSet::new();
                for header in headers.keys() {
                    let lower = header.to_ascii_lowercase();
                    if !seen_headers.insert(lower) {
                        return Err(format!(
                            "MCP server {name:?} has duplicate header (case-insensitive): {header:?}"
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn expand_placeholders(&self, plugin_root: &Path, plugin_data: &Path) -> Self {
        let root_str = plugin_root.to_string_lossy();
        let data_str = plugin_data.to_string_lossy();
        match self {
            McpServer::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                let resolved_command = if let Some(rel) = command.strip_prefix("./") {
                    plugin_root.join(rel).to_string_lossy().to_string()
                } else {
                    command.clone()
                };

                let expanded_args = args
                    .iter()
                    .map(|arg| expand_string(arg, &root_str, &data_str))
                    .collect();

                let mut expanded_env = BTreeMap::new();
                for (k, v) in env {
                    expanded_env.insert(k.clone(), expand_string(v, &root_str, &data_str));
                }

                let expanded_cwd = cwd.as_ref().map(|c| {
                    if let Some(rel) = c.strip_prefix("./") {
                        plugin_root.join(rel).to_string_lossy().to_string()
                    } else {
                        expand_string(c, &root_str, &data_str)
                    }
                });

                McpServer::Stdio {
                    command: resolved_command,
                    args: expanded_args,
                    env: expanded_env,
                    cwd: expanded_cwd,
                }
            }
            McpServer::StreamableHttp { url, headers } => McpServer::StreamableHttp {
                url: url.clone(),
                headers: headers.clone(),
            },
            McpServer::Sse { url, headers } => McpServer::Sse {
                url: url.clone(),
                headers: headers.clone(),
            },
        }
    }
}

fn expand_string(input: &str, root: &str, data: &str) -> String {
    input
        .replace("${PLUGIN_ROOT}", root)
        .replace("${PLUGIN_DATA}", data)
}

pub fn parse_agent_plugin(plugin_dir: &Path) -> Result<ParsedAgentPlugin, String> {
    let manifest_path = plugin_dir.join(PLUGIN_MANIFEST_FILE);
    let bytes = fs::read(&manifest_path)
        .map_err(|e| format!("Could not read {}: {e}", manifest_path.display()))?;
    let manifest = PluginManifest::from_slice(&bytes)?;

    let mcp_path = plugin_dir.join(MCP_MANIFEST_FILE);
    let mcp_config = if mcp_path.is_file() {
        let mcp_bytes = fs::read(&mcp_path)
            .map_err(|e| format!("Could not read {}: {e}", mcp_path.display()))?;
        Some(McpConfig::from_slice(&mcp_bytes)?)
    } else {
        None
    };

    let skills_dir = plugin_dir.join("skills");
    let mut skill_names = Vec::new();
    if skills_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && path.join("SKILL.md").is_file() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        skill_names.push(name.to_string());
                    }
                }
            }
        }
    }
    skill_names.sort();

    Ok(ParsedAgentPlugin {
        manifest,
        mcp_config,
        skill_names,
    })
}

pub fn materialize_agent_plugin(
    source: &Path,
    target: &Path,
    plugin_data: &Path,
) -> Result<(), String> {
    crate::sources::copy_directory(source, target)?;
    let mcp_path = target.join(MCP_MANIFEST_FILE);
    if mcp_path.is_file() {
        let mcp_bytes = fs::read(&mcp_path)
            .map_err(|e| format!("Could not read {}: {e}", mcp_path.display()))?;
        let config = McpConfig::from_slice(&mcp_bytes)?;
        let mut expanded_servers = BTreeMap::new();
        for (name, server) in config.mcp_servers {
            expanded_servers.insert(name, server.expand_placeholders(target, plugin_data));
        }
        let updated_config = McpConfig {
            schema: config.schema,
            mcp_servers: expanded_servers,
        };
        let updated_bytes = serde_json::to_vec_pretty(&updated_config)
            .map_err(|e| format!("Could not serialize {}: {e}", mcp_path.display()))?;
        fs::write(&mcp_path, updated_bytes)
            .map_err(|e| format!("Could not write {}: {e}", mcp_path.display()))?;
    }
    fs::create_dir_all(plugin_data)
        .map_err(|e| format!("Could not create plugin data dir {}: {e}", plugin_data.display()))?;
    Ok(())
}

pub fn update_copilot_settings(
    copilot_settings_path: &Path,
    plugin_name: &str,
    enabled: bool,
) -> Result<(), String> {
    let mut settings: serde_json::Value = if copilot_settings_path.is_file() {
        let bytes = fs::read(copilot_settings_path)
            .map_err(|e| format!("Could not read {}: {e}", copilot_settings_path.display()))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| format!("Could not parse {}: {e}", copilot_settings_path.display()))?
    } else {
        serde_json::json!({})
    };

    if !settings.is_object() {
        settings = serde_json::json!({});
    }

    let obj = settings.as_object_mut().unwrap();
    if enabled {
        let enabled_plugins = obj
            .entry("enabledPlugins")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(plugins_obj) = enabled_plugins.as_object_mut() {
            plugins_obj.insert(plugin_name.to_string(), serde_json::Value::Bool(true));
        }
    } else if let Some(enabled_plugins) = obj.get_mut("enabledPlugins") {
        if let Some(plugins_obj) = enabled_plugins.as_object_mut() {
            plugins_obj.remove(plugin_name);
        }
    }

    if let Some(parent) = copilot_settings_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }

    let updated_bytes = serde_json::to_vec_pretty(&settings)
        .map_err(|e| format!("Could not serialize settings: {e}"))?;
    fs::write(copilot_settings_path, updated_bytes)
        .map_err(|e| format!("Could not write {}: {e}", copilot_settings_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_names() {
        assert!(validate_plugin_name("my-plugin").is_ok());
        assert!(validate_plugin_name("acme.tools").is_ok());
        assert!(validate_plugin_name("lint3r").is_ok());
        assert!(validate_plugin_name("a").is_ok());

        assert!(validate_plugin_name("My-Plugin").is_err());
        assert!(validate_plugin_name("-start").is_err());
        assert!(validate_plugin_name("end-").is_err());
        assert!(validate_plugin_name("has--double").is_err());
        assert!(validate_plugin_name("too..many").is_err());
        assert!(validate_plugin_name("").is_err());
    }

    #[test]
    fn parse_valid_plugin_and_mcp() {
        let plugin_json = r#"{
            "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
            "name": "data-tools",
            "version": "1.0.0",
            "description": "Tools for data analysis"
        }"#;
        let manifest = PluginManifest::from_slice(plugin_json.as_bytes()).expect("valid");
        assert_eq!(manifest.name, "data-tools");

        let mcp_json = r#"{
            "$schema": "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json",
            "mcpServers": {
                "server": {
                    "type": "stdio",
                    "command": "./bin/server",
                    "args": ["--data", "${PLUGIN_DATA}/db"],
                    "env": { "CONFIG": "${PLUGIN_ROOT}/config.json" },
                    "cwd": "${PLUGIN_ROOT}"
                }
            }
        }"#;
        let mcp = McpConfig::from_slice(mcp_json.as_bytes()).expect("valid mcp");
        assert_eq!(mcp.mcp_servers.len(), 1);

        let server = &mcp.mcp_servers["server"];
        let expanded = server.expand_placeholders(Path::new("/opt/plugin"), Path::new("/var/data"));
        if let McpServer::Stdio { command, args, env, cwd } = expanded {
            assert_eq!(command, "/opt/plugin/bin/server");
            assert_eq!(args, vec!["--data", "/var/data/db"]);
            assert_eq!(env.get("CONFIG").unwrap(), "/opt/plugin/config.json");
            assert_eq!(cwd.unwrap(), "/opt/plugin");
        } else {
            panic!("Expected stdio");
        }
    }
}
