//! Manifest-driven catalog normalization and Agent Skill materialization.

use crate::catalog::{relative_path, valid_item_name, validate_portable_path_component};
use crate::digest::directory_digest;
use crate::domain::CatalogError;
use crate::manifest::{
    DestinationAnchor, DestinationTemplate, LifecycleHooks, ManifestAction, ManifestItem,
    PlatformSelector, SourceManifest, SOURCE_MANIFEST_FILE,
};
use crate::sources::copy_directory;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Serialize;
use serde_yaml_ng::{Mapping, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(crate) const AGENT_SKILL_KIND: &str = "agent-skill";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentSkillMetadata {
    pub(crate) local_name: String,
    pub(crate) license: Option<String>,
    pub(crate) compatibility: Option<String>,
    pub(crate) metadata: BTreeMap<String, String>,
    pub(crate) allowed_tools: Option<String>,
    pub(crate) manual_only: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedMapping {
    pub(crate) source: String,
    pub(crate) destination: ResolvedDestination,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedDestination {
    pub(crate) anchor: DestinationAnchor,
    pub(crate) path: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct CatalogItem {
    pub(crate) id: String,
    pub(crate) local_id: String,
    pub(crate) source_id: String,
    pub(crate) source_key: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) kind: String,
    pub(crate) digest: String,
    pub(crate) mappings: Vec<ResolvedMapping>,
    pub(crate) hooks: LifecycleHooks,
    pub(crate) actions: Vec<ManifestAction>,
    pub(crate) platform: Option<PlatformSelector>,
    pub(crate) agent_skill: Option<AgentSkillMetadata>,
    pub(crate) materialized_skill_name: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ManifestCatalog {
    pub(crate) manifest: SourceManifest,
    pub(crate) items: BTreeMap<String, CatalogItem>,
    pub(crate) errors: Vec<CatalogError>,
}

#[derive(Debug)]
struct ParsedSkill {
    metadata: AgentSkillMetadata,
    description: String,
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
    let paths = repository_paths(root)?;
    let source_id = manifest.source.id.clone();
    let mut items = BTreeMap::new();
    let mut errors = Vec::new();

    for group in &manifest.agent_skills {
        let matcher = build_matcher(&group.include)?;
        for path in matching_paths(root, &paths, &matcher, Path::is_dir) {
            let display = relative_path(root, &path)?;
            match normalize_agent_skill(root, source_key, &manifest.source.id, group, &path) {
                Ok(item) => insert_item(item, &mut items, &mut errors, &display),
                Err(message) => errors.push(CatalogError {
                    path: display,
                    message,
                }),
            }
        }
    }

    for item in &manifest.items {
        match normalize_generic_item(root, source_key, &source_id, item, &BTreeMap::new()) {
            Ok(item) => insert_item(item, &mut items, &mut errors, SOURCE_MANIFEST_FILE),
            Err(message) => errors.push(CatalogError {
                path: SOURCE_MANIFEST_FILE.to_string(),
                message,
            }),
        }
    }

    for collection in &manifest.collections {
        let matcher = build_matcher(&collection.include)?;
        for path in matching_paths(root, &paths, &matcher, Path::is_file) {
            let repository_path = relative_path(root, &path)?;
            let basename = path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(|| format!("{repository_path} is not UTF-8"))?;
            let stem = basename.split('.').next().unwrap_or(basename);
            let variables = BTreeMap::from([
                ("path", repository_path.as_str()),
                ("basename", basename),
                ("stem", stem),
            ]);
            match normalize_generic_item(root, source_key, &source_id, &collection.item, &variables)
            {
                Ok(item) => insert_item(item, &mut items, &mut errors, &repository_path),
                Err(message) => errors.push(CatalogError {
                    path: repository_path,
                    message,
                }),
            }
        }
    }

    if items.is_empty() {
        let detail = errors.first().map_or(String::new(), |error| {
            format!(" {}: {}", error.path, error.message)
        });
        return Err(format!(
            "The manifest does not produce any valid catalog items.{detail}"
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
) -> Result<String, String> {
    copy_directory(source, target)?;
    let skill_file = target.join("SKILL.md");
    let original = fs::read_to_string(&skill_file)
        .map_err(|error| format!("Could not read {}: {error}", skill_file.display()))?;
    let rendered = render_skill_markdown(&original, effective_name)?;
    fs::write(&skill_file, rendered)
        .map_err(|error| format!("Could not materialize {}: {error}", skill_file.display()))?;
    directory_digest(target)
}

fn repository_paths(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut paths = Vec::new();
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
                validate_portable_path_component(component, relative)?;
            }
            let key = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
                .collect::<Vec<_>>()
                .join("/");
            if let Some(existing) = portable.insert(key, relative.to_path_buf()) {
                if existing != relative {
                    return Err(format!(
                        "Source paths {} and {} collide on case-insensitive filesystems.",
                        existing.display(),
                        relative.display()
                    ));
                }
            }
            paths.push(path.clone());
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
    paths.sort();
    Ok(paths)
}

fn build_matcher(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .backslash_escape(false)
            .build()
            .map_err(|error| format!("Invalid include pattern {pattern:?}: {error}"))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|error| format!("Could not compile include patterns: {error}"))
}

fn matching_paths(
    root: &Path,
    paths: &[PathBuf],
    matcher: &GlobSet,
    kind: fn(&Path) -> bool,
) -> Vec<PathBuf> {
    paths
        .iter()
        .filter(|path| kind(path))
        .filter(|path| {
            path.strip_prefix(root)
                .is_ok_and(|relative| matcher.is_match(normalized_path(relative)))
        })
        .cloned()
        .collect()
}

fn normalized_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn normalize_agent_skill(
    root: &Path,
    source_key: &str,
    source_id: &str,
    group: &crate::manifest::AgentSkillCollection,
    path: &Path,
) -> Result<CatalogItem, String> {
    let skill_file = path.join("SKILL.md");
    let contents = fs::read_to_string(&skill_file).map_err(|error| {
        format!(
            "Could not read {}: {error}",
            relative_path(root, &skill_file).unwrap_or_else(|_| "SKILL.md".to_string())
        )
    })?;
    let parsed = parse_skill_metadata(&contents)?;
    let directory_name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    if parsed.metadata.local_name != directory_name {
        return Err(format!(
            "SKILL.md declares the name {}, expected {directory_name}",
            parsed.metadata.local_name
        ));
    }
    let effective_name = format!("{source_id}-{}", parsed.metadata.local_name);
    if effective_name.len() > 64 || !valid_item_name(&effective_name) {
        return Err(format!(
            "The materialized Agent Skill name {effective_name:?} exceeds the 64-character portable name contract."
        ));
    }
    let variables = BTreeMap::from([
        ("skill.name", effective_name.as_str()),
        ("skill.localName", parsed.metadata.local_name.as_str()),
    ]);
    let mut mappings = Vec::new();
    for destination in &group.destinations {
        let resolved = resolve_destination(destination, &variables)?;
        if resolved.path.file_name().and_then(OsStr::to_str) != Some(effective_name.as_str()) {
            return Err(format!(
                "Agent Skill destinations must end in ${{skill.name}} ({effective_name})."
            ));
        }
        mappings.push(ResolvedMapping {
            source: relative_path(root, path)?,
            destination: resolved,
        });
    }
    let local_id = parsed.metadata.local_name.clone();
    let id = format!("{source_id}/{local_id}");
    let digest = item_digest(
        root,
        &id,
        &mappings,
        Some((&contents, effective_name.as_str())),
    )?;
    Ok(CatalogItem {
        id,
        local_id,
        source_id: source_id.to_string(),
        source_key: source_key.to_string(),
        name: effective_name.clone(),
        description: parsed.description,
        kind: AGENT_SKILL_KIND.to_string(),
        digest,
        mappings,
        hooks: group.hooks.clone(),
        actions: group.actions.clone(),
        platform: group.when.clone(),
        agent_skill: Some(parsed.metadata),
        materialized_skill_name: Some(effective_name),
    })
}

fn normalize_generic_item(
    root: &Path,
    source_key: &str,
    source_id: &str,
    manifest: &ManifestItem,
    variables: &BTreeMap<&str, &str>,
) -> Result<CatalogItem, String> {
    let local_id = expand_template(&manifest.id, variables)?;
    if !valid_item_name(&local_id) {
        return Err(format!("Invalid generated item id: {local_id}"));
    }
    let name = expand_template(&manifest.name, variables)?;
    let description = expand_template(&manifest.description, variables)?;
    let kind = expand_template(&manifest.kind, variables)?;
    let mut mappings = Vec::new();
    for mapping in &manifest.files {
        let source = expand_template(&mapping.source, variables)?;
        let source_path = root.join(&source);
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|error| format!("Could not inspect {source}: {error}"))?;
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(format!("{source} is not a regular file or directory."));
        }
        mappings.push(ResolvedMapping {
            source,
            destination: resolve_destination(&mapping.destination, variables)?,
        });
    }
    let id = format!("{source_id}/{local_id}");
    let digest = item_digest(root, &id, &mappings, None)?;
    Ok(CatalogItem {
        id,
        local_id,
        source_id: source_id.to_string(),
        source_key: source_key.to_string(),
        name,
        description,
        kind,
        digest,
        mappings,
        hooks: manifest.hooks.clone(),
        actions: manifest.actions.clone(),
        platform: manifest.when.clone(),
        agent_skill: None,
        materialized_skill_name: None,
    })
}

fn resolve_destination(
    destination: &DestinationTemplate,
    variables: &BTreeMap<&str, &str>,
) -> Result<ResolvedDestination, String> {
    let expanded = expand_template(&destination.path, variables)?;
    let path = PathBuf::from(&expanded);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(format!("Invalid destination path: {expanded}"));
    }
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(format!("Destination may not escape its anchor: {expanded}"));
        };
        validate_portable_path_component(component, &path)?;
    }
    Ok(ResolvedDestination {
        anchor: destination.anchor,
        path,
    })
}

