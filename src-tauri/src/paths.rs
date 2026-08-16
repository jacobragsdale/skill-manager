//! Host filesystem roots. Paths resolve; they do not recover journals or mutate state.

use crate::ledger::OwnedPath;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct SystemPaths {
    pub(crate) home: PathBuf,
    pub(crate) config: PathBuf,
    pub(crate) data: PathBuf,
    pub(crate) local_data: PathBuf,
    pub(crate) cache: PathBuf,
}

impl SystemPaths {
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

    pub(crate) fn cache_base() -> Result<PathBuf, String> {
        if let Some(root) = crate::qa_paths::root()? {
            return Ok(root.join("cache/skill-manager"));
        }
        dirs::cache_dir()
            .map(|directory| directory.join("skill-manager"))
            .ok_or_else(|| "Could not find your cache directory.".to_string())
    }

    pub(crate) fn config_base() -> Result<PathBuf, String> {
        if let Some(root) = crate::qa_paths::root()? {
            return Ok(root.join("config/skill-manager"));
        }
        dirs::config_dir()
            .map(|directory| directory.join("skill-manager"))
            .ok_or_else(|| "Could not find your configuration directory.".to_string())
    }

    pub(crate) fn resolve_owned(&self, owned: &OwnedPath) -> Result<PathBuf, String> {
        self.validate_destination(Path::new(&owned.path))
    }

    pub(crate) fn validate_destination(&self, path: &Path) -> Result<PathBuf, String> {
        if path.as_os_str().is_empty() || !path.is_absolute() || path.file_name().is_none() {
            return Err("Owned destinations must be non-root absolute paths.".to_string());
        }
        if path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        }) {
            return Err("Owned destinations may not contain . or .. components.".to_string());
        }
        let state_roots = [
            self.config.join("skill-manager"),
            self.data.join("skill-manager"),
            self.local_data.join("skill-manager"),
            self.cache.join("skill-manager"),
        ];
        if state_roots
            .iter()
            .any(|state_root| path == state_root || path.starts_with(state_root))
        {
            return Err(format!(
                "Destination {} is inside Agent Plugins' own state.",
                path.display()
            ));
        }
        Ok(path.to_path_buf())
    }
}
