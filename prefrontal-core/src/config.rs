use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use prefrontal_protocol::Activity;
use serde::{Deserialize, Serialize};

/// Central config (charter D5): one file the dashboard owns, projects stay unpolluted.
/// Every field defaults so a missing file means a fully working zero-config setup (D8).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Directories whose immediate subdirectories are treated as projects.
    pub roots: Vec<String>,
    /// Directory names skipped during scanning.
    pub ignore: Vec<String>,
    pub thresholds: Thresholds,
    pub timeline: TimelineConfig,
    pub server: ServerConfig,
    pub features: Features,
    /// Keyed by project directory name.
    pub overrides: HashMap<String, ProjectOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Thresholds {
    pub active_days: u32,
    pub warm_days: u32,
    pub cold_days: u32,
    pub dirty_pile: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TimelineConfig {
    /// How far back "where was I" looks.
    pub days: u32,
    /// Cap per project so one rebase-heavy repo can't flood the view.
    pub max_per_project: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
    pub ui_dir: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Features {
    /// CerebroCortex-RS semantic layer (charter D6): off by default, phase 6.
    pub cerebro: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectOverride {
    /// Pin an activity state regardless of the derived one (e.g. `parked`, `archived`).
    pub status: Option<Activity>,
    pub tags: Vec<String>,
    pub tagline: Option<String>,
    /// Hide from the dashboard entirely.
    pub ignore: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roots: vec!["~/Projects".into()],
            ignore: vec![".vite".into(), "node_modules".into(), "target".into()],
            thresholds: Thresholds::default(),
            timeline: TimelineConfig::default(),
            server: ServerConfig::default(),
            features: Features::default(),
            overrides: HashMap::new(),
        }
    }
}

impl Default for Thresholds {
    fn default() -> Self {
        Self { active_days: 7, warm_days: 30, cold_days: 180, dirty_pile: 10 }
    }
}

impl Default for TimelineConfig {
    fn default() -> Self {
        Self { days: 14, max_per_project: 30 }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        // 7320 = "PFC" on a phone keypad.
        Self { bind: "127.0.0.1:7320".into(), ui_dir: "ui-web".into() }
    }
}

impl Config {
    /// `~/.config/prefrontal/config.toml`, or defaults if it doesn't exist.
    pub fn load() -> Result<Self> {
        match Self::path() {
            Some(p) if p.exists() => {
                let raw = std::fs::read_to_string(&p)
                    .with_context(|| format!("reading {}", p.display()))?;
                toml::from_str(&raw).with_context(|| format!("parsing {}", p.display()))
            }
            _ => Ok(Self::default()),
        }
    }

    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("prefrontal").join("config.toml"))
    }

    /// Roots with `~` expanded, keeping only ones that exist.
    pub fn root_paths(&self) -> Vec<PathBuf> {
        self.roots
            .iter()
            .map(|r| expand_tilde(r))
            .filter(|p| p.is_dir())
            .collect()
    }
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    Path::new(path).to_path_buf()
}
