//! Manifest normalization and Agent Skill name materialization.

use crate::digest::directory_digest;
use crate::manifest::{ManifestComponent, ManifestPackage, SourceManifest, SOURCE_MANIFEST_FILE};
use crate::mcp::{McpConfig, McpServer};
use crate::sources::copy_directory;
use serde::Serialize;
use serde_yaml_ng::{Mapping, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogError {
    pub(crate) path: String,
    pub(crate) message: String,
}

#[derive(Clone, Debug)]
pub(crate) struct CatalogItem {
    pub(crate) id: String,
    pub(crate) local_id: String,
    pub(crate) source_id: String,
    pub(crate) source_key: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) disable_model_invocation: bool,
    pub(crate) digest: String,
    pub(crate) source: String,
    pub(crate) source_is_directory: bool,
    pub(crate) destination: PathBuf,
    pub(crate) manifest_version: u8,
    pub(crate) components: Vec<CatalogComponent>,
    pub(crate) conflicts_with: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CatalogComponentKind {
    Skill,
    McpServer,
}

#[derive(Clone, Debug)]
pub(crate) struct CatalogComponent {
    pub(crate) id: String,
    pub(crate) kind: CatalogComponentKind,
    pub(crate) source: String,
    pub(crate) source_is_directory: bool,
    pub(crate) digest: String,
    pub(crate) effective_name: String,
    pub(crate) description: String,
    pub(crate) disable_model_invocation: bool,
    pub(crate) mcp_server: Option<McpServer>,
}

struct SkillFrontmatter {
    name: String,
    description: String,
    disable_model_invocation: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ManifestCatalog {
    pub(crate) manifest: SourceManifest,
    pub(crate) items: BTreeMap<String, CatalogItem>,
    pub(crate) errors: Vec<CatalogError>,
}

pub(crate) fn read_manifest_catalog(
    root: &Path,
    source_key: &str,
) -> Result<ManifestCatalog, String> {
    let manifest_path = root.join(SOURCE_MANIFEST_FILE);
    let bytes = fs::read(&manifest_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!(
                "This repository does not publish the required top-level {SOURCE_MANIFEST_FILE}."
            )
        } else {
            format!("Could not read {SOURCE_MANIFEST_FILE}: {error}")
        }
    })?;
    let manifest = SourceManifest::from_slice(&bytes)?;
    validate_repository_tree(root)?;

    let mut items = BTreeMap::new();
    let mut errors = Vec::new();
    for package in manifest.packages() {
        match normalize_package(root, source_key, &manifest.source().id, package) {
            Ok(item) if items.contains_key(&item.local_id) => errors.push(CatalogError {
                path: SOURCE_MANIFEST_FILE.to_string(),
                message: format!("Duplicate package id: {}", item.local_id),
            }),
            Ok(item) => {
                items.insert(item.local_id.clone(), item);
            }
            Err(message) => errors.push(CatalogError {
                path: SOURCE_MANIFEST_FILE.to_string(),
                message,
            }),
        }
    }

    if items.is_empty() {
        let detail = errors.first().map_or(String::new(), |error| {
            format!(" {}: {}", error.path, error.message)
        });
        return Err(format!(
            "The manifest does not contain any valid packages.{detail}"
        ));
    }
    validate_destination_ownership(&items)?;
    Ok(ManifestCatalog {
        manifest,
        items,
        errors,
    })
}

pub(crate) fn materialize_agent_skill(
    source: &Path,
    target: &Path,
    effective_name: &str,
) -> Result<(), String> {
    copy_directory(source, target)?;
    let skill_file = target.join("SKILL.md");
    let original = fs::read_to_string(&skill_file)
        .map_err(|error| format!("Could not read {}: {error}", skill_file.display()))?;
    let rendered = render_skill_markdown(&original, effective_name)?;
    fs::write(&skill_file, rendered)
        .map_err(|error| format!("Could not materialize {}: {error}", skill_file.display()))
}

