//! Outbound-only Collector runtime.
//!
//! The Collector owns a separate local SQLite database containing only
//! reconciliation state, redacted request bytes, credentials, and diagnostics.
//! Parser reads happen against the local usage database, while the transport
//! seam never receives a local path or session body.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{CollectorConfig as FileCollectorConfig, Config, SourceRoot};
use crate::db::{
    CollectorEventManifestRecord, CollectorIdentityRecord, CollectorManifestRecord,
    CollectorOutboxRecord, Database,
};
use crate::hub::{
    CheckpointInput, CollectorCommandAckRequest, CollectorCommandResult,
    CollectorDiagnosticsReceipt, CollectorOutboxDiagnosticReceipt, CollectorUpdateReceipt,
    CollectorUpdateReceiptRequest, CollectorUsageEvent, CollectorWatcherReceipt,
    IngestBatchRequest, IngestBatchResponse, OwnerCommand, SourceManifestInput, SyncRunInput,
    API_V1_PROTOCOL_VERSION,
};
use crate::importers::{
    self, parse_sources_for_collector, stable_event_fingerprint, SourceKind, UsageEvent,
};

pub const DEFAULT_RECONCILIATION_INTERVAL: Duration = Duration::from_secs(15 * 60);
pub const DEFAULT_WATCHER_DEBOUNCE: Duration = Duration::from_millis(500);
pub const OWNER_COMMAND_LONG_POLL: Duration = Duration::from_secs(20);
pub const DEFAULT_OUTBOX_BATCH_LIMIT: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconciliationReason {
    Startup,
    Periodic,
    Manual,
    WatcherHint,
    WatcherFallback,
}

impl ReconciliationReason {
    fn forces_reparse(self) -> bool {
        matches!(self, Self::Manual)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(15 * 60),
        }
    }
}

impl RetryPolicy {
    pub fn delay_for(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        let exponent = attempt.saturating_sub(1).min(30);
        let multiplier = 1_u32.checked_shl(exponent).unwrap_or(u32::MAX);
        let seconds = self
            .base_delay
            .as_secs()
            .saturating_mul(u64::from(multiplier));
        Duration::from_secs(seconds.min(self.max_delay.as_secs()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryClass {
    Offline,
    Timeout,
    RateLimited,
    Server,
    Unauthorized,
    Protocol,
    Permanent,
}

impl RetryClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Timeout => "timeout",
            Self::RateLimited => "rate-limited",
            Self::Server => "server",
            Self::Unauthorized => "unauthorized",
            Self::Protocol => "protocol",
            Self::Permanent => "permanent",
        }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Offline | Self::Timeout | Self::RateLimited | Self::Server
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    pub class: RetryClass,
    pub message: String,
}

impl TransportError {
    pub fn new(class: RetryClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
        }
    }

    pub fn offline(message: impl Into<String>) -> Self {
        Self::new(RetryClass::Offline, message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(RetryClass::Timeout, message)
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(RetryClass::Protocol, message)
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.class, self.message)
    }
}

impl std::error::Error for TransportError {}

/// A narrow, typed seam for the real `/api/v1` transport. Implementations may
/// use HTTP, an in-process Hub router, or a deterministic test double. The
/// Collector never holds a SQLite connection while one of these methods is
/// waiting on the network.
pub trait CollectorTransport {
    fn send_batch(
        &mut self,
        credential_token: &str,
        request: &IngestBatchRequest,
    ) -> std::result::Result<IngestBatchResponse, TransportError>;

    fn poll_owner_command(
        &mut self,
        credential_token: &str,
        machine_id: &str,
        wait: Duration,
    ) -> std::result::Result<Option<OwnerCommand>, TransportError>;

    fn acknowledge_owner_command(
        &mut self,
        credential_token: &str,
        machine_id: &str,
        command_id: &str,
        result: &CommandOutcome,
    ) -> std::result::Result<(), TransportError> {
        let _ = (credential_token, machine_id, command_id, result);
        Ok(())
    }

    fn report_collector_update_receipt(
        &mut self,
        credential_token: &str,
        receipt: &CollectorUpdateReceipt,
    ) -> std::result::Result<(), TransportError> {
        let _ = (credential_token, receipt);
        Err(TransportError::protocol(
            "Collector update receipts are unsupported by this transport",
        ))
    }

    /// Activate one locally generated replacement. Implementations must send
    /// the secret only in this request and must not persist or echo it.
    fn activate_collector_credential_rotation(
        &mut self,
        credential_token: &str,
        machine_id: &str,
        rotation_id: &str,
        replacement_secret: &str,
    ) -> std::result::Result<(), TransportError> {
        let _ = (
            credential_token,
            machine_id,
            rotation_id,
            replacement_secret,
        );
        Err(TransportError::protocol(
            "credential rotation activation is unsupported by this transport",
        ))
    }

    /// Prove the replacement with the newly constructed bearer token. The Hub
    /// retires older credentials only after this authenticated proof.
    fn prove_collector_credential_rotation(
        &mut self,
        replacement_token: &str,
        machine_id: &str,
        rotation_id: &str,
    ) -> std::result::Result<(), TransportError> {
        let _ = (replacement_token, machine_id, rotation_id);
        Err(TransportError::protocol(
            "credential rotation proof is unsupported by this transport",
        ))
    }
}

/// Production outbound-only HTTP transport. TLS is provided by reqwest's
/// rustls backend; no listener or inbound socket is created by this type.
pub struct CollectorHttpTransport {
    base_url: String,
    client: reqwest::blocking::Client,
}

impl CollectorHttpTransport {
    pub fn new(hub_url: &str) -> Result<Self> {
        let parsed = reqwest::Url::parse(hub_url).context("parsing collector hub_url")?;
        if !matches!(parsed.scheme(), "https" | "http") {
            anyhow::bail!("collector hub_url must use https or explicit http loopback transport");
        }
        if parsed.scheme() == "http"
            && !parsed
                .host_str()
                .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "::1"))
        {
            anyhow::bail!("collector hub_url must use TLS unless it targets loopback");
        }
        let client = reqwest::blocking::Client::builder()
            .use_rustls_tls()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(35))
            .build()
            .context("building Collector TLS HTTP client")?;
        Ok(Self {
            base_url: hub_url.trim_end_matches('/').to_string(),
            client,
        })
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn classify_status(status: reqwest::StatusCode) -> RetryClass {
        match status.as_u16() {
            401 | 403 => RetryClass::Unauthorized,
            408 => RetryClass::Timeout,
            429 => RetryClass::RateLimited,
            500..=599 => RetryClass::Server,
            400..=499 => RetryClass::Permanent,
            _ => RetryClass::Protocol,
        }
    }

    fn transport_error(error: reqwest::Error) -> TransportError {
        let class = if error.is_timeout() {
            RetryClass::Timeout
        } else if error.is_connect() {
            RetryClass::Offline
        } else {
            RetryClass::Protocol
        };
        TransportError::new(
            class,
            format!("HTTP transport failed: {}", error.is_timeout()),
        )
    }

    fn response_error(response: &reqwest::blocking::Response) -> TransportError {
        TransportError::new(
            Self::classify_status(response.status()),
            format!("Hub HTTP status {}", response.status().as_u16()),
        )
    }
}

impl CollectorTransport for CollectorHttpTransport {
    fn send_batch(
        &mut self,
        credential_token: &str,
        request: &IngestBatchRequest,
    ) -> std::result::Result<IngestBatchResponse, TransportError> {
        let response = self
            .client
            .post(self.endpoint("/api/v1/ingest/batches"))
            .bearer_auth(credential_token)
            .json(request)
            .send()
            .map_err(Self::transport_error)?;
        if !response.status().is_success() {
            return Err(Self::response_error(&response));
        }
        response
            .json::<IngestBatchResponse>()
            .map_err(|_| TransportError::protocol("Hub returned an invalid ingest response"))
    }

    fn poll_owner_command(
        &mut self,
        credential_token: &str,
        machine_id: &str,
        wait: Duration,
    ) -> std::result::Result<Option<OwnerCommand>, TransportError> {
        let wait_seconds = wait.as_secs().min(20);
        let response = self
            .client
            .get(self.endpoint("/api/v1/collector/commands"))
            .query(&[("wait_seconds", wait_seconds)])
            .bearer_auth(credential_token)
            .header("x-dirtydash-collector-machine", machine_id)
            .send()
            .map_err(Self::transport_error)?;
        if !response.status().is_success() {
            return Err(Self::response_error(&response));
        }
        response
            .json::<crate::hub::CollectorCommandPollResponse>()
            .map(|body| body.command)
            .map_err(|_| TransportError::protocol("Hub returned an invalid command poll response"))
    }

