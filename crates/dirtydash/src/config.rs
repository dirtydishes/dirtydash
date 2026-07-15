use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub source_roots: Vec<SourceRoot>,
    #[serde(default)]
    pub remotes: Vec<RemoteConfig>,
    /// Optional Hub runtime settings. Local loopback mode remains the default.
    #[serde(default)]
    pub hub: HubConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HubConfig {
    #[serde(default)]
    pub tailscale_owner_mappings: Vec<TailscaleOwnerMappingConfig>,
    #[serde(default)]
    pub trusted_proxy: Option<TrustedProxyConfig>,
    #[serde(default)]
    pub cookie_transport: CookieTransportConfig,
    /// One-time setup secret for bootstrap when the local setup route is not available.
    #[serde(default)]
    pub bootstrap_setup_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TailscaleOwnerMappingConfig {
    #[serde(alias = "owner")]
    pub owner_username: String,
    #[serde(alias = "tailscale_user", alias = "tailscale_login")]
    pub tailscale_identity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedProxyConfig {
    pub identity_header: String,
    pub provenance_header: String,
    pub provenance_value: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CookieTransportConfig {
    #[default]
    Secure,
    LoopbackHttp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRoot {
    pub kind: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteConfig {
    pub name: String,
    pub ssh_target: String,
    #[serde(default)]
    pub source_roots: Vec<SourceRoot>,
}

fn default_version() -> u32 {
    1
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                version: 1,
                ..Self::default()
            });
        }

        let raw = fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let config =
            toml::from_str(&raw).with_context(|| format!("parsing config {}", path.display()))?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating config directory {}", parent.display()))?;
        }
        let raw = toml::to_string_pretty(self).context("serializing config")?;
        fs::write(path, raw).with_context(|| format!("writing config {}", path.display()))?;
        Ok(())
    }

    pub fn merge_cli_source_roots(&mut self, roots: &[String]) -> Result<()> {
        for root in roots {
            let (kind, path) = root
                .split_once('=')
                .with_context(|| format!("source root must use kind=path syntax: {root}"))?;
            self.source_roots.push(SourceRoot {
                kind: kind.trim().to_string(),
                path: PathBuf::from(path.trim()),
            });
        }
        Ok(())
    }
}
