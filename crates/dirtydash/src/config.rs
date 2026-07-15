use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const SECRET_FILE_NAME: &str = "secrets.json";

use crate::listener::ListenerPlan;

#[derive(Clone, Default, Serialize, Deserialize)]
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
    /// Local outbound Collector settings. Collector state and credentials are
    /// persisted in the separate Collector SQLite database.
    #[serde(default)]
    pub collector: CollectorConfig,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct HubConfig {
    #[serde(default)]
    pub tailscale_owner_mappings: Vec<TailscaleOwnerMappingConfig>,
    #[serde(default)]
    pub trusted_proxy: Option<TrustedProxyConfig>,
    #[serde(default)]
    pub cookie_transport: CookieTransportConfig,
    /// Non-secret Hub listener policy. Tailscale Serve is the default.
    #[serde(default)]
    pub listener: ListenerPlan,
    /// Non-secret release publisher allowlist. Deployment requires both
    /// values and checks them against the supplied public key/manifest.
    #[serde(default)]
    pub allowed_publisher_key_id: Option<String>,
    #[serde(default)]
    pub allowed_publisher_fingerprint: Option<String>,
    /// One-time setup secret for bootstrap when the local setup route is not available.
    ///
    /// This value is loaded from [`SecretStore`].  It is deliberately skipped
    /// by TOML serialization so a normal config snapshot can never contain a
    /// bootstrap credential.
    #[serde(skip)]
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
    /// Direct peer IPs or CIDRs allowed to supply the trusted proxy headers.
    /// An empty policy fails closed.
    #[serde(default)]
    pub source_cidrs: Vec<String>,
}

impl HubConfig {
    pub fn validate(&self) -> Result<()> {
        self.listener.validate()?;
        if let Some(proxy) = &self.trusted_proxy {
            proxy.validate()?;
        }
        if self.allowed_publisher_key_id.is_some() != self.allowed_publisher_fingerprint.is_some() {
            bail!("publisher allowlist requires both key ID and fingerprint");
        }
        if self
            .allowed_publisher_key_id
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
            || self
                .allowed_publisher_fingerprint
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
        {
            bail!("publisher allowlist values cannot be empty");
        }
        Ok(())
    }
}

impl TrustedProxyConfig {
    pub fn validate(&self) -> Result<()> {
        if self.identity_header.trim().is_empty()
            || self.provenance_header.trim().is_empty()
            || self.provenance_value.trim().is_empty()
            || self.source_cidrs.is_empty()
        {
            bail!("trusted proxy configuration must be complete and fail closed");
        }
        for cidr in &self.source_cidrs {
            crate::ssh::validate_cidr(cidr)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CookieTransportConfig {
    #[default]
    Secure,
    LoopbackHttp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedUpdateConfig {
    pub version: String,
    pub sha256: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CollectorConfig {
    #[serde(default)]
    pub hub_url: Option<String>,
    #[serde(default)]
    pub machine_id: Option<String>,
    /// Loaded from [`SecretStore`], never serialized into config.toml.
    #[serde(skip)]
    pub credential_token: Option<String>,
    #[serde(default = "default_collector_reconcile_seconds")]
    pub reconcile_seconds: u64,
    #[serde(default = "default_collector_watcher_debounce_millis")]
    pub watcher_debounce_millis: u64,
    /// Only local, preconfigured artifact bytes may be considered by the
    /// restricted updater; there is intentionally no URL or command field.
    #[serde(default)]
    pub update_artifact_dir: Option<PathBuf>,
    #[serde(default)]
    pub update_target: Option<PathBuf>,
    #[serde(default)]
    pub approved_updates: Vec<ApprovedUpdateConfig>,
}

fn default_collector_reconcile_seconds() -> u64 {
    15 * 60
}

fn default_collector_watcher_debounce_millis() -> u64 {
    500
}

impl fmt::Debug for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("version", &self.version)
            .field("source_roots", &self.source_roots)
            .field("remotes", &self.remotes)
            .field("hub", &self.hub)
            .field("collector", &self.collector)
            .finish()
    }
}

impl fmt::Debug for HubConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HubConfig")
            .field("tailscale_owner_mappings", &self.tailscale_owner_mappings)
            .field("trusted_proxy", &self.trusted_proxy)
            .field("cookie_transport", &self.cookie_transport)
            .field("listener", &self.listener)
            .field("allowed_publisher_key_id", &self.allowed_publisher_key_id)
            .field(
                "allowed_publisher_fingerprint",
                &self.allowed_publisher_fingerprint,
            )
            .field(
                "bootstrap_setup_token",
                &self.bootstrap_setup_token.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl fmt::Debug for CollectorConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CollectorConfig")
            .field("hub_url", &self.hub_url)
            .field("machine_id", &self.machine_id)
            .field(
                "credential_token",
                &self.credential_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("reconcile_seconds", &self.reconcile_seconds)
            .field("watcher_debounce_millis", &self.watcher_debounce_millis)
            .field("update_artifact_dir", &self.update_artifact_dir)
            .field("update_target", &self.update_target)
            .field("approved_updates", &self.approved_updates)
            .finish()
    }
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            hub_url: None,
            machine_id: None,
            credential_token: None,
            reconcile_seconds: default_collector_reconcile_seconds(),
            watcher_debounce_millis: default_collector_watcher_debounce_millis(),
            update_artifact_dir: None,
            update_target: None,
            approved_updates: Vec::new(),
        }
    }
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

/// The only durable location for bootstrap and Collector bearer credentials.
/// The file is JSON for straightforward atomic snapshots, but it is never
/// included in the ordinary TOML config.  Values are loaded only into request
/// memory and are not included in `Debug`/TOML output by this module.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretSnapshot {
    pub bootstrap_setup_token: Option<String>,
    pub collector_credential_token: Option<String>,
}

#[derive(Clone)]
pub struct SecretStore {
    path: PathBuf,
}

impl SecretStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn for_config(config_path: &Path) -> Self {
        let path = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(SECRET_FILE_NAME);
        Self::new(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<SecretSnapshot> {
        if !self.path.exists() {
            return Ok(SecretSnapshot::default());
        }
        ensure_secret_permissions(&self.path)?;
        let bytes = fs::read(&self.path)
            .with_context(|| format!("reading secret store {}", self.path.display()))?;
        serde_json::from_slice(&bytes).context("parsing dirtydash secret store")
    }

    pub fn save(&self, snapshot: &SecretSnapshot) -> Result<()> {
        if snapshot.bootstrap_setup_token.is_none()
            && snapshot.collector_credential_token.is_none()
            && !self.path.exists()
        {
            return Ok(());
        }
        let parent = self
            .path
            .parent()
            .context("secret store path has no parent")?;
        fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(snapshot)?;
        atomic_write_config(&self.path, &bytes, 0o600)
    }
}

fn ensure_secret_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            bail!(
                "secret store {} is accessible by group or other users",
                path.display()
            );
        }
    }
    Ok(())
}

