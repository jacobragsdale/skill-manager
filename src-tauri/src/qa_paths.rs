//! Debug-only state isolation for native application QA.

use std::path::PathBuf;

#[cfg(debug_assertions)]
const QA_ROOT_ENV: &str = "SKILL_MANAGER_QA_ROOT";

pub(crate) fn root() -> Result<Option<PathBuf>, String> {
    #[cfg(not(debug_assertions))]
    {
        Ok(None)
    }
    #[cfg(debug_assertions)]
    {
        let Some(value) = std::env::var_os(QA_ROOT_ENV) else {
            return Ok(None);
        };
        let root = PathBuf::from(value);
        let temporary = std::env::temp_dir();
        if !root.is_absolute() || root == temporary || !root.starts_with(&temporary) {
            return Err(format!(
                "{QA_ROOT_ENV} must name a dedicated absolute directory beneath {}.",
                temporary.display()
            ));
        }
        Ok(Some(root))
    }
}