fn expand_template(value: &str, variables: &BTreeMap<&str, &str>) -> Result<String, String> {
    let mut expanded = value.to_string();
    while let Some(start) = expanded.find("${") {
        let Some(relative_end) = expanded[start + 2..].find('}') else {
            return Err(format!("Unterminated template in {value:?}."));
        };
        let end = start + 2 + relative_end;
        let variable = &expanded[start + 2..end];
        let replacement = variables
            .get(variable)
            .ok_or_else(|| format!("Unsupported template variable ${{{variable}}}."))?;
        expanded.replace_range(start..=end, replacement);
    }
    Ok(expanded)
}

fn insert_item(
    item: CatalogItem,
    items: &mut BTreeMap<String, CatalogItem>,
    errors: &mut Vec<CatalogError>,
    path: &str,
) {
    if items.contains_key(&item.local_id) {
        errors.push(CatalogError {
            path: path.to_string(),
            message: format!("Duplicate catalog item id: {}", item.local_id),
        });
    } else {
        items.insert(item.local_id.clone(), item);
    }
}

fn validate_destination_ownership(items: &BTreeMap<String, CatalogItem>) -> Result<(), String> {
    let mut roots = Vec::new();
    for item in items.values() {
        for mapping in &item.mappings {
            let normalized = normalized_path(&mapping.destination.path).to_lowercase();
            roots.push((mapping.destination.anchor, normalized, item.id.as_str()));
        }
    }
    roots.sort();
    for pair in roots.windows(2) {
        let (left_anchor, left_path, left_item) = &pair[0];
        let (right_anchor, right_path, right_item) = &pair[1];
        if left_anchor == right_anchor
            && (left_path == right_path
                || right_path
                    .strip_prefix(left_path.as_str())
                    .is_some_and(|suffix| suffix.starts_with('/')))
        {
            return Err(format!(
                "Catalog items {left_item} and {right_item} declare overlapping destination roots."
            ));
        }
    }
    Ok(())
}

