//! Transactional installation and ownership protection for one destination per item.

use crate::catalog_v1::{materialize_agent_skill, CatalogItem, ResolvedDestination};
use crate::fs_retry;
use crate::ledger::{self, InstallationLedger, InstallationRecord, OwnedPath, OwnedPathKind};
use crate::manifest::DestinationAnchor;
use crate::source_v1::{ConfiguredSource, SourceSnapshot};
use crate::sources::{copy_directory, temporary_path};
use serde::Serialize;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub(crate) struct AnchorPaths {
    pub(crate) home: PathBuf,
    pub(crate) config: PathBuf,
    pub(crate) data: PathBuf,
    pub(crate) local_data: PathBuf,
    pub(crate) cache: PathBuf,
}

impl AnchorPaths {
    pub(crate) fn from_system() -> Result<Self, String> {
        if let Some(root) = crate::qa_paths::root()? {
            return Ok(Self {
                home: root.join("home"),
                config: root.join("config"),
                data: root.join("data"),
                local_data: root.join("local-data"),
                cache: root.join("cache"),
            });
        }
        Ok(Self {
            home: dirs::home_dir()
                .ok_or_else(|| "Could not find your home directory.".to_string())?,
            config: dirs::config_dir()
                .ok_or_else(|| "Could not find your configuration directory.".to_string())?,
            data: dirs::data_dir()
                .ok_or_else(|| "Could not find your data directory.".to_string())?,
            local_data: dirs::data_local_dir()
                .ok_or_else(|| "Could not find your local data directory.".to_string())?,
            cache: dirs::cache_dir()
                .ok_or_else(|| "Could not find your cache directory.".to_string())?,
        })
    }

    pub(crate) fn app_data(&self) -> PathBuf {
        self.data.join("skill-manager")
    }

    pub(crate) fn resolve(&self, destination: &ResolvedDestination) -> Result<PathBuf, String> {
        self.resolve_relative(destination.anchor, &destination.path)
    }

    pub(crate) fn resolve_owned(&self, owned: &OwnedPath) -> Result<PathBuf, String> {
        self.resolve_relative(owned.anchor, Path::new(&owned.path))
    }

