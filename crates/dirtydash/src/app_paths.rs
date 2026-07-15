use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_path: PathBuf,
    pub db_path: PathBuf,
    /// Collector state is intentionally separate from the local dashboard
    /// history database. It contains manifests, credentials, and the durable
    /// outbound outbox, not session bodies.
    pub collector_db_path: PathBuf,
}

impl AppPaths {
    pub fn resolve(config_override: Option<PathBuf>, db_override: Option<PathBuf>) -> Result<Self> {
        let project_dirs = ProjectDirs::from("dev", "dirtydash", "dirtydash")
            .context("could not resolve platform app directories")?;

        let config_path =
            config_override.unwrap_or_else(|| project_dirs.config_dir().join("config.toml"));
        let db_path =
            db_override.unwrap_or_else(|| project_dirs.data_dir().join("dirtydash.sqlite3"));
        let collector_db_path = db_path.with_file_name(format!(
            "{}-collector.sqlite3",
            db_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("dirtydash")
        ));

        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating config directory {}", parent.display()))?;
        }
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating data directory {}", parent.display()))?;
        }

        Ok(Self {
            config_path,
            db_path,
            collector_db_path,
        })
    }
}
