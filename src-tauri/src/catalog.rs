//! Normalizes the conventional `skills/` tree into validated catalog entries.

use crate::digest::directory_digest;
use crate::domain::{CatalogContents, CatalogError, CatalogSkill, MARKER_FILE};
use crate::parallel;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

const CATALOG_METADATA_FILE: &str = ".skill-manager-catalog.json";

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

pub(crate) fn validate_portable_path_component(
    component: &OsStr,
    path: &Path,
) -> Result<(), String> {
    let Some(name) = component.to_str() else {
        return Err(format!("{} contains a non-UTF-8 path.", path.display()));
    };
    let utf16_length = name.encode_utf16().count();
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
        || utf16_length > 255
        || is_windows_reserved_name(name);
    if invalid {
        return Err(format!(
            "{} is not portable to Windows because it contains an invalid path component.",
            path.display()
        ));
    }
    Ok(())
}

pub(crate) fn valid_item_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !is_windows_reserved_name(name)
}

pub(crate) fn validate_item_name(name: &str, kind: &str) -> Result<(), String> {
    if valid_item_name(name) {
        Ok(())
    } else {
        Err(format!("Invalid {kind} name: {name}"))
    }
}

pub(crate) fn validate_skill_name(name: &str) -> Result<(), String> {
    validate_item_name(name, "skill")
}

fn frontmatter_value(contents: &str, key: &str) -> Option<String> {
    let frontmatter = contents.strip_prefix("---\n")?.split_once("\n---")?.0;
    let prefix = format!("{key}:");
    frontmatter.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(str::trim)
            .map(|value| value.trim_matches(['"', '\'']).to_string())
            .filter(|value| !value.is_empty())
    })
}

pub(crate) fn skill_frontmatter_at(
    path: &Path,
    display_path: &str,
) -> Result<(String, String), String> {
    let bytes =
        fs::read(path).map_err(|error| format!("Could not read {display_path}: {error}"))?;
    let contents = String::from_utf8(bytes)
        .map_err(|error| format!("{display_path} must be valid UTF-8: {error}"))?;
    let normalized = contents
        .strip_prefix('\u{feff}')
        .unwrap_or(&contents)
        .replace("\r\n", "\n");
    let name = frontmatter_value(&normalized, "name")
        .ok_or_else(|| format!("{display_path} is missing a name"))?;
    let description = frontmatter_value(&normalized, "description")
        .ok_or_else(|| format!("{display_path} is missing a description"))?;
    Ok((name, description))
}

pub(crate) fn skill_frontmatter(skill: &Path) -> Result<(String, String), String> {
    let path = skill.join("SKILL.md");
    skill_frontmatter_at(&path, &path.display().to_string())
}

pub(crate) fn relative_path(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| format!("{} is outside {}", path.display(), root.display()))?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(format!("{} has an invalid path", path.display()));
        };
        parts.push(
            part.to_str()
                .ok_or_else(|| format!("{} is not UTF-8", path.display()))?,
        );
    }
    Ok(parts.join("/"))
}

fn skills_catalog_root(root: &Path) -> PathBuf {
    let conventional = root.join("skills");
    if conventional.is_dir() {
        conventional
    } else {
        root.to_path_buf()
    }
}

pub(crate) fn skill_catalog_path(root: &Path, name: &str) -> PathBuf {
    skills_catalog_root(root).join(name)
}

fn read_catalog_skill(entry: &fs::DirEntry) -> Option<Result<CatalogSkill, CatalogError>> {
    let path = entry.path();
    let Some(name) = entry.file_name().to_str().map(str::to_string) else {
        return Some(Err(CatalogError {
            path: "skills".to_string(),
            message: "A catalog skill has a non-UTF-8 name.".to_string(),
        }));
    };
    if name == CATALOG_METADATA_FILE {
        return None;
    }
    let repository_skill_path = format!("skills/{name}");
    let result = (|| {
        if !entry
            .file_type()
            .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?
            .is_dir()
        {
            return Err("Skill entries must be directories.".to_string());
        }
        validate_skill_name(&name)?;
        if path.join(MARKER_FILE).exists() {
            return Err("The skill contains the reserved marker file.".to_string());
        }
        let repository_skill_file = format!("{repository_skill_path}/SKILL.md");
        let (declared_name, description) =
            skill_frontmatter_at(&path.join("SKILL.md"), &repository_skill_file)?;
        if declared_name != name {
            return Err(format!(
                "{repository_skill_file} declares the name {declared_name}, expected {name}"
            ));
        }
        Ok(CatalogSkill {
            name: name.clone(),
            description,
            digest: directory_digest(&path)?,
        })
    })();
    Some(result.map_err(|message| CatalogError {
        path: repository_skill_path,
        message,
    }))
}

fn read_catalog_skills(
    root: &Path,
    errors: &mut Vec<CatalogError>,
) -> Result<BTreeMap<String, CatalogSkill>, String> {
    let skills_root = skills_catalog_root(root);
    if !skills_root.is_dir() {
        return Ok(BTreeMap::new());
    }
    let mut entries = fs::read_dir(&skills_root)
        .map_err(|error| format!("Could not read catalog {}: {error}", skills_root.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Could not read catalog {}: {error}", skills_root.display()))?;
    entries.sort_by_key(fs::DirEntry::file_name);

    let mut skills = BTreeMap::new();
    for outcome in parallel::map(&entries, read_catalog_skill) {
        match outcome {
            Some(Ok(skill)) => {
                skills.insert(skill.name.clone(), skill);
            }
            Some(Err(error)) => errors.push(error),
            None => {}
        }
    }
    Ok(skills)
}

pub(crate) fn catalog_contents(root: &Path) -> Result<CatalogContents, String> {
    let mut errors = Vec::new();
    let skills = read_catalog_skills(root, &mut errors)?;
    if skills.is_empty() {
        let detail = errors.first().map_or(String::new(), |error| {
            format!(" {}: {}", error.path, error.message)
        });
        return Err(format!(
            "The catalog does not contain any valid skills.{detail}"
        ));
    }
    Ok(CatalogContents { skills, errors })
}

#[cfg(test)]
pub(crate) fn catalog_skills(root: &Path) -> Result<BTreeMap<String, CatalogSkill>, String> {
    Ok(catalog_contents(root)?.skills)
}