fn atomic_write_config(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().context("managed config path has no parent")?;
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dirtydash");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = parent.join(format!(".{name}.tmp-{}-{nonce}", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .with_context(|| format!("creating atomic config snapshot {}", temp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, path)?;
    Ok(())
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let mut config = if !path.exists() {
            Self {
                version: 1,
                ..Self::default()
            }
        } else {
            let raw = fs::read_to_string(path)
                .with_context(|| format!("reading config {}", path.display()))?;
            toml::from_str(&raw).with_context(|| format!("parsing config {}", path.display()))?
        };
        let secrets = SecretStore::for_config(path).load()?;
        config.hub.bootstrap_setup_token = secrets.bootstrap_setup_token;
        config.collector.credential_token = secrets.collector_credential_token;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating config directory {}", parent.display()))?;
        }
        let raw = toml::to_string_pretty(self).context("serializing config")?;
        atomic_write_config(path, raw.as_bytes(), 0o600)?;
        SecretStore::for_config(path).save(&SecretSnapshot {
            bootstrap_setup_token: self.hub.bootstrap_setup_token.clone(),
            collector_credential_token: self.collector.credential_token.clone(),
        })?;
        Ok(())
    }

    /// Validate security-sensitive configuration before it can be persisted or
    /// consumed by a listener.
    pub fn validate(&self) -> Result<()> {
        self.hub.listener.validate()?;
        if let Some(proxy) = &self.hub.trusted_proxy {
            proxy.validate()?;
        }
        self.hub.validate()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn secrets_are_atomic_restrictive_and_absent_from_toml_snapshots() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let mut config = Config::default();
        config.hub.bootstrap_setup_token = Some("BOOTSTRAP_SENTINEL".to_string());
        config.collector.credential_token = Some("COLLECTOR_SENTINEL".to_string());
        config.save(&config_path).unwrap();

        let toml = fs::read_to_string(&config_path).unwrap();
        assert!(!toml.contains("BOOTSTRAP_SENTINEL"));
        assert!(!toml.contains("COLLECTOR_SENTINEL"));
        let secret_path = SecretStore::for_config(&config_path).path().to_path_buf();
        let secret_bytes = fs::read_to_string(&secret_path).unwrap();
        assert!(secret_bytes.contains("BOOTSTRAP_SENTINEL"));
        assert!(secret_bytes.contains("COLLECTOR_SENTINEL"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&secret_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let loaded = Config::load(&config_path).unwrap();
        assert_eq!(
            loaded.hub.bootstrap_setup_token.as_deref(),
            Some("BOOTSTRAP_SENTINEL")
        );
        assert_eq!(
            loaded.collector.credential_token.as_deref(),
            Some("COLLECTOR_SENTINEL")
        );
        assert!(!format!("{loaded:?}").contains("BOOTSTRAP_SENTINEL"));
        assert!(!format!("{loaded:?}").contains("COLLECTOR_SENTINEL"));
        assert!(!serde_json::to_string(&loaded)
            .unwrap()
            .contains("BOOTSTRAP_SENTINEL"));
        assert!(!serde_json::to_string(&loaded)
            .unwrap()
            .contains("COLLECTOR_SENTINEL"));
    }

    #[test]
    fn trusted_proxy_cidrs_are_rejected_at_config_validation_time() {
        let mut config = Config::default();
        config.hub.trusted_proxy = Some(TrustedProxyConfig {
            identity_header: "x-id".to_string(),
            provenance_header: "x-proof".to_string(),
            provenance_value: "ok".to_string(),
            source_cidrs: vec!["127.0.0.1/33".to_string()],
        });
        assert!(config.validate().is_err());
    }
}