fn item_digest(
    root: &Path,
    id: &str,
    mappings: &[ResolvedMapping],
    materialized_skill: Option<(&str, &str)>,
) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, id.as_bytes());
    for mapping in mappings {
        hash_field(&mut hasher, mapping.source.as_bytes());
        hash_field(
            &mut hasher,
            format!("{:?}", mapping.destination.anchor).as_bytes(),
        );
        hash_field(
            &mut hasher,
            normalized_path(&mapping.destination.path).as_bytes(),
        );
        let source = root.join(&mapping.source);
        if source.is_dir() {
            hash_field(&mut hasher, directory_digest(&source)?.as_bytes());
        } else {
            hash_field(
                &mut hasher,
                &fs::read(&source)
                    .map_err(|error| format!("Could not read {}: {error}", source.display()))?,
            );
        }
    }
    if let Some((contents, effective_name)) = materialized_skill {
        hash_field(
            &mut hasher,
            render_skill_markdown(contents, effective_name)?.as_bytes(),
        );
    }
    let digest = hasher.finalize();
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(output)
}

fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn parse_skill_metadata(contents: &str) -> Result<ParsedSkill, String> {
    let (frontmatter, _) = split_skill_markdown(contents)?;
    let mapping = serde_yaml_ng::from_str::<Mapping>(frontmatter)
        .map_err(|error| format!("SKILL.md frontmatter is invalid YAML: {error}"))?;
    let local_name = required_string(&mapping, "name")?;
    if local_name.len() > 64 || !valid_item_name(&local_name) {
        return Err(format!(
            "SKILL.md has an invalid Agent Skill name: {local_name}"
        ));
    }
    let description = required_string(&mapping, "description")?;
    if description.chars().count() > 1024 {
        return Err("SKILL.md description exceeds 1024 characters.".to_string());
    }
    let license = optional_string(&mapping, "license")?;
    let compatibility = optional_string(&mapping, "compatibility")?;
    if compatibility
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.chars().count() > 500)
    {
        return Err("SKILL.md compatibility must contain 1-500 characters.".to_string());
    }
    let allowed_tools = optional_string(&mapping, "allowed-tools")?;
    let manual_only = optional_bool(&mapping, "disable-model-invocation")?.unwrap_or(false);
    let metadata = optional_string_map(&mapping, "metadata")?;
    Ok(ParsedSkill {
        metadata: AgentSkillMetadata {
            local_name,
            license,
            compatibility,
            metadata,
            allowed_tools,
            manual_only,
        },
        description,
    })
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
            let body_start = if line_end < contents.len() {
                line_end + 1
            } else {
                line_end
            };
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
    optional_string(mapping, key)?.ok_or_else(|| format!("SKILL.md is missing {key}"))
}