    fn resolve_relative(
        &self,
        anchor: DestinationAnchor,
        relative: &Path,
    ) -> Result<PathBuf, String> {
        if relative.as_os_str().is_empty() || relative.is_absolute() {
            return Err("Owned destinations must be non-empty relative paths.".to_string());
        }
        if relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err("Owned destinations may not escape their anchor.".to_string());
        }
        let root = match anchor {
            DestinationAnchor::Home => &self.home,
            DestinationAnchor::Config => &self.config,
            DestinationAnchor::Data => &self.data,
            DestinationAnchor::LocalData => &self.local_data,
            DestinationAnchor::Cache => &self.cache,
        };
        let resolved = root.join(relative);
        let state_roots = [
            self.config.join("skill-manager"),
            self.data.join("skill-manager"),
            self.local_data.join("skill-manager"),
            self.cache.join("skill-manager"),
        ];
        if state_roots
            .iter()
            .any(|state_root| resolved == *state_root || resolved.starts_with(state_root))
        {
            return Err(format!(
                "Destination {} is inside Skill Manager's own state.",
                resolved.display()
            ));
        }
        Ok(resolved)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ItemStatus {
    Available,
    Installed,
    UpdateAvailable,
    Removed,
    Modified,
    Conflict,
    SourceConflict,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationOutcome {
    pub(crate) backup_paths: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemovalPathWarning {
    pub(crate) path: String,
    pub(crate) modified: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemovalItemPlan {
    pub(crate) id: String,
    pub(crate) paths: Vec<RemovalPathWarning>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceRemovalPlan {
    pub(crate) source_id: String,
    pub(crate) items: Vec<RemovalItemPlan>,
}

struct StagedInstall {
    staging: PathBuf,
    target: PathBuf,
    owned: OwnedPath,
}

struct MovedPath {
    target: PathBuf,
    backup: PathBuf,
    persistent: bool,
}

pub(crate) fn item_status(
    anchors: &AnchorPaths,
    ledger: &InstallationLedger,
    item: Option<&CatalogItem>,
    canonical_id: &str,
) -> ItemStatus {
    let Some(record) = ledger.items.get(canonical_id) else {
        return item.map_or(ItemStatus::Removed, |item| {
            if anchors
                .resolve(&item.destination)
                .is_ok_and(|path| path_entry_exists(&path))
            {
                ItemStatus::Conflict
            } else {
                ItemStatus::Available
            }
        });
    };
    if item.is_some_and(|item| item.source_key != record.source_key) {
        return ItemStatus::SourceConflict;
    }
    if !record_path_matches(anchors, record) {
        return ItemStatus::Modified;
    }
    match item {
        None => ItemStatus::Removed,
        Some(item) if item.digest != record.item_digest => ItemStatus::UpdateAvailable,
        Some(_) => ItemStatus::Installed,
    }
}

pub(crate) fn install_item(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
) -> Result<OperationOutcome, String> {
    install_item_with_policy(anchors, source, snapshot, item, false)
}

pub(crate) fn replace_item(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
) -> Result<OperationOutcome, String> {
    install_item_with_policy(anchors, source, snapshot, item, true)
}

fn install_item_with_policy(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
    replace_unmanaged: bool,
) -> Result<OperationOutcome, String> {
    let mut ledger_state = ledger::read(&anchors.app_data())?;
    let existing = ledger_state.items.get(&item.id).cloned();
    if replace_unmanaged && existing.is_some() {
        return Err(format!(
            "{} is already managed; use the normal update operation.",
            item.id
        ));
    }
    if let Some(record) = &existing {
        if record.source_key != source.source_key {
            return Err(format!("{} is owned by a different source.", item.id));
        }
        if !record_path_matches(anchors, record) {
            return Err(format!(
                "{} contains local changes and cannot be updated.",
                item.id
            ));
        }
        if record.item_digest == item.digest {
            return Err(format!("{} is already installed.", item.id));
        }
    }

    let staged = stage_item(anchors, snapshot, item)?;
    let new_record = InstallationRecord {
        source_key: source.source_key.clone(),
        source_url: source.url.clone(),
        source_id: source.source_id.clone(),
        local_id: item.local_id.clone(),
        commit: snapshot.commit.clone(),
        item_digest: item.digest.clone(),
        name: item.name.clone(),
        description: item.description.clone(),
        source: item.source.clone(),
        destination: staged.owned.clone(),
    };
    let backup_paths = activate_install(
        anchors,
        &mut ledger_state,
        &item.id,
        existing.as_ref(),
        staged,
        new_record,
        replace_unmanaged,
    )?;
    Ok(OperationOutcome {
        backup_paths: backup_paths
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
    })
}

pub(crate) fn uninstall_item(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
    canonical_id: &str,
    force_modified: bool,
) -> Result<OperationOutcome, String> {
    let mut ledger_state = ledger::read(&anchors.app_data())?;
    let record = ledger_state
        .items
        .get(canonical_id)
        .cloned()
        .ok_or_else(|| format!("{canonical_id} is not installed."))?;
    if record.source_key != source.source_key {
        return Err(format!("{canonical_id} is owned by a different source."));
    }
    if !force_modified && !record_path_matches(anchors, &record) {
        return Err(format!(
            "{canonical_id} contains local changes and cannot be uninstalled."
        ));
    }
    let target = anchors.resolve_owned(&record.destination)?;
    let moved = if path_entry_exists(&target) {
        let backup = temporary_path(
            target.parent().expect("destination parent"),
            "item-removing",
        );
        fs_retry::rename(&target, &backup).map_err(|error| {
            format!(
                "Could not prepare {} for removal: {error}",
                target.display()
            )
        })?;
        Some(MovedPath {
            target,
            backup,
            persistent: false,
        })
    } else {
        None
    };
    ledger_state.items.remove(canonical_id);
    if let Err(error) = ledger::write(&anchors.app_data(), &ledger_state) {
        if let Some(moved) = &moved {
            let _ = fs_retry::rename(&moved.backup, &moved.target);
        }
        return Err(error);
    }
    if let Some(moved) = moved {
        remove_path_result(&moved.backup)?;
    }
    Ok(OperationOutcome::default())
}

pub(crate) fn source_removal_plan(
    anchors: &AnchorPaths,
    source: &ConfiguredSource,
) -> Result<SourceRemovalPlan, String> {
    let ledger_state = ledger::read(&anchors.app_data())?;
    let mut items = ledger_state
        .items
        .iter()
        .filter(|(_, record)| record.source_key == source.source_key)
        .map(|(id, record)| {
            let path = anchors.resolve_owned(&record.destination)?;
            Ok(RemovalItemPlan {
                id: id.clone(),
                paths: vec![RemovalPathWarning {
                    path: path.display().to_string(),
                    modified: !record_path_matches(anchors, record),
                }],
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    items.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(SourceRemovalPlan {
        source_id: source.source_id.clone(),
        items,
    })
}

fn stage_item(
    anchors: &AnchorPaths,
    snapshot: &SourceSnapshot,
    item: &CatalogItem,
) -> Result<StagedInstall, String> {
    let source = snapshot.path.join(&item.source);
    let target = anchors.resolve(&item.destination)?;
    let parent = target
        .parent()
        .ok_or_else(|| format!("{} has no parent.", target.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    let label = target
        .file_name()
        .and_then(OsStr::to_str)
        .map_or("item-installing".to_string(), |name| {
            format!("{name}-installing")
        });
    let staging = temporary_path(parent, &label);
    let result = if source.is_dir() {
        if let Some(effective_name) = item.materialized_skill_name.as_deref() {
            materialize_agent_skill(&source, &staging, effective_name)?;
        } else {
            copy_directory(&source, &staging)?;
        }
        ledger::path_digest(&staging, OwnedPathKind::Directory)
            .map(|digest| (OwnedPathKind::Directory, digest))
    } else if source.is_file() {
        fs::copy(&source, &staging).map_err(|error| {
            format!(
                "Could not stage {} at {}: {error}",
                source.display(),
                staging.display()
            )
        })?;
        ledger::path_digest(&staging, OwnedPathKind::File)
            .map(|digest| (OwnedPathKind::File, digest))
    } else {
        Err(format!("Source {} no longer exists.", source.display()))
    };
    match result {
        Ok((kind, installed_digest)) => Ok(StagedInstall {
            staging,
            target,
            owned: OwnedPath {
                anchor: item.destination.anchor,
                path: normalized_relative(&item.destination.path),
                kind,
                installed_digest,
            },
        }),
        Err(error) => {
            remove_path(&staging);
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn activate_install(
    anchors: &AnchorPaths,
    ledger_state: &mut InstallationLedger,
    id: &str,
    previous: Option<&InstallationRecord>,
    staged: StagedInstall,
    new_record: InstallationRecord,
    replace_unmanaged: bool,
) -> Result<Vec<PathBuf>, String> {
    let previous_target = previous
        .map(|record| anchors.resolve_owned(&record.destination))
        .transpose()?;
    let target_is_previous = previous_target.as_ref() == Some(&staged.target);
    let unmanaged_target = path_entry_exists(&staged.target) && !target_is_previous;
    if unmanaged_target && !replace_unmanaged {
        remove_path(&staged.staging);
        return Err(format!(
            "{} already exists and is not an owned destination.",
            staged.target.display()
        ));
    }

    let persistent_root = if unmanaged_target {
        Some(next_backup_path(&anchors.home, &id.replace('/', "-"))?)
    } else {
        None
    };
    let mut targets = Vec::new();
    if path_entry_exists(&staged.target) {
        targets.push((staged.target.clone(), unmanaged_target));
    }
    if let Some(previous_target) = previous_target {
        if previous_target != staged.target && path_entry_exists(&previous_target) {
            targets.push((previous_target, false));
        }
    }
    let mut moved = Vec::new();
    for (index, (target, persistent)) in targets.into_iter().enumerate() {
        let backup = if persistent {
            let filename = target
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("destination");
            persistent_root
                .as_ref()
                .expect("unmanaged targets have a backup root")
                .join(format!("{index}-{filename}"))
        } else {
            temporary_path(
                target.parent().expect("destination parent"),
                "item-previous",
            )
        };
        if let Err(error) = fs_retry::rename(&target, &backup) {
            rollback_moved(&moved);
            remove_path(&staged.staging);
            return Err(format!("Could not prepare {}: {error}", target.display()));
        }
        moved.push(MovedPath {
            target,
            backup,
            persistent,
        });
    }
    if let Err(error) = fs_retry::rename(&staged.staging, &staged.target) {
        rollback_moved(&moved);
        return Err(format!(
            "Could not activate {}: {error}",
            staged.target.display()
        ));
    }
    ledger_state.items.insert(id.to_string(), new_record);
    if let Err(error) = ledger::write(&anchors.app_data(), ledger_state) {
        remove_path(&staged.target);
        rollback_moved(&moved);
        return Err(error);
    }
    let persistent = moved
        .iter()
        .filter(|moved| moved.persistent)
        .map(|moved| moved.backup.clone())
        .collect::<Vec<_>>();
    for moved in moved.iter().filter(|moved| !moved.persistent) {
        remove_path_result(&moved.backup)?;
    }
    Ok(persistent)
}

fn record_path_matches(anchors: &AnchorPaths, record: &InstallationRecord) -> bool {
    anchors
        .resolve_owned(&record.destination)
        .is_ok_and(|path| {
            path_entry_exists(&path)
                && ledger::path_digest(&path, record.destination.kind)
                    .is_ok_and(|digest| digest == record.destination.installed_digest)
        })
}

fn normalized_relative(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn path_entry_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn current_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn next_backup_path(home: &Path, name: &str) -> Result<PathBuf, String> {
    let parent = home
        .join(".agents")
        .join(".skill-manager-backups")
        .join(name);
    fs::create_dir_all(&parent)
        .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    let timestamp = current_epoch_seconds();
    for suffix in 0..10_000_u16 {
        let directory_name = if suffix == 0 {
            timestamp.to_string()
        } else {
            format!("{timestamp}-{suffix}")
        };
        let candidate = parent.join(directory_name);
        if !path_entry_exists(&candidate) {
            fs::create_dir(&candidate)
                .map_err(|error| format!("Could not create {}: {error}", candidate.display()))?;
            return Ok(candidate);
        }
    }
    Err(format!(
        "Could not choose a unique backup path in {}.",
        parent.display()
    ))
}

fn rollback_moved(moved: &[MovedPath]) {
    for moved in moved.iter().rev() {
        let _ = fs_retry::rename(&moved.backup, &moved.target);
    }
}

fn remove_path(path: &Path) {
    if path_entry_exists(path) {
        let _ = remove_path_result(path);
    }
}

fn remove_path_result(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_dir() {
        fs_retry::remove_dir_all(path)
            .map_err(|error| format!("Could not remove {}: {error}", path.display()))
    } else {
        fs_retry::remove_file(path)
            .map_err(|error| format!("Could not remove {}: {error}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_v1::read_manifest_catalog;
    use crate::source_v1::BUILT_IN_SOURCE_KEY;

    fn anchors(root: &Path) -> AnchorPaths {
        AnchorPaths {
            home: root.join("home"),
            config: root.join("config"),
            data: root.join("data"),
            local_data: root.join("local-data"),
            cache: root.join("cache"),
        }
    }

    fn snapshot(root: &Path) -> (ConfiguredSource, SourceSnapshot, CatalogItem) {
        let source_root = root.join("source");
        let skill = source_root.join("skills/review");
        fs::create_dir_all(&skill).expect("skill");
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: review\ndescription: Review code\nlicense: MIT\n---\nBody\n",
        )
        .expect("skill");
        fs::create_dir(skill.join("scripts")).expect("scripts");
        let script = skill.join("scripts/check.sh");
        fs::write(&script, "#!/bin/sh\nexit 0\n").expect("script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
                .expect("executable script");
        }
        fs::write(
            source_root.join("skill-manager.json"),
            r#"{
              "version": 1,
              "source": { "id": "skillbook", "name": "Skillbook", "description": "Skills" },
              "installs": [{
                "id": "review",
                "source": "skills/review",
                "destination": { "anchor": "home", "path": ".agents/skills/skillbook-review" }
              }]
            }"#,
        )
        .expect("manifest");
        let catalog = read_manifest_catalog(&source_root, BUILT_IN_SOURCE_KEY).expect("catalog");
        let source = ConfiguredSource::built_in();
        let item = catalog.items["review"].clone();
        let snapshot = SourceSnapshot {
            definition: source.clone(),
            commit: "a".repeat(40),
            path: source_root,
            catalog,
        };
        (source, snapshot, item)
    }

    #[test]
    fn install_materializes_namespaced_skill_and_uninstall_removes_it() {
        let root = tempfile::tempdir().expect("root");
        let anchors = anchors(root.path());
        let (source, snapshot, item) = snapshot(root.path());
        install_item(&anchors, &source, &snapshot, &item).expect("install");
        let target = anchors.home.join(".agents/skills/skillbook-review");
        assert!(fs::read_to_string(target.join("SKILL.md"))
            .expect("skill")
            .contains("name: skillbook-review"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_ne!(
                fs::metadata(target.join("scripts/check.sh"))
                    .expect("script metadata")
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
        assert_eq!(
            item_status(
                &anchors,
                &ledger::read(&anchors.app_data()).expect("ledger"),
                Some(&item),
                &item.id
            ),
            ItemStatus::Installed
        );
        uninstall_item(&anchors, &source, &item.id, false).expect("uninstall");
        assert!(!target.exists());
    }

    #[test]
    fn modified_owned_path_is_protected() {
        let root = tempfile::tempdir().expect("root");
        let anchors = anchors(root.path());
        let (source, snapshot, item) = snapshot(root.path());
        install_item(&anchors, &source, &snapshot, &item).expect("install");
        let target = anchors.home.join(".agents/skills/skillbook-review");
        fs::write(target.join("local.txt"), "edit").expect("local edit");
        assert!(uninstall_item(&anchors, &source, &item.id, false)
            .expect_err("protected")
            .contains("local changes"));
        assert!(target.join("local.txt").exists());
    }

    #[test]
    #[cfg(unix)]
    fn unmanaged_replacement_keeps_backup_and_does_not_follow_symlink() {
        let root = tempfile::tempdir().expect("root");
        let anchors = anchors(root.path());
        let (source, snapshot, item) = snapshot(root.path());
        let target = anchors.home.join(".agents/skills/skillbook-review");
        let external = root.path().join("external");
        fs::create_dir_all(target.parent().expect("parent")).expect("parent");
        fs::create_dir_all(&external).expect("external");
        fs::write(external.join("keep.txt"), "untouched").expect("external file");
        std::os::unix::fs::symlink(&external, &target).expect("symlink");
        let outcome = replace_item(&anchors, &source, &snapshot, &item).expect("replace");
        assert_eq!(outcome.backup_paths.len(), 1);
        assert!(Path::new(&outcome.backup_paths[0]).is_symlink());
        assert_eq!(
            fs::read_to_string(external.join("keep.txt")).expect("external file"),
            "untouched"
        );
        assert!(target.is_dir());
    }

    #[test]
    fn source_removal_plan_reports_modified_destination() {
        let root = tempfile::tempdir().expect("root");
        let anchors = anchors(root.path());
        let (source, snapshot, item) = snapshot(root.path());
        install_item(&anchors, &source, &snapshot, &item).expect("install");
        fs::write(
            anchors
                .home
                .join(".agents/skills/skillbook-review/local.txt"),
            "edit",
        )
        .expect("edit");
        let plan = source_removal_plan(&anchors, &source).expect("plan");
        assert!(plan.items[0].paths[0].modified);
    }
}