    fn acknowledge_owner_command(
        &mut self,
        credential_token: &str,
        _machine_id: &str,
        command_id: &str,
        result: &CommandOutcome,
    ) -> std::result::Result<(), TransportError> {
        result.validate().map_err(|_| {
            TransportError::protocol("Collector command result failed local validation")
        })?;
        let response = self
            .client
            .post(self.endpoint("/api/v1/collector/commands/ack"))
            .bearer_auth(credential_token)
            .json(&CollectorCommandAckRequest {
                command_id: command_id.to_string(),
                result: result.clone(),
            })
            .send()
            .map_err(Self::transport_error)?;
        if !response.status().is_success() {
            return Err(Self::response_error(&response));
        }
        Ok(())
    }

    fn report_collector_update_receipt(
        &mut self,
        credential_token: &str,
        receipt: &CollectorUpdateReceipt,
    ) -> std::result::Result<(), TransportError> {
        let response = self
            .client
            .post(self.endpoint("/api/v1/collector/updates/receipt"))
            .bearer_auth(credential_token)
            .json(&CollectorUpdateReceiptRequest {
                receipt: receipt.clone(),
            })
            .send()
            .map_err(Self::transport_error)?;
        if !response.status().is_success() {
            return Err(Self::response_error(&response));
        }
        Ok(())
    }

    fn activate_collector_credential_rotation(
        &mut self,
        credential_token: &str,
        machine_id: &str,
        rotation_id: &str,
        replacement_secret: &str,
    ) -> std::result::Result<(), TransportError> {
        let response = self
            .client
            .post(self.endpoint("/api/v1/collector/credentials/rotation/activate"))
            .bearer_auth(credential_token)
            .json(&crate::hub::CollectorCredentialRotationActivationRequest {
                machine_id: machine_id.to_string(),
                rotation_id: rotation_id.to_string(),
                replacement_secret: replacement_secret.to_string(),
            })
            .send()
            .map_err(Self::transport_error)?;
        if !response.status().is_success() {
            return Err(Self::response_error(&response));
        }
        response
            .json::<crate::hub::CollectorCredentialRotationResponse>()
            .map(|_| ())
            .map_err(|_| TransportError::protocol("Hub returned an invalid rotation activation"))
    }

    fn prove_collector_credential_rotation(
        &mut self,
        replacement_token: &str,
        machine_id: &str,
        rotation_id: &str,
    ) -> std::result::Result<(), TransportError> {
        let response = self
            .client
            .post(self.endpoint("/api/v1/collector/credentials/rotation/prove"))
            .bearer_auth(replacement_token)
            .json(&crate::hub::CollectorCredentialRotationProofRequest {
                machine_id: machine_id.to_string(),
                rotation_id: rotation_id.to_string(),
            })
            .send()
            .map_err(Self::transport_error)?;
        if !response.status().is_success() {
            return Err(Self::response_error(&response));
        }
        response
            .json::<crate::hub::CollectorCredentialRotationResponse>()
            .map(|_| ())
            .map_err(|_| TransportError::protocol("Hub returned an invalid rotation proof"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovedUpdate {
    pub version: String,
    pub sha256: String,
}

/// Restricted update seam. Implementations receive already-approved bytes,
/// never a command or URL. The stock implementation atomically replaces one
/// configured target file after verifying its digest.
pub trait RestrictedUpdater: Send + Sync {
    fn apply(&self, version: &str, expected_sha256: &str, artifact: &[u8]) -> Result<()>;

    fn rollback(&self, _version: &str, _expected_sha256: &str) -> Result<()> {
        anyhow::bail!("the configured Collector updater does not support rollback")
    }
}

#[derive(Debug, Clone)]
pub struct AtomicFileUpdater {
    target: PathBuf,
}

impl AtomicFileUpdater {
    pub fn new(target: impl Into<PathBuf>) -> Self {
        Self {
            target: target.into(),
        }
    }
}

impl RestrictedUpdater for AtomicFileUpdater {
    fn apply(&self, version: &str, expected_sha256: &str, artifact: &[u8]) -> Result<()> {
        if version.is_empty()
            || !version.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
            })
        {
            anyhow::bail!("update version contains unsupported path characters");
        }
        let expected = expected_sha256.trim().to_ascii_lowercase();
        if expected.len() != 64
            || !expected
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            anyhow::bail!("update digest is not a SHA-256 hex digest");
        }
        let actual = hex::encode(Sha256::digest(artifact));
        if actual != expected {
            anyhow::bail!("update artifact digest does not match the approved digest");
        }
        let parent = self
            .target
            .parent()
            .context("configured update target has no parent directory")?;
        fs::create_dir_all(parent)?;
        let backup = self.target.with_extension("previous");
        if self.target.exists() {
            fs::copy(&self.target, &backup).with_context(|| {
                format!(
                    "snapshotting the current Collector release at {}",
                    self.target.display()
                )
            })?;
        }
        let temp = parent.join(format!(
            ".{}.{}.tmp",
            self.target
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("dirtydash-update"),
            random_hex(8)
        ));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(artifact)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &self.target).with_context(|| {
            format!(
                "atomically applying approved update {} to {}",
                version,
                self.target.display()
            )
        })?;
        Ok(())
    }