fn normalize_package(
    root: &Path,
    source_key: &str,
    source_id: &str,
    package: &ManifestPackage,
) -> Result<CatalogItem, String> {
    validate_local_id(&package.id)?;
    let canonical_id = format!("{source_id}/{}", package.id);
    let package_name = format!("{source_id}-{}", package.id);
    if package_name.len() > 64 || !valid_name(&package_name) {
        return Err(format!(
            "The installed package name {package_name:?} exceeds the portable name contract."
        ));
    }
    let mut components = Vec::new();
    for component in &package.components {
        normalize_component(root, source_id, package, component, &mut components)?;
    }
    let source = package
        .components
        .first()
        .map(|component| component.path().to_string())
        .ok_or_else(|| format!("Package {} has no components.", package.id))?;
    let source_is_directory = components
        .first()
        .is_some_and(|component| component.source_is_directory);
    let default_description = format!("Portable agent configuration package {}.", package.id);
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, canonical_id.as_bytes());
    for component in &components {
        hash_field(&mut hasher, component.id.as_bytes());
        hash_field(&mut hasher, component.digest.as_bytes());
    }
    let digest = hex_digest(hasher.finalize());
    let destination = destination_home()?
        .join(".agents")
        .join("packages")
        .join(&package_name);
    let skill_components = components
        .iter()
        .filter(|component| component.kind == CatalogComponentKind::Skill)
        .collect::<Vec<_>>();
    let description = match package.description.clone() {
        Some(description) => description,
        None => match skill_components.as_slice() {
            [skill] => skill.description.clone(),
            _ => default_description,
        },
    };
    Ok(CatalogItem {
        id: canonical_id,
        local_id: package.id.clone(),
        source_id: source_id.to_string(),
        source_key: source_key.to_string(),
        name: package.name.clone().unwrap_or_else(|| package_name.clone()),
        description,
        disable_model_invocation: components
            .iter()
            .any(|component| component.disable_model_invocation),
        digest,
        source,
        source_is_directory,
        destination,
        manifest_version: 2,
        components,
        conflicts_with: package.conflicts_with.clone(),
    })
}

fn normalize_component(
    root: &Path,
    source_id: &str,
    package: &ManifestPackage,
    component: &ManifestComponent,
    output: &mut Vec<CatalogComponent>,
) -> Result<(), String> {
    let relative = validate_relative_path(component.path(), "component path")?;
    let source_path = root.join(&relative);
    let component_id = component
        .id()
        .map(str::to_string)
        .unwrap_or_else(|| package.id.clone());
    match component {
        ManifestComponent::Skill { .. } => {
            let metadata = fs::symlink_metadata(&source_path)
                .map_err(|error| format!("Could not inspect {}: {error}", component.path()))?;
            if !metadata.is_dir() {
                return Err(format!("{} is not a skill directory.", component.path()));
            }
            let skill = parse_skill(&source_path.join("SKILL.md"))?;
            if skill.name != component_id {
                return Err(format!(
                    "Skill component id {component_id:?} must match its SKILL.md name {:?}.",
                    skill.name
                ));
            }
            let effective_name = format!("{source_id}-{component_id}");
            if effective_name.len() > 64 || !valid_name(&effective_name) {
                return Err(format!(
                    "Installed skill name {effective_name:?} is not portable."
                ));
            }
            output.push(CatalogComponent {
                id: component_id,
                kind: CatalogComponentKind::Skill,
                source: component.path().to_string(),
                source_is_directory: true,
                digest: directory_digest(&source_path)?,
                effective_name,
                description: skill.description,
                disable_model_invocation: skill.disable_model_invocation,
                mcp_server: None,
            });
        }
        ManifestComponent::McpServer { .. } => {
            let bytes = fs::read(&source_path)
                .map_err(|error| format!("Could not read {}: {error}", component.path()))?;
            let config = McpConfig::from_slice(&bytes)?;
            if config.mcp_servers.is_empty() {
                return Err(format!("{} declares no MCP servers.", component.path()));
            }
            let single_server = config.mcp_servers.len() == 1;
            for (server_name, server) in config.mcp_servers {
                let id = if single_server {
                    component_id.clone()
                } else {
                    format!("{component_id}-{server_name}")
                };
                output.push(CatalogComponent {
                    id,
                    kind: CatalogComponentKind::McpServer,
                    source: component.path().to_string(),
                    source_is_directory: false,
                    digest: digest_bytes(&serde_json::to_vec(&server).map_err(|error| {
                        format!("Could not serialize MCP server {server_name}: {error}")
                    })?),
                    effective_name: format!("{source_id}-{server_name}"),
                    description: mcp_component_description(&server),
                    disable_model_invocation: false,
                    mcp_server: Some(server),
                });
            }
        }
    }
    Ok(())
}

