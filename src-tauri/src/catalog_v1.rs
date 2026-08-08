//! Manifest normalization and Agent Skill name materialization.

use crate::digest::directory_digest;
use crate::manifest::{ManifestInstall, SourceManifest, SOURCE_MANIFEST_FILE};
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
pub(crate) struct ResolvedDestination {
    pub(crate) declared: String,
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
    pub(crate) disable_model_invocation: bool,
    pub(crate) digest: String,
    pub(crate) source: String,
    pub(crate) source_is_directory: bool,
    pub(crate) destination: ResolvedDestination,
    pub(crate) materialized_skill_name: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ManifestCatalog {
    pub(crate) manifest: SourceManifest,
    pub(crate) items: BTreeMap<String, CatalogItem>,
    pub(crate) errors: Vec<CatalogError>,
}

struct ParsedSkill {
    local_name: String,
    description: String,
    disable_model_invocation: bool,
    contents: String,
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
    for install in &manifest.installs {
        match normalize_install(root, source_key, &manifest.source.id, install) {
            Ok(item) if items.contains_key(&item.local_id) => errors.push(CatalogError {
                path: SOURCE_MANIFEST_FILE.to_string(),
                message: format!("Duplicate install id: {}", item.local_id),
            }),
            Ok(item) => {
                items.insert(item.local_id.clone(), item);
            }
            Err(message) => errors.push(CatalogError {
                path: install.source.clone(),
                message,
            }),
        }
    }