fn optional_string(mapping: &Mapping, key: &str) -> Result<Option<String>, String> {
    match mapping.get(Value::String(key.to_string())) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(_) => Err(format!("SKILL.md {key} must be a non-empty string.")),
    }
}

fn optional_bool(mapping: &Mapping, key: &str) -> Result<Option<bool>, String> {
    match mapping.get(Value::String(key.to_string())) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("SKILL.md {key} must be true or false.")),
    }
}

fn optional_string_map(mapping: &Mapping, key: &str) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = mapping.get(Value::String(key.to_string())) else {
        return Ok(BTreeMap::new());
    };
    let Value::Mapping(values) = value else {
        return Err(format!(
            "SKILL.md {key} must be a string-to-string mapping."
        ));
    };
    values
        .iter()
        .map(|(key, value)| match (key, value) {
            (Value::String(key), Value::String(value)) => Ok((key.clone(), value.clone())),
            _ => Err(format!(
                "SKILL.md {key:?} metadata must contain only string keys and values."
            )),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(root: &Path, source_id: &str) {
        fs::write(
            root.join(SOURCE_MANIFEST_FILE),
            format!(
                r#"{{
                  "version": 1,
                  "source": {{ "id": "{source_id}", "name": "Test", "description": "Test source" }},
                  "agentSkills": [{{
                    "include": ["skills/*"],
                    "destinations": [{{ "anchor": "home", "path": ".agents/skills/${{skill.name}}" }}]
                  }}]
                }}"#
            ),
        )
        .expect("manifest");
    }

    fn write_skill(root: &Path, name: &str, extra: &str) {
        let skill = root.join("skills").join(name);
        fs::create_dir_all(&skill).expect("skill directory");
        fs::write(
            skill.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: Example skill\nlicense: MIT\nmetadata:\n  owner: test\ndisable-model-invocation: true\n{extra}---\n\n# Body\n\nKeep me exactly.\n"
            ),
        )
        .expect("skill file");
    }

    #[test]
    fn agent_skills_are_namespaced_and_metadata_is_exposed() {
        let source = tempfile::tempdir().expect("source");
        write_manifest(source.path(), "acme");
        write_skill(source.path(), "review", "compatibility: Requires git\n");
        let catalog = read_manifest_catalog(source.path(), "source-key").expect("catalog");
        let item = &catalog.items["review"];
        assert_eq!(item.id, "acme/review");
        assert_eq!(item.name, "acme-review");
        assert_eq!(item.materialized_skill_name.as_deref(), Some("acme-review"));
        let metadata = item.agent_skill.as_ref().expect("skill metadata");
        assert_eq!(metadata.license.as_deref(), Some("MIT"));
        assert_eq!(metadata.compatibility.as_deref(), Some("Requires git"));
        assert_eq!(metadata.metadata["owner"], "test");
        assert!(metadata.manual_only);
    }

    #[test]
    fn materialization_rewrites_only_the_name_and_preserves_the_body() {
        let source = tempfile::tempdir().expect("source");
        let target_root = tempfile::tempdir().expect("target");
        write_skill(source.path(), "review", "custom-field: preserved\n");
        let original = source.path().join("skills/review/SKILL.md");
        let target = target_root.path().join("acme-review");
        materialize_agent_skill(original.parent().expect("skill"), &target, "acme-review")
            .expect("materialized skill");
        let rendered = fs::read_to_string(target.join("SKILL.md")).expect("rendered skill");
        assert!(rendered.contains("name: acme-review"));
        assert!(rendered.contains("custom-field: preserved"));
        assert!(rendered.ends_with("# Body\n\nKeep me exactly.\n"));
    }

    #[test]
    fn an_overlong_prefixed_name_rejects_only_that_catalog_entry() {
        let source = tempfile::tempdir().expect("source");
        write_manifest(source.path(), "sixteencharslong");
        write_skill(source.path(), "valid", "");
        write_skill(
            source.path(),
            "this-name-is-deliberately-forty-nine-characters-longish",
            "",
        );
        let catalog = read_manifest_catalog(source.path(), "source-key").expect("partial catalog");
        assert!(catalog.items.contains_key("valid"));
        assert_eq!(catalog.errors.len(), 1);
        assert!(catalog.errors[0].message.contains("64-character"));
    }

    #[test]
    fn collection_variables_generate_generic_items() {
        let source = tempfile::tempdir().expect("source");
        fs::create_dir_all(source.path().join("prompts")).expect("prompts");
        fs::write(source.path().join("prompts/review.prompt.md"), "review").expect("prompt");
        fs::write(
            source.path().join(SOURCE_MANIFEST_FILE),
            r#"{
              "version": 1,
              "source": { "id": "acme", "name": "Acme", "description": "Acme source" },
              "collections": [{
                "include": ["prompts/*.prompt.md"],
                "item": {
                  "id": "${stem}", "name": "${stem}", "description": "Installs ${basename}.", "kind": "prompt",
                  "files": [{ "source": "${path}", "destination": { "anchor": "config", "path": "acme/${basename}" } }]
                }
              }]
            }"#,
        )
        .expect("manifest");
        let catalog = read_manifest_catalog(source.path(), "source-key").expect("catalog");
        let item = &catalog.items["review"];
        assert_eq!(item.id, "acme/review");
        assert_eq!(item.mappings[0].source, "prompts/review.prompt.md");
        assert_eq!(
            item.mappings[0].destination.path,
            Path::new("acme/review.prompt.md")
        );
    }

    #[test]
    fn overlapping_destinations_are_rejected() {
        let source = tempfile::tempdir().expect("source");
        fs::write(source.path().join("one"), "one").expect("one");
        fs::write(source.path().join("two"), "two").expect("two");
        fs::write(
            source.path().join(SOURCE_MANIFEST_FILE),
            r#"{
              "version": 1,
              "source": { "id": "acme", "name": "Acme", "description": "Acme source" },
              "items": [
                { "id": "one", "name": "One", "description": "One", "kind": "file", "files": [{ "source": "one", "destination": { "anchor": "config", "path": "acme" } }] },
                { "id": "two", "name": "Two", "description": "Two", "kind": "file", "files": [{ "source": "two", "destination": { "anchor": "config", "path": "acme/two" } }] }
              ]
            }"#,
        )
        .expect("manifest");
        assert!(read_manifest_catalog(source.path(), "source-key")
            .expect_err("overlap")
            .contains("overlapping destination roots"));
    }
}