fn digest_bytes(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes))
}

fn validate_repository_tree(root: &Path) -> Result<(), String> {
    let mut pending = vec![root.to_path_buf()];
    let mut portable = BTreeMap::<String, PathBuf>::new();
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)
            .map_err(|error| format!("Could not read {}: {error}", directory.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not read {}: {error}", directory.display()))?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| format!("{} is outside the source", path.display()))?;
            if relative == Path::new(".git") || relative.starts_with(".git") {
                continue;
            }
            let file_type = entry
                .file_type()
                .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?;
            if file_type.is_symlink() {
                return Err(format!(
                    "{} is a symlink; source snapshots may not contain symlinks.",
                    relative.display()
                ));
            }
            for component in relative.components() {
                let Component::Normal(component) = component else {
                    return Err(format!("{} contains an unsafe path.", relative.display()));
                };
                validate_portable_component(component, relative)?;
            }
            let key = normalized_path(relative).to_lowercase();
            if let Some(existing) = portable.insert(key, relative.to_path_buf()) {
                if existing != relative {
                    return Err(format!(
                        "Source paths {} and {} collide on case-insensitive filesystems.",
                        existing.display(),
                        relative.display()
                    ));
                }
            }
            if file_type.is_dir() {
                pending.push(path);
            } else if !file_type.is_file() {
                return Err(format!(
                    "{} is not a regular file or directory.",
                    relative.display()
                ));
            }
        }
    }
    Ok(())
}

fn validate_relative_path(value: &str, label: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if value.is_empty() || value.contains('\\') || path.is_absolute() {
        return Err(format!(
            "Invalid {label} path {value:?}; use a non-empty relative path with forward slashes."
        ));
    }
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(format!("{label} path may not escape its root: {value}"));
        };
        validate_portable_component(component, &path)?;
    }
    Ok(path)
}

fn destination_home() -> Result<PathBuf, String> {
    if let Some(root) = crate::qa_paths::root()? {
        return Ok(root.join("home"));
    }
    dirs::home_dir().ok_or_else(|| "Could not find your home directory.".to_string())
}

