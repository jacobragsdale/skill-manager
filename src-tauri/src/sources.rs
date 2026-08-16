//! Source tree validation and file copying.

use crate::catalog_v1::{relative_path, validate_portable_component};
use crate::parallel;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const MAX_SOURCE_BYTES: u64 = 50 * 1024 * 1024;
pub(crate) const MAX_SOURCE_FILES: usize = 2_000;

pub(crate) fn cache_base_dir() -> Result<PathBuf, String> {
    crate::paths::SystemPaths::cache_base()
}

pub(crate) fn config_base_dir() -> Result<PathBuf, String> {
    crate::paths::SystemPaths::config_base()
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        fs::File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("Could not synchronize {}: {error}", path.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

pub(crate) fn valid_commit_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn temporary_path(parent: &Path, label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    parent.join(format!(".{label}-{}-{nonce}", std::process::id()))
}

pub(crate) fn validate_catalog_tree(root: &Path) -> Result<(), String> {
    fn visit(
        root: &Path,
        directory: &Path,
        seen_paths: &mut BTreeSet<String>,
        file_count: &mut usize,
        total_bytes: &mut u64,
    ) -> Result<(), String> {
        for entry in fs::read_dir(directory)
            .map_err(|error| format!("Could not read {}: {error}", directory.display()))?
        {
            let entry = entry
                .map_err(|error| format!("Could not read {}: {error}", directory.display()))?;
            let path = entry.path();
            validate_portable_component(&entry.file_name(), &path)?;
            let relative = relative_path(root, &path)?;
            if !seen_paths.insert(relative.to_lowercase()) {
                return Err(format!(
                    "Source paths collide on a case-insensitive filesystem: {relative}"
                ));
            }
            let file_type = entry
                .file_type()
                .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?;
            if file_type.is_dir() {
                visit(root, &path, seen_paths, file_count, total_bytes)?;
            } else if file_type.is_file() {
                *file_count = file_count
                    .checked_add(1)
                    .ok_or_else(|| "The source contains too many files.".to_string())?;
                if *file_count > MAX_SOURCE_FILES {
                    return Err(format!(
                        "The source contains more than {MAX_SOURCE_FILES} files."
                    ));
                }
                *total_bytes = total_bytes
                    .checked_add(
                        entry
                            .metadata()
                            .map_err(|error| {
                                format!("Could not inspect {}: {error}", path.display())
                            })?
                            .len(),
                    )
                    .ok_or_else(|| "The source is too large.".to_string())?;
                if *total_bytes > MAX_SOURCE_BYTES {
                    return Err("The source expands beyond 50 MB.".to_string());
                }
            } else {
                return Err(format!(
                    "Source entry is not a regular file or directory: {}",
                    path.display()
                ));
            }
        }
        Ok(())
    }
    let mut seen_paths = BTreeSet::new();
    let mut file_count = 0;
    let mut total_bytes = 0;
    visit(
        root,
        root,
        &mut seen_paths,
        &mut file_count,
        &mut total_bytes,
    )
}

fn plan_directory_copy(
    source: &Path,
    target: &Path,
    files: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), String> {
    fs::create_dir(target)
        .map_err(|error| format!("Could not create {}: {error}", target.display()))?;
    let mut entries = fs::read_dir(source)
        .map_err(|error| format!("Could not read {}: {error}", source.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Could not read {}: {error}", source.display()))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let source_path = entry.path();
        let destination = target.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Could not inspect {}: {error}", source_path.display()))?;
        if file_type.is_dir() {
            plan_directory_copy(&source_path, &destination, files)?;
        } else if file_type.is_file() {
            files.push((source_path, destination));
        } else {
            return Err(format!(
                "{} is not a regular file or directory",
                source_path.display()
            ));
        }
    }
    Ok(())
}

pub(crate) fn copy_directory(source: &Path, target: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    plan_directory_copy(source, target, &mut files)?;
    parallel::try_map(&files, |(source_path, destination)| {
        fs::copy(source_path, destination).map_err(|error| {
            format!(
                "Could not copy {} to {}: {error}",
                source_path.display(),
                destination.display()
            )
        })
    })
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_tree_accepts_regular_files() {
        let root = tempfile::tempdir().expect("root");
        fs::write(root.path().join("skill-manager.json"), "{}").expect("manifest");
        validate_catalog_tree(root.path()).expect("valid");
    }
}
