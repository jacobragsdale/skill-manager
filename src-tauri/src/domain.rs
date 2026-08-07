//! Tauri-free source, catalog, status, ownership, and destination models.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

pub(crate) const BUILT_IN_SOURCE_ID: &str = "skillbook";
pub(crate) const CATALOG_SOURCE: &str = "https://github.com/jacobragsdale/skillbook";
pub(crate) const BUILT_IN_SOURCE_NAME: &str = "skillbook";
pub(crate) const MARKER_FILE: &str = ".skill-manager-managed";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogError {
    pub(crate) path: String,
    pub(crate) message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SkillStatus {
    Available,
    Installed,
    UpdateAvailable,
    Removed,
    Modified,
    UnmanagedMatch,
    Conflict,
    SourceConflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SourceStatus {
    Fresh,
    Cached,
    Error,
}

#[derive(Debug)]
pub(crate) struct CatalogSkill {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) digest: String,
}

#[derive(Debug, Default)]
pub(crate) struct CatalogContents {
    pub(crate) skills: BTreeMap<String, CatalogSkill>,
    pub(crate) errors: Vec<CatalogError>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct InstallMarker {
    pub(crate) version: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_id: Option<String>,
    pub(crate) source: String,
    pub(crate) skill_digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct CatalogMetadata {
    pub(crate) version: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_id: Option<String>,
    pub(crate) source: String,
    pub(crate) commit_sha: String,
    pub(crate) etag: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct SourceDefinition {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) url: String,
}

impl SourceDefinition {
    pub(crate) fn built_in() -> Self {
        Self {
            id: BUILT_IN_SOURCE_ID.to_string(),
            name: BUILT_IN_SOURCE_NAME.to_string(),
            url: CATALOG_SOURCE.to_string(),
        }
    }

    pub(crate) fn is_built_in(&self) -> bool {
        self.id == BUILT_IN_SOURCE_ID
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct SourcesConfig {
    pub(crate) version: u8,
    pub(crate) sources: Vec<SourceDefinition>,
}

#[derive(Debug)]
pub(crate) enum InstallOwnership {
    Unmanaged,
    Legacy,
    Managed(InstallMarker),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DestinationAnchor {
    UserHome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Destination {
    anchor: DestinationAnchor,
    relative_path: PathBuf,
}

impl Destination {
    pub(crate) fn user_skills_root() -> Self {
        Self {
            anchor: DestinationAnchor::UserHome,
            relative_path: PathBuf::from(".agents").join("skills"),
        }
    }

    pub(crate) fn user_skill(name: &str) -> Result<Self, String> {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains(['/', '\\'])
            || !portable_component(name)
        {
            return Err(format!("Invalid skill destination name: {name}"));
        }
        Self::new(
            DestinationAnchor::UserHome,
            Path::new(".agents").join("skills").join(name),
        )
    }

    pub(crate) fn new(anchor: DestinationAnchor, relative_path: PathBuf) -> Result<Self, String> {
        if relative_path.as_os_str().is_empty() || relative_path.is_absolute() {
            return Err("A destination must be a non-empty relative path.".to_string());
        }
        if relative_path.components().any(|component| match component {
            Component::Normal(part) => part.to_str().is_none_or(|name| !portable_component(name)),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => true,
        }) {
            return Err("A destination may not escape its anchor.".to_string());
        }
        Ok(Self {
            anchor,
            relative_path,
        })
    }

    pub(crate) fn resolve(&self, user_home: &Path) -> PathBuf {
        match self.anchor {
            DestinationAnchor::UserHome => user_home.join(&self.relative_path),
        }
    }

    #[cfg(test)]
    pub(crate) fn relative_path(&self) -> &Path {
        &self.relative_path
    }
}

fn portable_component(name: &str) -> bool {
    let stem = name
        .split_once('.')
        .map_or(name, |(before_extension, _)| before_extension)
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            });
    !name.is_empty()
        && !name.ends_with([' ', '.'])
        && name.encode_utf16().count() <= 255
        && !reserved
        && !name.chars().any(|character| {
            character < '\u{20}'
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_skill_destination_resolves_from_the_home_anchor() {
        let destination = Destination::user_skill("python-standards").expect("destination");
        assert_eq!(
            destination.resolve(Path::new("/home/tester")),
            Path::new("/home/tester/.agents/skills/python-standards")
        );
        assert_eq!(
            destination.relative_path(),
            Path::new(".agents/skills/python-standards")
        );
    }

    #[test]
    fn destinations_reject_absolute_and_escaping_paths() {
        assert!(
            Destination::new(DestinationAnchor::UserHome, PathBuf::from("/tmp/skill")).is_err()
        );
        assert!(Destination::new(
            DestinationAnchor::UserHome,
            PathBuf::from(".agents/../outside")
        )
        .is_err());
        assert!(Destination::user_skill("../outside").is_err());
        assert!(Destination::user_skill("nested/skill").is_err());
        assert!(Destination::user_skill("con").is_err());
        assert!(Destination::new(
            DestinationAnchor::UserHome,
            PathBuf::from(".agents/bad:name")
        )
        .is_err());
    }
}