    if items.is_empty() {
        let detail = errors.first().map_or(String::new(), |error| {
            format!(" {}: {}", error.path, error.message)
        });
        return Err(format!(
            "The manifest does not contain any valid installs.{detail}"
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

fn normalize_install(
    root: &Path,
    source_key: &str,
    source_id: &str,
    install: &ManifestInstall,
) -> Result<CatalogItem, String> {
    validate_local_id(&install.id)?;
    let source = validate_relative_path(&install.source, "source")?;
    let destination = resolve_destination(&install.destination)?;
    let source_path = root.join(&source);
    let metadata = fs::symlink_metadata(&source_path)
        .map_err(|error| format!("Could not inspect {}: {error}", install.source))?;
    if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
        return Err(format!(
            "{} is not a regular file or directory.",
            install.source
        ));
    }

    let parsed_skill = if metadata.is_dir() && source_path.join("SKILL.md").is_file() {
        Some(parse_skill(&source_path.join("SKILL.md"))?)
    } else {
        None
    };
    let (name, description, materialized_skill_name) = if let Some(parsed) = &parsed_skill {
        let source_name = source_path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| format!("{} has a non-UTF-8 basename.", install.source))?;
        if parsed.local_name != install.id || source_name != install.id {
            return Err(format!(
                "Agent Skill name, install id, and source directory must match (found {:?}, {:?}, and {:?}).",
                parsed.local_name, install.id, source_name
            ));
        }
        let effective = format!("{source_id}-{}", install.id);
        if effective.len() > 64 || !valid_name(&effective) {
            return Err(format!(
                "The installed Agent Skill name {effective:?} exceeds the 64-character portable name contract."
            ));
        }
        if destination.path.file_name().and_then(OsStr::to_str) != Some(effective.as_str()) {
            return Err(format!(
                "Agent Skill destination must end in its installed name {effective:?}."
            ));
        }
        (
            effective.clone(),
            parsed.description.clone(),
            Some(effective),
        )
    } else {
        (
            install.id.clone(),
            format!("Installs {} to {}.", install.source, install.destination),
            None,
        )
    };
    let id = format!("{source_id}/{}", install.id);
    let digest = item_digest(
        &source_path,
        &id,
        &install.source,
        &destination,
        parsed_skill.as_ref(),
        materialized_skill_name.as_deref(),
    )?;
    Ok(CatalogItem {
        id,
        local_id: install.id.clone(),
        source_id: source_id.to_string(),
        source_key: source_key.to_string(),
        name,
        description,
        disable_model_invocation: parsed_skill
            .as_ref()
            .is_some_and(|skill| skill.disable_model_invocation),
        digest,
        source: install.source.clone(),
        source_is_directory: metadata.is_dir(),
        destination,
        materialized_skill_name,
    })
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

fn resolve_destination(value: &str) -> Result<ResolvedDestination, String> {
    let path = if let Some(relative) = value.strip_prefix("~/") {
        let relative = validate_relative_path(relative, "destination")?;
        destination_home()?.join(relative)
    } else {
        let path = PathBuf::from(value);
        if value.is_empty() || !path.is_absolute() {
            return Err(format!(
                "Invalid destination path {value:?}; use an absolute path or ~/ for your home directory."
            ));
        }
        path
    };
    if path.file_name().is_none() {
        return Err(format!(
            "Invalid destination path {value:?}; a filesystem root cannot be a destination."
        ));
    }
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {}
            Component::Normal(component) => validate_portable_component(component, &path)?,
            Component::CurDir | Component::ParentDir => {
                return Err(format!(
                    "Invalid destination path {value:?}; . and .. components are not allowed."
                ));
            }
        }
    }
    Ok(ResolvedDestination {
        declared: value.to_string(),
        path,
    })
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
                normalized_path(&item.destination.path).to_lowercase(),
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

fn item_digest(
    source_path: &Path,
    id: &str,
    source: &str,
    destination: &ResolvedDestination,
    parsed_skill: Option<&ParsedSkill>,
    materialized_name: Option<&str>,
) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, id.as_bytes());
    hash_field(&mut hasher, source.as_bytes());
    hash_field(&mut hasher, destination.declared.as_bytes());
    if source_path.is_dir() {
        hash_field(&mut hasher, directory_digest(source_path)?.as_bytes());
    } else {
        let bytes = fs::read(source_path)
            .map_err(|error| format!("Could not read {}: {error}", source_path.display()))?;
        hash_field(&mut hasher, &bytes);
    }
    if let (Some(skill), Some(name)) = (parsed_skill, materialized_name) {
        hash_field(
            &mut hasher,
            render_skill_markdown(&skill.contents, name)?.as_bytes(),
        );
    }
    Ok(hex_digest(hasher.finalize()))
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

fn parse_skill(path: &Path) -> Result<ParsedSkill, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let (frontmatter, _) = split_skill_markdown(&contents)?;
    let mapping = serde_yaml_ng::from_str::<Mapping>(frontmatter)
        .map_err(|error| format!("SKILL.md frontmatter is invalid YAML: {error}"))?;
    let local_name = required_string(&mapping, "name")?;
    if local_name.len() > 64 || !valid_name(&local_name) {
        return Err(format!(
            "SKILL.md has an invalid Agent Skill name: {local_name}"
        ));
    }
    let description = required_string(&mapping, "description")?;
    if description.chars().count() > 1024 {
        return Err("SKILL.md description exceeds 1024 characters.".to_string());
    }
    let disable_model_invocation = optional_boolean(&mapping, "disable-model-invocation")?;
    Ok(ParsedSkill {
        local_name,
        description,
        disable_model_invocation,
        contents,
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

    fn write_manifest(root: &Path, source_id: &str, installs: &str) {
        fs::write(
            root.join(SOURCE_MANIFEST_FILE),
            format!(
                r#"{{"version":1,"source":{{"id":"{source_id}","name":"Test","description":"Test source."}},"installs":{installs}}}"#
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
            r#"[{"id":"review","source":"skills/review","destination":"~/.agents/skills/acme-review"}]"#,
        );
        let catalog = read_manifest_catalog(root.path(), "source-key").expect("catalog");
        let item = &catalog.items["review"];
        assert_eq!(item.name, "acme-review");
        assert_eq!(item.description, "A test skill.");
        assert_eq!(item.materialized_skill_name.as_deref(), Some("acme-review"));
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
    fn invalid_installs_are_reported_without_hiding_valid_installs() {
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(root.path().join("valid.txt"), "valid").expect("file");
        write_manifest(
            root.path(),
            "acme",
            r#"[
              {"id":"valid","source":"valid.txt","destination":"~/.config/acme/valid.txt"},
              {"id":"missing","source":"missing.txt","destination":"~/.config/acme/missing.txt"}
            ]"#,
        );
        let catalog = read_manifest_catalog(root.path(), "source-key").expect("partial catalog");
        assert_eq!(catalog.items.len(), 1);
        assert_eq!(catalog.errors.len(), 1);
    }

    #[test]
    fn overlapping_destinations_are_rejected() {
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(root.path().join("one"), "one").expect("file");
        fs::write(root.path().join("two"), "two").expect("file");
        write_manifest(
            root.path(),
            "acme",
            r#"[
              {"id":"one","source":"one","destination":"~/shared"},
              {"id":"two","source":"two","destination":"~/shared/nested"}
            ]"#,
        );
        assert!(read_manifest_catalog(root.path(), "source-key")
            .expect_err("overlap")
            .contains("overlapping"));
    }
}