fn validate_local_id(value: &str) -> Result<(), String> {
    if value.len() <= 64 && valid_name(value) {
        Ok(())
    } else {
        Err(format!("Invalid install id: {value}"))
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !is_windows_reserved_name(name)
}

fn is_windows_reserved_name(name: &str) -> bool {
    let stem = name
        .split_once('.')
        .map_or(name, |(before_extension, _)| before_extension)
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

pub(crate) fn validate_portable_component(component: &OsStr, path: &Path) -> Result<(), String> {
    let Some(name) = component.to_str() else {
        return Err(format!("{} contains a non-UTF-8 path.", path.display()));
    };
    let invalid = name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with([' ', '.'])
        || name.chars().any(|character| {
            character < '\u{20}'
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
        })
        || name.encode_utf16().count() > 255
        || is_windows_reserved_name(name);
    if invalid {
        Err(format!(
            "{} contains an invalid portable path component.",
            path.display()
        ))
    } else {
        Ok(())
    }
}

fn validate_destination_ownership(items: &BTreeMap<String, CatalogItem>) -> Result<(), String> {
    let mut roots = items
        .values()
        .map(|item| {
            (
                normalized_path(&item.destination).to_lowercase(),
                item.id.as_str(),
            )
        })
        .collect::<Vec<_>>();
    roots.sort();
    for pair in roots.windows(2) {
        let (left_path, left_item) = &pair[0];
        let (right_path, right_item) = &pair[1];
        if left_path == right_path
            || right_path
                .strip_prefix(left_path.as_str())
                .is_some_and(|suffix| suffix.starts_with('/'))
        {
            return Err(format!(
                "Installs {left_item} and {right_item} declare overlapping destinations."
            ));
        }
    }
    Ok(())
}

fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let mut output = String::with_capacity(64);
    for byte in digest.as_ref() {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn parse_skill(path: &Path) -> Result<SkillFrontmatter, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let (frontmatter, _) = split_skill_markdown(&contents)?;
    let mapping = serde_yaml_ng::from_str::<Mapping>(frontmatter)
        .map_err(|error| format!("SKILL.md frontmatter is invalid YAML: {error}"))?;
    let name = required_string(&mapping, "name")?;
    if name.len() > 64 || !valid_name(&name) {
        return Err(format!("SKILL.md has an invalid Agent Skill name: {name}"));
    }
    let description = required_string(&mapping, "description")?;
    if description.chars().count() > 1024 {
        return Err("SKILL.md description exceeds 1024 characters.".to_string());
    }
    Ok(SkillFrontmatter {
        name,
        description,
        disable_model_invocation: optional_boolean(&mapping, "disable-model-invocation")?,
    })
}

fn mcp_component_description(server: &McpServer) -> String {
    match server {
        McpServer::Stdio { command, .. } => format!("Runs {command}."),
        McpServer::StreamableHttp { url, .. } => format!("HTTP MCP server at {url}."),
        McpServer::Sse { url, .. } => format!("SSE MCP server at {url}."),
    }
}

fn split_skill_markdown(contents: &str) -> Result<(&str, &str), String> {
    let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
    let first_end = contents
        .find('\n')
        .ok_or_else(|| "SKILL.md is missing YAML frontmatter.".to_string())?;
    if contents[..first_end].trim_end_matches('\r') != "---" {
        return Err("SKILL.md is missing YAML frontmatter.".to_string());
    }
    let mut line_start = first_end + 1;
    while line_start <= contents.len() {
        let line_end = contents[line_start..]
            .find('\n')
            .map_or(contents.len(), |offset| line_start + offset);
        if contents[line_start..line_end].trim_end_matches('\r') == "---" {
            let body_start = usize::min(line_end + 1, contents.len());
            return Ok((
                &contents[first_end + 1..line_start],
                &contents[body_start..],
            ));
        }
        if line_end == contents.len() {
            break;
        }
        line_start = line_end + 1;
    }
    Err("SKILL.md has unterminated YAML frontmatter.".to_string())
}

fn render_skill_markdown(contents: &str, effective_name: &str) -> Result<String, String> {
    let (frontmatter, body) = split_skill_markdown(contents)?;
    let mut mapping = serde_yaml_ng::from_str::<Mapping>(frontmatter)
        .map_err(|error| format!("SKILL.md frontmatter is invalid YAML: {error}"))?;
    mapping.insert(
        Value::String("name".to_string()),
        Value::String(effective_name.to_string()),
    );
    let yaml = serde_yaml_ng::to_string(&mapping)
        .map_err(|error| format!("Could not render SKILL.md frontmatter: {error}"))?;
    Ok(format!("---\n{yaml}---\n{body}"))
}

fn required_string(mapping: &Mapping, key: &str) -> Result<String, String> {
    match mapping.get(Value::String(key.to_string())) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        _ => Err(format!("SKILL.md {key} must be a non-empty string.")),
    }
}

fn optional_boolean(mapping: &Mapping, key: &str) -> Result<bool, String> {
    match mapping.get(Value::String(key.to_string())) {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("SKILL.md {key} must be a boolean.")),
    }
}

