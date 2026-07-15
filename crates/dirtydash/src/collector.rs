//! Outbound-only Collector runtime.
//!
//! The Collector owns a separate local SQLite database containing only
//! reconciliation state, redacted request bytes, credentials, and diagnostics.
//! Parser reads happen against the local usage database, while the transport
//! seam never receives a local path or session body.

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{CollectorConfig as FileCollectorConfig, Config, SourceRoot};
use crate::db::{
    CollectorIdentityRecord, CollectorManifestRecord, CollectorOutboxRecord, Database,
    UsageEventWrite,
};
use crate::hub::{
    CheckpointInput, CollectorUsageEvent, IngestBatchRequest, IngestBatchResponse, OwnerCommand,
    SourceManifestInput, SyncRunInput, API_V1_PROTOCOL_VERSION,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovedUpdate {
    pub version: String,
    pub sha256: String,
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
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatcherStatus {
    pub enabled: bool,
    pub degraded: bool,
    pub last_error: Option<String>,
    pub hint_pending: bool,
    pub debounce_until: Option<String>,
    pub next_reconciliation_at: String,
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
    pub next_retry_at: Option<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollectorDiagnostics {
    pub machine_id: String,
    pub parser_versions: Vec<String>,
    pub pending_outbox: u64,
    pub last_reconciliation_at: Option<String>,
    pub watcher: WatcherStatus,
    pub credential_configured: bool,
    pub credential_rotation_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CommandOutcome {
    Refreshed { batch_id: Option<String> },
    CredentialRotationStaged,
    Diagnostics(CollectorDiagnostics),
    UpdateAccepted { version: String, sha256: String },
    Rejected { reason: String },
}

#[derive(Debug, Clone)]
pub struct Collector {
    usage_db: Database,
    store: Database,
    options: CollectorOptions,
    identity: CollectorIdentityRecord,
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

        Ok(Self {
            usage_db,
            store,
            options,
            identity,
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

    pub fn machine_id(&self) -> &str {
        &self.identity.machine_id
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
            credential_configured: identity.credential_token.is_some(),
            credential_rotation_pending: identity.pending_credential_token.is_some(),
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
            manifests.push(CollectorManifestRecord {
                source_key: source_key.clone(),
                agent: parsed_file.source.kind.as_str().to_string(),
                local_path: parsed_file.file.display().to_string(),
                file_fingerprint: parsed_file.file_fingerprint.clone(),
                parser_version: parser_version.to_string(),
                item_count,
                cursor: cursor.clone(),
                parse_error: parsed_file.parse_error.clone(),
                last_reconciled_at: imported_at.clone(),
            });
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

            if !should_reprocess {
                continue;
            }
            files_reprocessed += 1;
            for event in &parsed_file.events {
                match self.usage_db.upsert_usage_event(event)? {
                    UsageEventWrite::Inserted
                    | UsageEventWrite::Updated
                    | UsageEventWrite::Skipped => {}
                }
                let event_id = format!(
                    "{}:{}",
                    event.source.as_str(),
                    stable_event_fingerprint(event)
                );
                if seen_event_ids.insert(event_id) {
                    payload_events.push(self.redact_event(event, &source_key, now));
                }
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
            let payload_json = serde_json::to_string(&request)?;
            self.store.commit_collector_reconciliation(
                &manifests,
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

    pub fn deliver_pending<T: CollectorTransport>(
        &mut self,
        transport: &mut T,
        now: DateTime<Utc>,
    ) -> Result<DeliveryReport> {
        let identity = self
            .store
            .collector_identity()?
            .context("Collector identity is missing")?;
        let credential = identity
            .pending_credential_token
            .as_deref()
            .or(identity.credential_token.as_deref())
            .context("Collector credential is not configured")?
            .to_string();
        let records = self
            .store
            .collector_outbox_ready(&now.to_rfc3339(), DEFAULT_OUTBOX_BATCH_LIMIT)?;
        let mut report = DeliveryReport {
            attempted: 0,
            acknowledged: 0,
            failed: 0,
            pending: 0,
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
            match transport.send_batch(&credential, &request) {
                Ok(response) if response.batch_id == request.batch_id => {
                    if self.store.acknowledge_collector_batch_if_matching(
                        &request.batch_id,
                        &response.batch_id,
                    )? {
                        let _ = self.store.commit_staged_collector_credential()?;
                        report.acknowledged += 1;
                    }
                }
                Ok(response) => {
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
        report.pending = self.store.collector_outbox_count()?;
        Ok(report)
    }

    pub fn poll_owner_command<T: CollectorTransport>(
        &mut self,
        transport: &mut T,
        now: DateTime<Utc>,
    ) -> Result<Option<CommandOutcome>> {
        let identity = self
            .store
            .collector_identity()?
            .context("Collector identity is missing")?;
        let credential = identity
            .pending_credential_token
            .as_deref()
            .or(identity.credential_token.as_deref())
            .context("Collector credential is not configured")?
            .to_string();
        let command = transport
            .poll_owner_command(
                &credential,
                &self.identity.machine_id,
                OWNER_COMMAND_LONG_POLL,
            )
            .map_err(|error| anyhow::anyhow!(error))?;
        let Some(command) = command else {
            return Ok(None);
        };
        let command_id = command.command_id().to_string();
        if let Some(receipt) = self.store.collector_command_result(&command_id)? {
            let outcome = if receipt.status == "completed" {
                serde_json::from_str(&receipt.result_json)
                    .context("stored Collector command receipt is invalid")?
            } else {
                CommandOutcome::Rejected {
                    reason: "command execution was already started before a Collector restart"
                        .to_string(),
                }
            };
            transport
                .acknowledge_owner_command(
                    &credential,
                    &self.identity.machine_id,
                    &command_id,
                    &outcome,
                )
                .map_err(|error| anyhow::anyhow!(error))?;
            return Ok(Some(outcome));
        }
        if !self
            .store
            .begin_collector_command(&command_id, &now.to_rfc3339())?
        {
            return Ok(Some(CommandOutcome::Rejected {
                reason: "command receipt was claimed by another Collector instance".to_string(),
            }));
        }
        let outcome = self.handle_owner_command(command, now)?;
        let result_json = serde_json::to_string(&outcome)?;
        self.store
            .save_collector_command_result(&command_id, &result_json, &now.to_rfc3339())?;
        transport
            .acknowledge_owner_command(
                &credential,
                &self.identity.machine_id,
                &command_id,
                &outcome,
            )
            .map_err(|error| anyhow::anyhow!(error))?;
        Ok(Some(outcome))
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
            OwnerCommand::RotateCredential {
                credential_token, ..
            } => {
                if credential_token.trim().is_empty() {
                    return Ok(CommandOutcome::Rejected {
                        reason: "credential token is empty".to_string(),
                    });
                }
                self.store
                    .stage_collector_credential(credential_token.trim())?;
                Ok(CommandOutcome::CredentialRotationStaged)
            }
            OwnerCommand::Diagnostics { .. } => {
                Ok(CommandOutcome::Diagnostics(self.diagnostics()?))
            }
            OwnerCommand::ApprovedUpdate {
                version, sha256, ..
            } => {
                let digest = sha256.trim().to_ascii_lowercase();
                let approved = self.options.approved_updates.iter().any(|entry| {
                    entry.version == version && entry.sha256.eq_ignore_ascii_case(&digest)
                });
                let digest_valid = digest.len() == 64
                    && digest
                        .chars()
                        .all(|character| character.is_ascii_hexdigit());
                if !approved || !digest_valid {
                    return Ok(CommandOutcome::Rejected {
                        reason: "update is not present in the approved version/digest allowlist"
                            .to_string(),
                    });
                }
                self.store
                    .set_collector_state("approved_update_version", &version)?;
                self.store
                    .set_collector_state("approved_update_sha256", &digest)?;
                Ok(CommandOutcome::UpdateAccepted {
                    version,
                    sha256: digest,
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
        let delay = if class.is_retryable() {
            self.options.retry_policy.delay_for(attempt)
        } else {
            self.options.retry_policy.max_delay
        };
        let next_attempt_at = (now + chrono_from_std(delay)).to_rfc3339();
        self.store.fail_collector_batch(
            &record.batch_id,
            attempt,
            &next_attempt_at,
            &now.to_rfc3339(),
            &safe_diagnostic(message),
        )?;
        Ok(())
    }

    fn redact_event(
        &self,
        event: &UsageEvent,
        source_key: &str,
        now: DateTime<Utc>,
    ) -> CollectorUsageEvent {
        let occurred_at = event
            .event_timestamp
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc).to_rfc3339())
            .unwrap_or_else(|| now.to_rfc3339());
        CollectorUsageEvent {
            agent: event.source.as_str().to_string(),
            collector_event_fingerprint: format!("event-{}", stable_event_fingerprint(event)),
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
            // This is a transport invariant, not a copy of a local import flag.
            metadata_only: true,
        }
    }
}

pub struct CollectorInstanceGuard {
    db: Database,
    owner_id: String,
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

pub fn redacted_identifier(project_salt: &str, namespace: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project_salt.as_bytes());
    hasher.update(b"\n");
    hasher.update(namespace.as_bytes());
    hasher.update(b"\n");
    hasher.update(value.as_bytes());
    format!("{}-{}", namespace, hex::encode(hasher.finalize()))
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
    let mut result = value
        .trim()
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