    fn rollback(&self, version: &str, expected_sha256: &str) -> Result<()> {
        if !is_safe_update_version(version) || !is_sha256_hex(expected_sha256) {
            anyhow::bail!("rollback evidence is invalid");
        }
        let backup = self.target.with_extension("previous");
        if !backup.exists() {
            anyhow::bail!("Collector rollback snapshot is unavailable");
        }
        fs::rename(&backup, &self.target)
            .with_context(|| format!("restoring the Collector rollback snapshot for {version}"))?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct CollectorOptions {
    pub source_roots: Vec<SourceRoot>,
    pub machine_id: Option<String>,
    pub credential_token: Option<String>,
    pub reconcile_interval: Duration,
    pub watcher_debounce: Duration,
    pub retry_policy: RetryPolicy,
    pub approved_updates: Vec<ApprovedUpdate>,
    pub update_artifact_dir: Option<PathBuf>,
    pub update_target: Option<PathBuf>,
}

impl Default for CollectorOptions {
    fn default() -> Self {
        Self {
            source_roots: Vec::new(),
            machine_id: None,
            credential_token: None,
            reconcile_interval: DEFAULT_RECONCILIATION_INTERVAL,
            watcher_debounce: DEFAULT_WATCHER_DEBOUNCE,
            retry_policy: RetryPolicy::default(),
            approved_updates: Vec::new(),
            update_artifact_dir: None,
            update_target: None,
        }
    }
}

impl CollectorOptions {
    pub fn from_config(config: &Config) -> Self {
        let file: &FileCollectorConfig = &config.collector;
        Self {
            source_roots: config.source_roots.clone(),
            machine_id: file.machine_id.clone(),
            credential_token: file.credential_token.clone(),
            reconcile_interval: Duration::from_secs(file.reconcile_seconds.max(1)),
            watcher_debounce: Duration::from_millis(file.watcher_debounce_millis),
            approved_updates: file
                .approved_updates
                .iter()
                .map(|entry| ApprovedUpdate {
                    version: entry.version.clone(),
                    sha256: entry.sha256.clone(),
                })
                .collect(),
            update_artifact_dir: file.update_artifact_dir.clone(),
            update_target: file.update_target.clone(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconciliationReport {
    pub reason: String,
    pub sync_run_id: String,
    pub batch_id: Option<String>,
    pub source_count: usize,
    pub files_seen: usize,
    pub files_reprocessed: usize,
    pub events_queued: usize,
    pub manifests_committed: usize,
    pub parse_errors: usize,
    pub watcher_degraded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryReport {
    pub attempted: usize,
    pub acknowledged: usize,
    pub failed: usize,
    pub pending: u64,
    pub terminal: u64,
    pub next_retry_at: Option<String>,
    pub errors: Vec<String>,
}

pub type CollectorDiagnostics = CollectorDiagnosticsReceipt;
pub type CommandOutcome = CollectorCommandResult;
pub type WatcherStatus = CollectorWatcherReceipt;
pub type OutboxDiagnostic = CollectorOutboxDiagnosticReceipt;

#[derive(Clone)]
pub struct Collector {
    usage_db: Database,
    store: Database,
    options: CollectorOptions,
    identity: CollectorIdentityRecord,
    command_owner_id: String,
    updater: Arc<dyn RestrictedUpdater>,
    next_reconciliation_at: DateTime<Utc>,
    hint_pending: bool,
    debounce_until: Option<DateTime<Utc>>,
    watcher_degraded: bool,
    watcher_error: Option<String>,
}

impl Collector {
    pub fn open(paths: &crate::app_paths::AppPaths, config: &Config) -> Result<Self> {
        Self::from_config(
            Database::open(&paths.db_path)?,
            Database::open(&paths.collector_db_path)?,
            config,
        )
    }

    /// Backwards-friendly constructor for callers that already have a local
    /// database. Production callers should use [`Self::with_databases`] so the
    /// Collector state is isolated in `AppPaths::collector_db_path`.
    pub fn new(usage_db: Database, options: CollectorOptions) -> Result<Self> {
        Self::with_databases(usage_db.clone(), usage_db, options)
    }

    pub fn with_databases(
        usage_db: Database,
        store: Database,
        options: CollectorOptions,
    ) -> Result<Self> {
        usage_db.migrate()?;
        store.migrate_collector()?;
        crate::pricing::seed_bundled_pricing(&usage_db)?;

        let identity = if let Some(existing) = store.collector_identity()? {
            if existing.credential_token.is_none() {
                if let Some(token) = options.credential_token.as_deref() {
                    store.set_collector_credential(Some(token))?;
                    store
                        .collector_identity()?
                        .context("Collector identity disappeared")?
                } else {
                    existing
                }
            } else {
                existing
            }
        } else {
            let project_salt = random_hex(32);
            let machine_id = options
                .machine_id
                .as_deref()
                .map(|value| safe_identifier(value, "machine-config"))
                .unwrap_or_else(|| {
                    let host_hash = hex::encode(Sha256::digest(local_machine_bytes()));
                    format!("machine-{}", &host_hash[..24])
                });
            let identity = CollectorIdentityRecord {
                machine_id,
                project_salt,
                credential_token: options.credential_token.clone(),
                pending_credential_token: None,
                pending_credential_id: None,
            };
            store.save_collector_identity(&identity)?;
            identity
        };

        let now = Utc::now();
        let watcher_degraded = store
            .collector_state("watcher_degraded")?
            .is_some_and(|value| value == "true");
        let watcher_error = store.collector_state("watcher_error")?;
        let next_reconciliation_at = store
            .collector_state("next_reconciliation_at")?
            .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or(now);
        let updater_target = options
            .update_target
            .clone()
            .unwrap_or_else(|| store.path().with_extension("update-artifact"));

        Ok(Self {
            usage_db,
            store,
            options,
            identity,
            command_owner_id: format!("command-owner-{}", random_hex(12)),
            updater: Arc::new(AtomicFileUpdater::new(updater_target)),
            next_reconciliation_at,
            hint_pending: false,
            debounce_until: None,
            watcher_degraded,
            watcher_error,
        })
    }

    pub fn from_config(
        usage_db: Database,
        collector_db: Database,
        config: &Config,
    ) -> Result<Self> {
        Self::with_databases(
            usage_db,
            collector_db,
            CollectorOptions::from_config(config),
        )
    }

    pub fn with_updater(mut self, updater: Arc<dyn RestrictedUpdater>) -> Self {
        self.updater = updater;
        self
    }

    pub fn machine_id(&self) -> &str {
        &self.identity.machine_id
    }

    /// Record the process generation that owns the current Collector runtime.
    /// Update receipts are emitted only after a subsequent generation starts,
    /// so an owner cannot turn an artifact write into restart proof.
    pub fn mark_runtime_started(&self, now: DateTime<Utc>) -> Result<()> {
        self.store
            .set_collector_state("runtime_generation", &format!("runtime-{}", random_hex(16)))?;
        self.store
            .set_collector_state("runtime_started_at", &now.to_rfc3339())?;
        self.store
            .set_collector_state("restart_requested", "false")?;
        Ok(())
    }

    pub fn report_pending_update_receipt<T: CollectorTransport>(
        &mut self,
        transport: &mut T,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let Some(update_id) = self.store.collector_state("pending_update_id")? else {
            return Ok(false);
        };
        if update_id.is_empty() {
            return Ok(false);
        }
        let command_id = self
            .store
            .collector_state("pending_update_command_id")?
            .context("pending Collector update command ID is missing")?;
        let version = self
            .store
            .collector_state("pending_update_version")?
            .context("pending Collector update version is missing")?;
        let sha256 = self
            .store
            .collector_state("pending_update_sha256")?
            .context("pending Collector update digest is missing")?;
        let runtime_generation = self
            .store
            .collector_state("runtime_generation")?
            .context("Collector runtime generation is missing")?;
        let restarted_at = self
            .store
            .collector_state("runtime_started_at")?
            .context("Collector runtime start time is missing")?;
        let diagnostics = self.diagnostics()?;
        if diagnostics.watcher.degraded || diagnostics.terminal_outbox > 0 {
            return Err(anyhow::anyhow!("Collector health proof is not ready"));
        }
        let receipt = CollectorUpdateReceipt {
            update_id,
            command_id,
            version,
            sha256,
            collector_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: API_V1_PROTOCOL_VERSION,
            runtime_generation,
            restarted_at,
            health_checked_at: now.to_rfc3339(),
        };
        let candidates = self.credential_candidates()?;
        let mut last_error = None;
        for (credential, pending) in candidates {
            match transport.report_collector_update_receipt(&credential, &receipt) {
                Ok(()) => {
                    self.store.set_collector_state("pending_update_id", "")?;
                    self.store
                        .set_collector_state("restart_requested", "false")?;
                    return Ok(true);
                }
                Err(error) if error.class == RetryClass::Unauthorized && pending => {
                    last_error = Some(error);
                }
                Err(error) => return Err(anyhow::anyhow!(error)),
            }
        }
        Err(anyhow::anyhow!(last_error.unwrap_or_else(|| {
            TransportError::new(
                RetryClass::Unauthorized,
                "all Collector credentials rejected",
            )
        })))
    }

    pub fn project_salt(&self) -> &str {
        &self.identity.project_salt
    }

    pub fn watcher_status(&self) -> WatcherStatus {
        WatcherStatus {
            enabled: true,
            degraded: self.watcher_degraded,
            last_error: self.watcher_error.clone(),
            hint_pending: self.hint_pending,
            debounce_until: self.debounce_until.map(|value| value.to_rfc3339()),
            next_reconciliation_at: self.next_reconciliation_at.to_rfc3339(),
        }
    }

    pub fn diagnostics(&self) -> Result<CollectorDiagnostics> {
        let identity = self
            .store
            .collector_identity()?
            .unwrap_or_else(|| self.identity.clone());
        Ok(CollectorDiagnostics {
            machine_id: self.identity.machine_id.clone(),
            parser_versions: SourceKind::all()
                .into_iter()
                .map(|source| source.parser_version().to_string())
                .collect(),
            pending_outbox: self.store.collector_outbox_count()?,
            last_reconciliation_at: self.store.collector_state("last_reconciliation_at")?,
            watcher: self.watcher_status(),
            credential_configured: identity.credential_token.is_some()
                || identity.pending_credential_token.is_some(),
            credential_rotation_pending: identity.pending_credential_token.is_some(),
            terminal_outbox: self.store.collector_outbox_dead_letter_count()?,
            outbox_diagnostics: self
                .store
                .collector_outbox_records(true)?
                .into_iter()
                .filter(|record| record.status == "dead-letter")
                .map(|record| OutboxDiagnostic {
                    batch_id: record.batch_id,
                    status: record.status,
                    retry_class: record.retry_class,
                    attempts: record.attempts,
                    next_attempt_at: record.next_attempt_at,
                    last_error: record.last_error,
                })
                .collect(),
        })
    }

    /// Record a watcher hint only. Multiple filesystem notifications coalesce
    /// into one delayed reconciliation; the periodic reconciler remains the
    /// correctness path.
    pub fn notify_watcher_hint(&mut self, now: DateTime<Utc>) {
        self.hint_pending = true;
        self.debounce_until = Some(now + chrono_from_std(self.options.watcher_debounce));
    }

    pub fn on_watcher_hint(&mut self, now: DateTime<Utc>) {
        self.notify_watcher_hint(now);
    }

    /// A watcher failure is visible and immediately makes the complete
    /// reconciler due. No inbound watcher service is started by this module.
    pub fn watcher_failed(&mut self, now: DateTime<Utc>, error: impl Into<String>) {
        self.watcher_degraded = true;
        self.watcher_error = Some(safe_diagnostic(error.into()));
        self.hint_pending = true;
        self.debounce_until = Some(now);
        self.next_reconciliation_at = now;
        let _ = self.store.set_collector_state("watcher_degraded", "true");
        let _ = self.store.set_collector_state(
            "watcher_error",
            self.watcher_error.as_deref().unwrap_or("watcher-failed"),
        );
        let _ = self
            .store
            .set_collector_state("next_reconciliation_at", &now.to_rfc3339());
    }

    pub fn clear_watcher_failure(&mut self) -> Result<()> {
        self.watcher_degraded = false;
        self.watcher_error = None;
        self.store
            .set_collector_state("watcher_degraded", "false")?;
        self.store.set_collector_state("watcher_error", "")?;
        Ok(())
    }

    pub fn reconciliation_due(&self, now: DateTime<Utc>) -> bool {
        (self.hint_pending && self.debounce_until.is_some_and(|until| now >= until))
            || now >= self.next_reconciliation_at
    }

    pub fn reconcile_if_due(&mut self, now: DateTime<Utc>) -> Result<Option<ReconciliationReport>> {
        if !self.reconciliation_due(now) {
            return Ok(None);
        }
        let reason = if self.watcher_degraded {
            ReconciliationReason::WatcherFallback
        } else if self.hint_pending {
            ReconciliationReason::WatcherHint
        } else {
            ReconciliationReason::Periodic
        };
        self.hint_pending = false;
        self.debounce_until = None;
        self.reconcile(reason, now).map(Some)
    }

    pub fn reconcile_startup(&mut self, now: DateTime<Utc>) -> Result<ReconciliationReport> {
        self.reconcile(ReconciliationReason::Startup, now)
    }

    pub fn reconcile_manual(&mut self, now: DateTime<Utc>) -> Result<ReconciliationReport> {
        self.reconcile(ReconciliationReason::Manual, now)
    }

    pub fn reconcile(
        &mut self,
        reason: ReconciliationReason,
        now: DateTime<Utc>,
    ) -> Result<ReconciliationReport> {
        let imported_at = now.to_rfc3339();
        let source_config = Config {
            source_roots: self.options.source_roots.clone(),
            ..Config::default()
        };
        let sources = if self.options.source_roots.is_empty() {
            importers::scan_sources(&source_config)?
        } else {
            importers::scan_configured_sources(&source_config)?
        };
        let parsed = parse_sources_for_collector(
            &self.usage_db,
            &sources,
            &self.identity.machine_id,
            &imported_at,
        )?;

        let mut manifests = Vec::new();
        let mut payload_manifests = Vec::new();
        let mut payload_events = Vec::new();
        let mut seen_event_ids = BTreeSet::new();
        let pending_event_fingerprints = self.pending_collector_event_fingerprints()?;
        let mut seen_source_keys = BTreeSet::new();
        let mut files_reprocessed = 0;
        let mut parse_errors = 0;
        let mut current_agents = BTreeSet::new();

        for parsed_file in &parsed {
            let source_key = redacted_identifier(
                &self.identity.project_salt,
                "source",
                &format!(
                    "{}|{}",
                    parsed_file.source.kind.as_str(),
                    parsed_file.file.display()
                ),
            );
            seen_source_keys.insert(source_key.clone());
            let previous = self.store.collector_manifest(&source_key)?;
            let parser_version = parsed_file.source.kind.parser_version();
            let should_reprocess = reason.forces_reparse()
                || previous.as_ref().is_none_or(|manifest| {
                    manifest.file_fingerprint != parsed_file.file_fingerprint
                        || manifest.parser_version != parser_version
                });
            if parsed_file.parse_error.is_some() {
                parse_errors += 1;
            }
            current_agents.insert(parsed_file.source.kind.as_str().to_string());

            let item_count = parsed_file.events.len() as u64;
            let cursor = Some(redacted_identifier(
                &self.identity.project_salt,
                "cursor",
                &parsed_file.file_fingerprint,
            ));
            let manifest = CollectorManifestRecord {
                source_key: source_key.clone(),
                agent: parsed_file.source.kind.as_str().to_string(),
                local_path: parsed_file.file.display().to_string(),
                file_fingerprint: parsed_file.file_fingerprint.clone(),
                parser_version: parser_version.to_string(),
                item_count,
                cursor: cursor.clone(),
                parse_error: parsed_file.parse_error.clone(),
                last_reconciled_at: imported_at.clone(),
            };
            let changed = manifest_changed(previous.as_ref(), &manifest);
            manifests.push(manifest);
            if changed {
                payload_manifests.push(SourceManifestInput {
                    source_key: source_key.clone(),
                    agent: parsed_file.source.kind.as_str().to_string(),
                    display_path: format!(
                        "{}/{}",
                        parsed_file.source.kind.as_str(),
                        redacted_identifier(
                            &self.identity.project_salt,
                            "display-path",
                            &parsed_file.file.display().to_string(),
                        )
                    ),
                    item_count,
                    cursor,
                    manifest_fingerprint: parsed_file.file_fingerprint.clone(),
                });
            }

            if !should_reprocess {
                continue;
            }
            files_reprocessed += 1;
            for event in &parsed_file.events {
                let _write = self.usage_db.upsert_usage_event(event)?;
                let collector_event_fingerprint = canonical_collector_event_fingerprint(event);
                let event_identity =
                    canonical_event_identity(event.source.as_str(), &collector_event_fingerprint);
                let redacted = self.redact_event(event, &source_key, now);
                let canonical_fingerprint = canonical_event_fingerprint(&redacted)?;
                let existing = self.store.collector_event_manifest(&event_identity)?;
                let pending_fingerprint = pending_event_fingerprints.get(&event_identity);
                let already_emitted = existing.as_ref().is_some_and(|record| {
                    record.canonical_fingerprint == canonical_fingerprint
                        && matches!(record.status.as_str(), "emitted" | "delivered")
                });
                let already_pending = pending_fingerprint == Some(&canonical_fingerprint);
                // A forced Refresh still reparses the source, but a canonical
                // event is emitted only when it is new or its redacted wire
                // representation changed. The pending fallback closes the
                // crash window before the durable event-manifest row exists.
                if !already_emitted && !already_pending && seen_event_ids.insert(event_identity) {
                    payload_events.push(redacted);
                }
            }
        }

        // A complete reconciliation also publishes tombstones for files that
        // disappeared since the last scan. The local path remains local-only;
        // the Hub sees the same redacted source key with item_count=0.
        for stale in self.store.collector_manifests()? {
            if seen_source_keys.contains(&stale.source_key) {
                continue;
            }
            current_agents.insert(stale.agent.clone());
            let manifest = CollectorManifestRecord {
                source_key: stale.source_key.clone(),
                agent: stale.agent.clone(),
                local_path: stale.local_path.clone(),
                file_fingerprint: "missing".to_string(),
                parser_version: stale.parser_version.clone(),
                item_count: 0,
                cursor: None,
                parse_error: Some("source missing".to_string()),
                last_reconciled_at: imported_at.clone(),
            };
            let changed = manifest_changed(Some(&stale), &manifest);
            manifests.push(manifest);
            if changed {
                payload_manifests.push(SourceManifestInput {
                    source_key: stale.source_key.clone(),
                    agent: stale.agent,
                    display_path: format!(
                        "source/{}",
                        redacted_identifier(
                            &self.identity.project_salt,
                            "missing-source",
                            &stale.local_path,
                        )
                    ),
                    item_count: 0,
                    cursor: None,
                    manifest_fingerprint: redacted_identifier(
                        &self.identity.project_salt,
                        "missing-manifest",
                        &stale.source_key,
                    ),
                });
            }
        }

        if !payload_manifests.is_empty() || !payload_events.is_empty() {
            payload_manifests.sort_by(|left, right| left.source_key.cmp(&right.source_key));
            payload_events.sort_by(|left, right| {
                left.collector_event_fingerprint
                    .cmp(&right.collector_event_fingerprint)
            });
            let sync_run_id = format!(
                "sync-{}",
                redacted_identifier(
                    &self.identity.project_salt,
                    "sync",
                    &format!("{}|{}", reason_string(reason), now.to_rfc3339()),
                )
            );
            let checkpoints = build_checkpoints(&payload_manifests, &current_agents);
            let request_without_batch = IngestBatchRequest {
                protocol_version: API_V1_PROTOCOL_VERSION,
                batch_id: "pending".to_string(),
                machine_id: self.identity.machine_id.clone(),
                sync_run: SyncRunInput {
                    sync_run_id: sync_run_id.clone(),
                    collector_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                    runtime_generation: self.store.collector_state("runtime_generation")?,
                    started_at: imported_at.clone(),
                    finished_at: imported_at.clone(),
                },
                source_manifests: payload_manifests,
                checkpoints,
                events: payload_events,
            };
            let canonical_without_batch = serde_json::to_vec(&request_without_batch)?;
            let batch_id = format!(
                "batch-{}",
                hex::encode(Sha256::digest(&canonical_without_batch))
            );
            let mut request = request_without_batch;
            request.batch_id = batch_id.clone();
            let events_queued = request.events.len();
            let event_manifests = request
                .events
                .iter()
                .map(|event| {
                    Ok(CollectorEventManifestRecord {
                        event_identity: canonical_event_identity(
                            &event.agent,
                            &event.collector_event_fingerprint,
                        ),
                        source_key: event.source_key.clone(),
                        canonical_fingerprint: canonical_event_fingerprint(event)?,
                        status: "emitted".to_string(),
                        emitted_at: imported_at.clone(),
                        delivered_at: None,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let payload_json = serde_json::to_string(&request)?;
            self.store.commit_collector_reconciliation(
                &manifests,
                &event_manifests,
                &batch_id,
                &self.identity.machine_id,
                &payload_json,
                &imported_at,
            )?;
            self.next_reconciliation_at = now + chrono_from_std(self.options.reconcile_interval);
            self.store
                .set_collector_state("last_reconciliation_at", &imported_at)?;
            self.store.set_collector_state(
                "next_reconciliation_at",
                &self.next_reconciliation_at.to_rfc3339(),
            )?;
            return Ok(ReconciliationReport {
                reason: reason_string(reason).to_string(),
                sync_run_id,
                batch_id: Some(batch_id),
                source_count: sources.len(),
                files_seen: parsed.len(),
                files_reprocessed,
                events_queued,
                manifests_committed: manifests.len(),
                parse_errors,
                watcher_degraded: self.watcher_degraded,
            });
        }

        self.next_reconciliation_at = now + chrono_from_std(self.options.reconcile_interval);
        self.store
            .set_collector_state("last_reconciliation_at", &imported_at)?;
        self.store.set_collector_state(
            "next_reconciliation_at",
            &self.next_reconciliation_at.to_rfc3339(),
        )?;
        Ok(ReconciliationReport {
            reason: reason_string(reason).to_string(),
            sync_run_id: format!("sync-empty-{}", now.timestamp()),
            batch_id: None,
            source_count: sources.len(),
            files_seen: parsed.len(),
            files_reprocessed,
            events_queued: 0,
            manifests_committed: 0,
            parse_errors,
            watcher_degraded: self.watcher_degraded,
        })
    }

    pub fn run_once<T: CollectorTransport>(
        &mut self,
        transport: &mut T,
        now: DateTime<Utc>,
    ) -> Result<(Option<ReconciliationReport>, DeliveryReport)> {
        let reconciliation = self.reconcile_if_due(now)?;
        let delivery = self.deliver_pending(transport, now)?;
        Ok((reconciliation, delivery))
    }

    fn pending_collector_event_fingerprints(&self) -> Result<BTreeMap<String, String>> {
        let mut event_fingerprints = BTreeMap::new();
        for record in self.store.collector_outbox_records(true)? {
            let Ok(request) = serde_json::from_str::<IngestBatchRequest>(&record.payload_json)
            else {
                continue;
            };
            for event in request.events {
                let event_identity =
                    canonical_event_identity(&event.agent, &event.collector_event_fingerprint);
                let fingerprint = canonical_event_fingerprint(&event)?;
                event_fingerprints.insert(event_identity, fingerprint);
            }
        }
        Ok(event_fingerprints)
    }

    fn credential_candidates(&self) -> Result<Vec<(String, bool)>> {
        let identity = self
            .store
            .collector_identity()?
            .context("Collector identity is missing")?;
        let mut candidates = Vec::new();
        if let Some(pending) = identity.pending_credential_token {
            candidates.push((pending, true));
        }
        if let Some(current) = identity.credential_token {
            if candidates
                .first()
                .is_none_or(|(pending, _)| pending != &current)
            {
                candidates.push((current, false));
            }
        }
        if candidates.is_empty() {
            anyhow::bail!("Collector credential is not configured");
        }
        Ok(candidates)
    }

    fn send_batch_with_fallback<T: CollectorTransport>(
        &self,
        transport: &mut T,
        request: &IngestBatchRequest,
    ) -> std::result::Result<(IngestBatchResponse, bool), TransportError> {
        let candidates = self
            .credential_candidates()
            .map_err(|error| TransportError::new(RetryClass::Permanent, error.to_string()))?;
        let mut last_error = None;
        for (credential, pending) in candidates {
            match transport.send_batch(&credential, request) {
                Ok(response) => return Ok((response, pending)),
                Err(error) if error.class == RetryClass::Unauthorized && pending => {
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            TransportError::new(
                RetryClass::Unauthorized,
                "all Collector credentials rejected",
            )
        }))
    }

    /// Explicit operator recovery for a terminal/dead-lettered batch. It is
    /// never part of automatic retry selection.
    pub fn recover_outbox_batch(&self, batch_id: &str, now: DateTime<Utc>) -> Result<bool> {
        self.store
            .recover_collector_batch(batch_id, &now.to_rfc3339())
    }

    pub fn deliver_pending<T: CollectorTransport>(
        &mut self,
        transport: &mut T,
        now: DateTime<Utc>,
    ) -> Result<DeliveryReport> {
        let records = self
            .store
            .collector_outbox_ready(&now.to_rfc3339(), DEFAULT_OUTBOX_BATCH_LIMIT)?;
        let mut report = DeliveryReport {
            attempted: 0,
            acknowledged: 0,
            failed: 0,
            pending: 0,
            terminal: 0,
            next_retry_at: None,
            errors: Vec::new(),
        };

        for record in records {
            report.attempted += 1;
            let request: IngestBatchRequest = match serde_json::from_str(&record.payload_json) {
                Ok(request) => request,
                Err(error) => {
                    self.record_delivery_failure(
                        &record,
                        now,
                        RetryClass::Permanent,
                        error.to_string(),
                    )?;
                    report.failed += 1;
                    report
                        .errors
                        .push("stored batch payload is invalid".to_string());
                    continue;
                }
            };
            let attempt = record.attempts.saturating_add(1);
            // No SQLite connection is held across this potentially long call.
            let sent = self.send_batch_with_fallback(transport, &request);
            match sent {
                Ok((response, _used_pending)) if response.batch_id == request.batch_id => {
                    let event_manifests = request
                        .events
                        .iter()
                        .map(|event| {
                            Ok(CollectorEventManifestRecord {
                                event_identity: canonical_event_identity(
                                    &event.agent,
                                    &event.collector_event_fingerprint,
                                ),
                                source_key: event.source_key.clone(),
                                canonical_fingerprint: canonical_event_fingerprint(event)?,
                                status: "emitted".to_string(),
                                emitted_at: request.sync_run.finished_at.clone(),
                                delivered_at: None,
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    if self
                        .store
                        .acknowledge_collector_batch_if_matching_with_events(
                            &request.batch_id,
                            &response.batch_id,
                            &event_manifests,
                            &now.to_rfc3339(),
                        )?
                    {
                        report.acknowledged += 1;
                    }
                }
                Ok((response, _)) => {
                    self.record_delivery_failure(
                        &record,
                        now,
                        RetryClass::Protocol,
                        format!(
                            "acknowledged batch {} instead of requested batch",
                            response.batch_id
                        ),
                    )?;
                    report.failed += 1;
                    report
                        .errors
                        .push("Hub acknowledged a different batch".to_string());
                }
                Err(error) => {
                    self.record_delivery_failure(
                        &record,
                        now,
                        error.class.clone(),
                        error.message.clone(),
                    )?;
                    report.failed += 1;
                    report.errors.push(safe_diagnostic(error.to_string()));
                    if error.class.is_retryable() {
                        let retry_at =
                            now + chrono_from_std(self.options.retry_policy.delay_for(attempt));
                        let retry_at = retry_at.to_rfc3339();
                        if report
                            .next_retry_at
                            .as_deref()
                            .is_none_or(|current| retry_at.as_str() < current)
                        {
                            report.next_retry_at = Some(retry_at);
                        }
                    }
                }
            }
        }
        report.pending = self.store.collector_outbox_count()?;
        report.terminal = self.store.collector_outbox_dead_letter_count()?;
        Ok(report)
    }

    fn poll_command_with_fallback<T: CollectorTransport>(
        &self,
        transport: &mut T,
    ) -> Result<(Option<OwnerCommand>, String, bool)> {
        let candidates = self.credential_candidates()?;
        let mut last_error = None;
        for (credential, pending) in candidates {
            match transport.poll_owner_command(
                &credential,
                &self.identity.machine_id,
                OWNER_COMMAND_LONG_POLL,
            ) {
                Ok(command) => return Ok((command, credential, pending)),
                Err(error) if error.class == RetryClass::Unauthorized && pending => {
                    last_error = Some(error);
                }
                Err(error) => return Err(anyhow::anyhow!(error)),
            }
        }
        Err(anyhow::anyhow!(last_error.unwrap_or_else(|| {
            TransportError::new(
                RetryClass::Unauthorized,
                "all Collector credentials rejected",
            )
        })))
    }

    fn acknowledge_command_with_fallback<T: CollectorTransport>(
        &self,
        transport: &mut T,
        credential: &str,
        used_pending: bool,
        command_id: &str,
        outcome: &CommandOutcome,
    ) -> Result<()> {
        match transport.acknowledge_owner_command(
            credential,
            &self.identity.machine_id,
            command_id,
            outcome,
        ) {
            Ok(()) => Ok(()),
            Err(error) if error.class == RetryClass::Unauthorized && used_pending => {
                let candidates = self.credential_candidates()?;
                let (current, _) = candidates
                    .into_iter()
                    .find(|(_, pending)| !*pending)
                    .context("current Collector credential is unavailable")?;
                transport
                    .acknowledge_owner_command(
                        &current,
                        &self.identity.machine_id,
                        command_id,
                        outcome,
                    )
                    .map_err(|error| anyhow::anyhow!(error))?;
                Ok(())
            }
            Err(error) => Err(anyhow::anyhow!(error)),
        }
    }

    pub fn poll_owner_command<T: CollectorTransport>(
        &mut self,
        transport: &mut T,
        now: DateTime<Utc>,
    ) -> Result<Option<CommandOutcome>> {
        let (command, credential, used_pending) = self.poll_command_with_fallback(transport)?;
        let Some(command) = command else {
            return Ok(None);
        };
        let command_id = command.command_id().to_string();
        let mut reclaimed = false;
        if let Some(receipt) = self.store.collector_command_result(&command_id)? {
            if receipt.status != "completed" {
                // A different process may have crashed after the started
                // receipt. The command-specific handlers below are replay
                // safe, so reclaim it instead of permanently rejecting it.
                reclaimed = self.store.reclaim_collector_command(
                    &command_id,
                    &now.to_rfc3339(),
                    &self.command_owner_id,
                )?;
            } else {
                let outcome: CommandOutcome = serde_json::from_str(&receipt.result_json)
                    .context("stored Collector command receipt is invalid")?;
                self.acknowledge_command_with_fallback(
                    transport,
                    &credential,
                    used_pending,
                    &command_id,
                    &outcome,
                )?;
                return Ok(Some(outcome));
            }
        }
        if !reclaimed
            && !self.store.begin_collector_command_owned(
                &command_id,
                &now.to_rfc3339(),
                &self.command_owner_id,
            )?
        {
            return Ok(Some(CommandOutcome::Rejected {
                reason: "command receipt is currently leased by another Collector instance"
                    .to_string(),
            }));
        }
        let (outcome, acknowledgement_credential, acknowledgement_used_pending) = match command {
            OwnerCommand::RotateCredential { rotation_id, .. } => {
                let (outcome, replacement_token) =
                    self.execute_credential_rotation(transport, &rotation_id)?;
                // The local commit happens immediately after Hub proof, so the
                // acknowledgement uses the replacement as the current token.
                (outcome, replacement_token, false)
            }
            command => (
                self.handle_owner_command(command, now)?,
                credential,
                used_pending,
            ),
        };
        let result_json = serde_json::to_string(&outcome)?;
        self.store
            .save_collector_command_result(&command_id, &result_json, &now.to_rfc3339())?;
        self.acknowledge_command_with_fallback(
            transport,
            &acknowledgement_credential,
            acknowledgement_used_pending,
            &command_id,
            &outcome,
        )?;
        Ok(Some(outcome))
    }

    pub fn apply_approved_update_artifact(
        &self,
        version: &str,
        expected_sha256: &str,
        artifact: &[u8],
    ) -> Result<()> {
        let digest = expected_sha256.trim().to_ascii_lowercase();
        let approved =
            self.options.approved_updates.iter().any(|entry| {
                entry.version == version && entry.sha256.eq_ignore_ascii_case(&digest)
            });
        if !approved || !is_sha256_hex(&digest) {
            anyhow::bail!("artifact is not approved by the configured version/digest allowlist");
        }
        self.updater.apply(version, &digest, artifact)?;
        self.store
            .set_collector_state("applied_update_version", version)?;
        self.store
            .set_collector_state("applied_update_sha256", &digest)?;
        Ok(())
    }

    fn rollback_approved_update(&self, version: &str, expected_sha256: &str) -> Result<()> {
        if !is_safe_update_version(version) || !is_sha256_hex(expected_sha256) {
            anyhow::bail!("rollback evidence is invalid");
        }
        self.updater.rollback(version, expected_sha256)?;
        self.store
            .set_collector_state("rollback_version", version)?;
        self.store
            .set_collector_state("rollback_sha256", expected_sha256)?;
        Ok(())
    }

    fn stage_local_credential_rotation(&self, rotation_id: &str) -> Result<String> {
        if !is_safe_rotation_id(rotation_id) {
            anyhow::bail!("credential rotation ID is invalid");
        }
        let identity = self
            .store
            .collector_identity()?
            .context("Collector identity is missing")?;
        if let Some(pending_id) = identity.pending_credential_id.as_deref() {
            if pending_id != rotation_id {
                anyhow::bail!("another credential rotation is already pending");
            }
            return identity
                .pending_credential_token
                .context("pending credential rotation is missing its local secret");
        }
        if identity.pending_credential_token.is_some() {
            anyhow::bail!("legacy pending credential rotation has no rotation ID");
        }
        if identity
            .credential_token
            .as_deref()
            .is_some_and(|token| token_credential_id(token) == Some(rotation_id))
        {
            return identity
                .credential_token
                .context("current credential disappeared during rotation");
        }
        let replacement_token = format!("ddcol_{rotation_id}.{}", random_hex(24));
        self.store
            .stage_collector_credential(rotation_id, &replacement_token)?;
        Ok(replacement_token)
    }

    fn rotation_activation_candidates(&self) -> Result<Vec<String>> {
        let identity = self
            .store
            .collector_identity()?
            .context("Collector identity is missing")?;
        let mut candidates = Vec::new();
        if let Some(current) = identity.credential_token {
            candidates.push(current);
        }
        if let Some(pending) = identity.pending_credential_token {
            if !candidates.iter().any(|candidate| candidate == &pending) {
                candidates.push(pending);
            }
        }
        if candidates.is_empty() {
            anyhow::bail!("Collector credential is not configured");
        }
        Ok(candidates)
    }

    fn execute_credential_rotation<T: CollectorTransport>(
        &mut self,
        transport: &mut T,
        rotation_id: &str,
    ) -> Result<(CommandOutcome, String)> {
        let replacement_token = self.stage_local_credential_rotation(rotation_id)?;
        let (_, replacement_secret) = replacement_token
            .strip_prefix("ddcol_")
            .and_then(|value| value.rsplit_once('.'))
            .context("locally generated replacement credential is invalid")?;
        let mut last_activation_error = None;
        let mut activated = false;
        for credential in self.rotation_activation_candidates()? {
            match transport.activate_collector_credential_rotation(
                &credential,
                &self.identity.machine_id,
                rotation_id,
                replacement_secret,
            ) {
                Ok(()) => {
                    activated = true;
                    break;
                }
                Err(error) if error.class == RetryClass::Unauthorized => {
                    last_activation_error = Some(error);
                }
                Err(error) => return Err(anyhow::anyhow!(error)),
            }
        }
        if !activated {
            return Err(anyhow::anyhow!(last_activation_error.unwrap_or_else(
                || {
                    TransportError::new(
                        RetryClass::Unauthorized,
                        "all Collector credentials rejected during rotation activation",
                    )
                }
            )));
        }
        transport
            .prove_collector_credential_rotation(
                &replacement_token,
                &self.identity.machine_id,
                rotation_id,
            )
            .map_err(|error| anyhow::anyhow!(error))?;
        // Proof is the Hub cutover point. Commit locally immediately after it
        // so a crash can leave either a durable pending secret or the complete
        // new credential, never a partially written replacement.
        let _ = self
            .store
            .commit_staged_collector_credential_if(rotation_id)?;
        self.identity = self
            .store
            .collector_identity()?
            .context("Collector identity disappeared after rotation")?;
        Ok((CommandOutcome::CredentialRotationStaged, replacement_token))
    }

    pub fn handle_owner_command(
        &mut self,
        command: OwnerCommand,
        now: DateTime<Utc>,
    ) -> Result<CommandOutcome> {
        match command {
            OwnerCommand::Refresh { .. } => {
                let report = self.reconcile_manual(now)?;
                Ok(CommandOutcome::Refreshed {
                    batch_id: report.batch_id,
                })
            }
            OwnerCommand::Repair { .. } => {
                // Repair deliberately reuses the complete reconciliation path;
                // it does not create a second SSH or parser state machine.
                let report = self.reconcile_manual(now)?;
                Ok(CommandOutcome::Repaired {
                    batch_id: report.batch_id,
                })
            }
            OwnerCommand::RotateCredential { rotation_id, .. } => {
                self.stage_local_credential_rotation(&rotation_id)?;
                Ok(CommandOutcome::CredentialRotationStaged)
            }
            OwnerCommand::Diagnostics { .. } => Ok(CommandOutcome::Diagnostics {
                diagnostics: self.diagnostics()?,
            }),
            OwnerCommand::ApprovedUpdate {
                update_id,
                command_id,
                version,
                sha256,
            } => {
                let digest = sha256.trim().to_ascii_lowercase();
                let Some(approved) = self
                    .options
                    .approved_updates
                    .iter()
                    .find(|entry| {
                        entry.version == version && entry.sha256.eq_ignore_ascii_case(&digest)
                    })
                    .cloned()
                else {
                    return Ok(CommandOutcome::Rejected {
                        reason: "update is not present in the approved version/digest allowlist"
                            .to_string(),
                    });
                };
                if !is_sha256_hex(&digest) || !is_safe_update_version(&version) {
                    return Ok(CommandOutcome::Rejected {
                        reason: "approved update version or digest is invalid".to_string(),
                    });
                }
                if let Some(directory) = &self.options.update_artifact_dir {
                    let artifact_name = format!("{}.artifact", version);
                    let artifact_path = directory.join(&artifact_name);
                    let artifact = match fs::read(&artifact_path) {
                        Ok(bytes) => bytes,
                        Err(_) => {
                            return Ok(CommandOutcome::Rejected {
                                reason: "approved update artifact is unavailable".to_string(),
                            })
                        }
                    };
                    self.apply_approved_update_artifact(&approved.version, &digest, &artifact)?;
                } else {
                    return Ok(CommandOutcome::Rejected {
                        reason: "approved update artifact directory is not configured".to_string(),
                    });
                }
                self.store
                    .set_collector_state("pending_update_id", &update_id)?;
                self.store
                    .set_collector_state("pending_update_command_id", &command_id)?;
                self.store
                    .set_collector_state("pending_update_version", &version)?;
                self.store
                    .set_collector_state("pending_update_sha256", &digest)?;
                self.store
                    .set_collector_state("restart_requested", "true")?;
                Ok(CommandOutcome::UpdateApplied {
                    update_id,
                    command_id,
                    version,
                    sha256: digest,
                })
            }
            OwnerCommand::RollbackUpdate {
                update_id,
                command_id,
                version,
                sha256,
            } => {
                self.rollback_approved_update(&version, &sha256)?;
                self.store.set_collector_state("pending_update_id", "")?;
                self.store
                    .set_collector_state("restart_requested", "false")?;
                Ok(CommandOutcome::UpdateRolledBack {
                    update_id,
                    command_id,
                    version,
                    sha256,
                })
            }
        }
    }

    fn record_delivery_failure(
        &self,
        record: &CollectorOutboxRecord,
        now: DateTime<Utc>,
        class: RetryClass,
        message: String,
    ) -> Result<()> {
        let attempt = record.attempts.saturating_add(1);
        let terminal = !class.is_retryable();
        let delay = if terminal {
            Duration::ZERO
        } else {
            self.options.retry_policy.delay_for(attempt)
        };
        let next_attempt_at = (now + chrono_from_std(delay)).to_rfc3339();
        self.store.fail_collector_batch_with_class(
            &record.batch_id,
            attempt,
            &next_attempt_at,
            &now.to_rfc3339(),
            &safe_diagnostic(message),
            class.as_str(),
            terminal,
        )?;
        Ok(())
    }

    fn redact_event(
        &self,
        event: &UsageEvent,
        source_key: &str,
        _now: DateTime<Utc>,
    ) -> CollectorUsageEvent {
        let occurred_at = event
            .event_timestamp
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc).to_rfc3339())
            // A missing source timestamp must not turn every forced Refresh
            // into a changed event merely because reconciliation ran later.
            .unwrap_or_else(|| "1970-01-01T00:00:00+00:00".to_string());
        CollectorUsageEvent {
            agent: event.source.as_str().to_string(),
            collector_event_fingerprint: canonical_collector_event_fingerprint(event),
            occurred_at,
            session_key: redacted_identifier(
                &self.identity.project_salt,
                "session",
                &format!("{}|{}", event.source.as_str(), event.session_id),
            ),
            project_key: redacted_identifier(
                &self.identity.project_salt,
                "project",
                &event.project_path,
            ),
            source_key: source_key.to_string(),
            turn_id: event
                .turn_id
                .as_deref()
                .map(|turn| redacted_identifier(&self.identity.project_salt, "turn", turn)),
            provider: safe_identifier(&event.provider, "unknown"),
            model: safe_identifier(&event.model, "unknown"),
            reasoning_effort: event.reasoning_effort.clone(),
            prompt_tokens: event.prompt_tokens,
            completion_tokens: event.completion_tokens,
            cache_read_tokens: event.cache_read_tokens,
            cache_write_tokens: event.cache_write_tokens,
            reasoning_tokens: event.reasoning_tokens,
            total_tokens: event.total_tokens,
            estimated_cost_usd: if event.estimated_cost_usd.is_finite() {
                event.estimated_cost_usd.max(0.0)
            } else {
                0.0
            },
            confidence: event.confidence.clamp(0.0, 1.0),
            parser_name: safe_identifier(&event.parser_name, "collector-parser"),
            parser_version: safe_identifier(&event.parser_version, "unknown-parser-version"),
            pricing_version: safe_identifier(&event.pricing_version, "unpriced"),
            pricing_mode: event.pricing_mode,
            // This is a transport invariant, not a copy of a local import flag.
            metadata_only: true,
        }
    }
}

pub struct CollectorInstanceGuard {
    db: Database,
    owner_id: String,
}

impl CollectorInstanceGuard {
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    pub fn renew(&self, now: DateTime<Utc>, lease_for: Duration) -> Result<bool> {
        let lease_until = now + chrono_from_std(lease_for);
        self.db.renew_collector_instance_lock(
            &self.owner_id,
            &now.to_rfc3339(),
            &lease_until.to_rfc3339(),
        )
    }
}

impl Drop for CollectorInstanceGuard {
    fn drop(&mut self) {
        let _ = self.db.release_collector_instance_lock(&self.owner_id);
    }
}

impl Collector {
    pub fn acquire_instance(&self, now: DateTime<Utc>) -> Result<CollectorInstanceGuard> {
        let owner_id = format!("instance-{}", random_hex(12));
        let lease_until = now + ChronoDuration::minutes(2);
        if !self.store.acquire_collector_instance_lock(
            &owner_id,
            &now.to_rfc3339(),
            &lease_until.to_rfc3339(),
        )? {
            anyhow::bail!("another Collector instance is already running");
        }
        Ok(CollectorInstanceGuard {
            db: self.store.clone(),
            owner_id,
        })
    }
}

pub fn redacted_project_id(project_salt: &str, project_path: &str) -> String {
    redacted_identifier(project_salt, "project", project_path)
}

/// Canonical wire identity used by reconciliation, outbox replay, and Hub
/// deduplication. Keeping the delimiter and fingerprint prefix in one helper
/// prevents local-parser and serialized-payload identities from diverging.
pub fn canonical_event_identity(agent: &str, collector_event_fingerprint: &str) -> String {
    crate::hub::canonical_event_identity(agent, collector_event_fingerprint)
}

fn canonical_collector_event_fingerprint(event: &UsageEvent) -> String {
    format!("event-{}", stable_event_fingerprint(event))
}

fn canonical_event_fingerprint(event: &CollectorUsageEvent) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(event)?)))
}

pub fn redacted_identifier(project_salt: &str, namespace: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project_salt.as_bytes());
    hasher.update(b"\n");
    hasher.update(namespace.as_bytes());
    hasher.update(b"\n");
    hasher.update(value.as_bytes());
    format!("{}-{}", namespace, hex::encode(hasher.finalize()))
}

fn manifest_changed(
    previous: Option<&CollectorManifestRecord>,
    current: &CollectorManifestRecord,
) -> bool {
    let Some(previous) = previous else {
        return true;
    };
    previous.agent != current.agent
        || previous.file_fingerprint != current.file_fingerprint
        || previous.parser_version != current.parser_version
        || previous.item_count != current.item_count
        || previous.cursor != current.cursor
        || previous.parse_error != current.parse_error
}

fn build_checkpoints(
    manifests: &[SourceManifestInput],
    agents: &BTreeSet<String>,
) -> Vec<CheckpointInput> {
    agents
        .iter()
        .map(|agent| {
            let mut hasher = Sha256::new();
            for manifest in manifests.iter().filter(|manifest| &manifest.agent == agent) {
                hasher.update(manifest.manifest_fingerprint.as_bytes());
                hasher.update(b"\n");
            }
            CheckpointInput {
                agent: agent.clone(),
                checkpoint_key: "manifest".to_string(),
                checkpoint_value: format!("cursor-{}", hex::encode(hasher.finalize())),
            }
        })
        .collect()
}

fn reason_string(reason: ReconciliationReason) -> &'static str {
    match reason {
        ReconciliationReason::Startup => "startup",
        ReconciliationReason::Periodic => "periodic",
        ReconciliationReason::Manual => "manual",
        ReconciliationReason::WatcherHint => "watcher-hint",
        ReconciliationReason::WatcherFallback => "watcher-fallback",
    }
}

fn chrono_from_std(duration: Duration) -> ChronoDuration {
    ChronoDuration::from_std(duration).unwrap_or(ChronoDuration::MAX)
}

fn safe_identifier(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if looks_like_secret_identifier(trimmed) {
        return format!(
            "redacted-{}",
            &hex::encode(Sha256::digest(trimmed.as_bytes()))[..24]
        );
    }
    let mut result = trimmed
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':' | '@')
            {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    result.truncate(180);
    if result.is_empty() {
        fallback.to_string()
    } else {
        result
    }
}

fn looks_like_secret_identifier(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    value.starts_with("sk-")
        || value.starts_with("sk_")
        || value.starts_with("ghp_")
        || value.starts_with("github_pat_")
        || value.starts_with("xoxb-")
        || value.starts_with("AKIA")
        || lower.starts_with("bearer ")
        || lower.contains("authorization:")
        || lower.contains("private key")
        || lower.starts_with("ssh-")
        || lower.starts_with("sudo ")
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|character| character.is_ascii_hexdigit())
}

fn is_safe_update_version(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
}

fn is_safe_rotation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | ':' | '@')
        })
}

fn token_credential_id(token: &str) -> Option<&str> {
    token
        .strip_prefix("ddcol_")?
        .rsplit_once('.')
        .map(|(id, _)| id)
}

fn safe_diagnostic(value: String) -> String {
    // Diagnostics cross the same trust boundary as usage metadata. Keep a
    // stable opaque marker for correlation, never a path, error body, or
    // arbitrary provider/session text.
    format!(
        "diagnostic-{}",
        &hex::encode(Sha256::digest(value.as_bytes()))[..24]
    )
}

fn random_hex(bytes: usize) -> String {
    let mut random = vec![0_u8; bytes];
    OsRng.fill_bytes(&mut random);
    hex::encode(random)
}

fn local_machine_bytes() -> Vec<u8> {
    crate::db::local_machine().into_bytes()
}

/// Run the operational outbound Collector. The main thread owns reconciliation
/// and watcher state; delivery and command polling are independent workers so
/// a 20-second long poll never holds a SQLite connection or blocks file
/// notification handling.
pub fn run_daemon(paths: &crate::app_paths::AppPaths, config: &Config) -> Result<()> {
    let hub_url = config
        .collector
        .hub_url
        .as_deref()
        .context("collector.hub_url is required for `collector run`")?;
    // Validate the configured endpoint before acquiring the durable lease.
    let _ = CollectorHttpTransport::new(hub_url)?;
    let usage_db = Database::open(&paths.db_path)?;
    let collector_db = Database::open(&paths.collector_db_path)?;
    let mut runtime = Collector::from_config(usage_db, collector_db, config)?;
    let guard = runtime.acquire_instance(Utc::now())?;
    let stop = Arc::new(AtomicBool::new(false));

    // Signal handling is deliberately outside the transport workers so every
    // worker can observe cancellation between network calls.
    let signal_stop = Arc::clone(&stop);
    thread::spawn(move || {
        if let Ok(runtime) = tokio::runtime::Runtime::new() {
            let _ = runtime.block_on(async { tokio::signal::ctrl_c().await });
            signal_stop.store(true, Ordering::SeqCst);
        }
    });

    runtime.mark_runtime_started(Utc::now())?;
    runtime.reconcile_startup(Utc::now())?;
    // A restart proof is sent from the new process generation, never from the
    // command handler that wrote the replacement artifact.
    if let Ok(mut transport) = CollectorHttpTransport::new(hub_url) {
        let _ = runtime.report_pending_update_receipt(&mut transport, Utc::now());
    }

    let (watch_tx, watch_rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = match RecommendedWatcher::new(
        move |event| {
            let _ = watch_tx.send(event);
        },
        notify::Config::default(),
    ) {
        Ok(mut watcher) => {
            let sources = if config.source_roots.is_empty() {
                importers::scan_sources(config)
            } else {
                importers::scan_configured_sources(config)
            };
            match sources {
                Ok(sources) => {
                    let mut watched = BTreeSet::new();
                    for source in sources {
                        if watched.insert(source.path.clone()) {
                            if let Err(error) =
                                watcher.watch(&source.path, RecursiveMode::Recursive)
                            {
                                runtime.watcher_failed(
                                    Utc::now(),
                                    format!("watcher initialization failed: {error}"),
                                );
                                break;
                            }
                        }
                    }
                    Some(watcher)
                }
                Err(error) => {
                    runtime.watcher_failed(
                        Utc::now(),
                        format!("watcher source discovery failed: {error}"),
                    );
                    Some(watcher)
                }
            }
        }
        Err(error) => {
            runtime.watcher_failed(
                Utc::now(),
                format!("watcher initialization failed: {error}"),
            );
            None
        }
    };

    let worker_stop = Arc::clone(&stop);
    let worker_paths = paths.clone();
    let worker_config = config.clone();
    let delivery_worker = thread::spawn(move || {
        let usage_db = match Database::open(&worker_paths.db_path) {
            Ok(db) => db,
            Err(_) => return,
        };
        let collector_db = match Database::open(&worker_paths.collector_db_path) {
            Ok(db) => db,
            Err(_) => return,
        };
        let mut collector = match Collector::from_config(usage_db, collector_db, &worker_config) {
            Ok(collector) => collector,
            Err(_) => return,
        };
        let mut transport = match CollectorHttpTransport::new(
            worker_config
                .collector
                .hub_url
                .as_deref()
                .unwrap_or_default(),
        ) {
            Ok(transport) => transport,
            Err(_) => return,
        };
        while !worker_stop.load(Ordering::SeqCst) {
            let _ = collector.deliver_pending(&mut transport, Utc::now());
            sleep_until_or_stop(&worker_stop, Duration::from_secs(1));
        }
    });

    let command_stop = Arc::clone(&stop);
    let command_paths = paths.clone();
    let command_config = config.clone();
    let command_worker = thread::spawn(move || {
        let usage_db = match Database::open(&command_paths.db_path) {
            Ok(db) => db,
            Err(_) => return,
        };
        let collector_db = match Database::open(&command_paths.collector_db_path) {
            Ok(db) => db,
            Err(_) => return,
        };
        let mut collector = match Collector::from_config(usage_db, collector_db, &command_config) {
            Ok(collector) => collector,
            Err(_) => return,
        };
        let mut transport = match CollectorHttpTransport::new(
            command_config
                .collector
                .hub_url
                .as_deref()
                .unwrap_or_default(),
        ) {
            Ok(transport) => transport,
            Err(_) => return,
        };
        while !command_stop.load(Ordering::SeqCst) {
            let _ = collector.poll_owner_command(&mut transport, Utc::now());
            // The transport performs the bounded 20-second poll. A short
            // cancellation check keeps shutdown clean after an empty result.
            sleep_until_or_stop(&command_stop, Duration::from_millis(50));
        }
    });

    let mut next_renewal = Utc::now() + ChronoDuration::seconds(30);
    let result = loop {
        if stop.load(Ordering::SeqCst) {
            break Ok(());
        }
        if runtime
            .store
            .collector_state("restart_requested")?
            .as_deref()
            == Some("true")
        {
            stop.store(true, Ordering::SeqCst);
            break Ok(());
        }
        while let Ok(event) = watch_rx.try_recv() {
            match event {
                Ok(event)
                    if matches!(
                        event.kind,
                        EventKind::Create(_)
                            | EventKind::Modify(_)
                            | EventKind::Remove(_)
                            | EventKind::Other
                    ) =>
                {
                    runtime.notify_watcher_hint(Utc::now())
                }
                Ok(_) => {}
                Err(error) => runtime.watcher_failed(Utc::now(), error.to_string()),
            }
        }
        let now = Utc::now();
        if now >= next_renewal {
            match guard.renew(now, Duration::from_secs(90)) {
                Ok(true) => next_renewal = now + ChronoDuration::seconds(30),
                Ok(false) => {
                    stop.store(true, Ordering::SeqCst);
                    break Err(anyhow::anyhow!("Collector instance lease was lost"));
                }
                Err(error) => {
                    stop.store(true, Ordering::SeqCst);
                    break Err(error.context("renewing Collector instance lease"));
                }
            }
        }
        let _ = runtime.reconcile_if_due(now)?;
        sleep_until_or_stop(&stop, Duration::from_millis(100));
    };

    stop.store(true, Ordering::SeqCst);
    let _ = delivery_worker.join();
    let _ = command_worker.join();
    drop(watcher.take());
    drop(guard);
    result
}

fn sleep_until_or_stop(stop: &AtomicBool, duration: Duration) {
    let deadline = std::time::Instant::now() + duration;
    while !stop.load(Ordering::SeqCst) {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}