fn normalized_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn relative_path(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| format!("{} is outside {}", path.display(), root.display()))?;
    let parts = relative
        .components()
        .map(|component| match component {
            Component::Normal(part) => part
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{} is not UTF-8", path.display())),
            _ => Err(format!("{} has an invalid path", path.display())),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(root: &Path, source_id: &str, packages: &str) {
        fs::write(
            root.join(SOURCE_MANIFEST_FILE),
            format!(
                r#"{{"version":2,"source":{{"id":"{source_id}","name":"Test","description":"Test source."}},"packages":{packages}}}"#
            ),
        )
        .expect("manifest");
    }

    fn write_skill(root: &Path, name: &str, extra: &str) {
        let skill = root.join("skills").join(name);
        fs::create_dir_all(&skill).expect("skill directory");
        fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: A test skill.\n{extra}---\n\n# Body\n"),
        )
        .expect("skill");
    }

    #[test]
    fn agent_skill_is_namespaced_and_only_name_is_interpreted() {
        let root = tempfile::tempdir().expect("tempdir");
        write_skill(
            root.path(),
            "review",
            "license: MIT\nmetadata:\n  arbitrary: value\n",
        );
        write_manifest(
            root.path(),
            "acme",
            r#"[{"id":"review","components":[{"kind":"skill","path":"skills/review"}]}]"#,
        );
        let catalog = read_manifest_catalog(root.path(), "source-key").expect("catalog");
        let item = &catalog.items["review"];
        assert_eq!(item.name, "acme-review");
        assert_eq!(item.components[0].effective_name, "acme-review");
        assert_eq!(item.components[0].kind, CatalogComponentKind::Skill);
        assert_eq!(item.components[0].description, "A test skill.");
        assert!(!item.components[0].disable_model_invocation);
        assert_eq!(item.description, "A test skill.");
        assert!(!item.disable_model_invocation);
    }

    #[test]
    fn skill_disable_model_invocation_is_surfaced_on_item_and_component() {
        let root = tempfile::tempdir().expect("tempdir");
        write_skill(root.path(), "review", "disable-model-invocation: true\n");
        write_manifest(
            root.path(),
            "acme",
            r#"[{"id":"review","components":[{"kind":"skill","path":"skills/review"}]}]"#,
        );
        let catalog = read_manifest_catalog(root.path(), "source-key").expect("catalog");
        let item = &catalog.items["review"];
        assert!(item.disable_model_invocation);
        assert!(item.components[0].disable_model_invocation);
        assert_eq!(item.components[0].description, "A test skill.");
    }

    #[test]
    fn plugin_components_keep_their_own_descriptions_and_manual_flags() {
        let root = tempfile::tempdir().expect("tempdir");
        write_skill(root.path(), "review", "disable-model-invocation: true\n");
        write_skill(root.path(), "debug", "");
        fs::create_dir_all(root.path().join("mcp")).expect("mcp directory");
        fs::write(
            root.path().join("mcp/database.json"),
            format!(
                r#"{{"$schema":"{schema}","mcpServers":{{"database":{{"type":"stdio","command":"uvx","args":["db"]}}}}}}"#,
                schema = crate::mcp::MCP_SCHEMA_V1
            ),
        )
        .expect("mcp");
        write_manifest(
            root.path(),
            "acme",
            r#"[
              {
                "id":"tools",
                "name":"Acme tools",
                "description":"Plugin package.",
                "components":[
                  {"kind":"skill","id":"review","path":"skills/review"},
                  {"kind":"skill","id":"debug","path":"skills/debug"},
                  {"kind":"mcpServer","id":"database","path":"mcp/database.json"}
                ]
              }
            ]"#,
        );
        let catalog = read_manifest_catalog(root.path(), "source-key").expect("catalog");
        let item = &catalog.items["tools"];
        assert_eq!(item.description, "Plugin package.");
        assert!(item.disable_model_invocation);
        assert_eq!(item.components.len(), 3);
        assert_eq!(item.components[0].id, "review");
        assert_eq!(item.components[0].description, "A test skill.");
        assert!(item.components[0].disable_model_invocation);
        assert_eq!(item.components[1].id, "debug");
        assert_eq!(item.components[1].description, "A test skill.");
        assert!(!item.components[1].disable_model_invocation);
        assert_eq!(item.components[2].id, "database");
        assert_eq!(item.components[2].description, "Runs uvx.");
        assert!(!item.components[2].disable_model_invocation);
    }

    #[test]
    fn materialization_preserves_extra_frontmatter_and_body() {
        let root = tempfile::tempdir().expect("tempdir");
        write_skill(root.path(), "review", "license: MIT\n");
        let target = root.path().join("installed");
        materialize_agent_skill(&root.path().join("skills/review"), &target, "acme-review")
            .expect("materialize");
        let rendered = fs::read_to_string(target.join("SKILL.md")).expect("rendered");
        assert!(rendered.contains("name: acme-review"));
        assert!(rendered.contains("license: MIT"));
        assert!(rendered.ends_with("# Body\n"));
    }

    #[test]
    fn invalid_packages_are_reported_without_hiding_valid_packages() {
        let root = tempfile::tempdir().expect("tempdir");
        write_skill(root.path(), "review", "");
        write_manifest(
            root.path(),
            "acme",
            r#"[
              {"id":"review","components":[{"kind":"skill","path":"skills/review"}]},
              {"id":"missing","components":[{"kind":"skill","path":"skills/missing"}]}
            ]"#,
        );
        let catalog = read_manifest_catalog(root.path(), "source-key").expect("partial catalog");
        assert_eq!(catalog.items.len(), 1);
        assert_eq!(catalog.errors.len(), 1);
    }
}
