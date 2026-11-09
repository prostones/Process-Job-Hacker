use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::model::policy::Policy;

const QUALIFIER: &str = "com";
const ORGANIZATION: &str = "prostones";
const APPLICATION: &str = "ProcessJobHacker";

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
}

pub fn config_dir() -> PathBuf {
    project_dirs()
        .map(|dirs| dirs.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn local_data_dir() -> PathBuf {
    project_dirs()
        .map(|dirs| dirs.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn journal_path() -> PathBuf {
    local_data_dir().join("active_jobs.json")
}

pub fn log_dir() -> PathBuf {
    local_data_dir().join("logs")
}

pub const CONFIG_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub version: u32,
    pub refresh_visible_ms: u64,
    pub refresh_hidden_ms: u64,
    pub close_to_tray: bool,
    pub start_minimized: bool,
    pub cpu_panel_open: bool,
    pub policies: Vec<Policy>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            refresh_visible_ms: 1000,
            refresh_hidden_ms: 2000,
            close_to_tray: true,
            start_minimized: false,
            cpu_panel_open: true,
            policies: Vec::new(),
        }
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(config) if config.version <= CONFIG_VERSION => config,
            Ok(config) => {
                log::warn!("unsupported config version {}", config.version);
                Self::default()
            }
            Err(error) => {
                log::warn!("failed to parse config {}: {error}", path.display());
                let backup = path.with_extension("json.bad");
                let _ = std::fs::rename(path, backup);
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self)?;
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        if let Err(error) = std::fs::rename(&temp, path) {
            let _ = std::fs::remove_file(path);
            std::fs::rename(&temp, path).map_err(|second| {
                anyhow::anyhow!(
                    "rename {} -> {} failed: {error}; retry: {second}",
                    temp.display(),
                    path.display()
                )
            })?;
        }
        Ok(())
    }
}
