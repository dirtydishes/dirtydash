use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use rand::RngCore;
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::Database;

const OWNER_SESSION_COOKIE: &str = "dirtydash_owner_session";
const OWNER_CSRF_HEADER: &str = "x-csrf-token";
const TAILSCALE_USER_LOGIN: &str = "tailscale-user-login";
const TAILSCALE_USER_NAME: &str = "tailscale-user-name";
const SUPPORTED_PROTOCOL_VERSION: u32 = 1;
const OWNER_SESSION_TTL_SECONDS: i64 = 60 * 60 * 12;
const DEFAULT_CREDENTIAL_LABEL: &str = "default";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerTrustMode {
    PrivateTailscale,
    Public,
}

#[derive(Debug, Clone)]
pub struct HubRepository {
    db: Database,
    write_guard: Arc<Mutex<()>>,
}

#[derive(Debug, Clone)]
struct HubState {
    repo: HubRepository,
    trust_mode: ListenerTrustMode,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UsageDayBucket {
    pub day: String,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapOwnerRequest {
    pub username: String,
    pub password: String,
    pub time_zone: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerLoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotateCollectorCredentialRequest {
    pub machine_id: String,
    pub display_name: String,
    #[serde(default = "default_credential_label")]
    pub credential_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeCollectorCredentialRequest {
    pub machine_id: String,
    pub credential_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestBatchRequest {
    pub protocol_version: u32,
    pub batch_id: String,
    pub machine_id: String,
    pub sync_run: SyncRunInput,
    #[serde(default)]
    pub source_manifests: Vec<SourceManifestInput>,
    #[serde(default)]
    pub checkpoints: Vec<CheckpointInput>,
    pub events: Vec<CollectorUsageEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncRunInput {
    pub sync_run_id: String,
    pub collector_version: Option<String>,
    pub started_at: String,
    pub finished_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestInput {
    pub source_key: String,
    pub agent: String,
    pub display_path: String,
    pub item_count: u64,
    pub cursor: Option<String>,
    pub manifest_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointInput {
    pub agent: String,
    pub checkpoint_key: String,
    pub checkpoint_value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectorUsageEvent {
    pub agent: String,
    pub collector_event_fingerprint: String,
    pub occurred_at: String,
    pub session_key: String,
    pub project_key: String,
    pub source_key: String,
    pub turn_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
    pub confidence: f64,
    pub parser_name: String,
    pub parser_version: String,
    pub pricing_version: String,
    #[serde(default = "default_metadata_only_true")]
    pub metadata_only: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AdminSessionResponse {
    pub owner_username: String,
    pub time_zone: String,
    pub csrf_token: String,
    pub trusted_tailscale_user: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CurrentSessionResponse {
    pub authenticated: bool,
    pub owner_username: Option<String>,
    pub time_zone: Option<String>,
    pub trusted_tailscale_user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RotateCollectorCredentialResponse {
    pub machine_id: String,
    pub credential_id: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IngestBatchResponse {
    pub batch_id: String,
    pub inserted_events: u64,
    pub updated_events: u64,
    pub skipped_events: u64,
    pub idempotent_replay: bool,
    pub committed_at: String,
}

#[derive(Debug, Clone, Serialize)]
struct ErrorResponse {
    code: &'static str,
    message: String,
}

#[derive(Debug, Clone)]
pub(crate) struct HubError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

#[derive(Debug, Clone)]
struct OwnerRecord {
    owner_id: String,
    username: String,
    password_hash: String,
    time_zone: String,
}

#[derive(Debug, Clone)]
pub(crate) struct OwnerSessionRecord {
    session_id: String,
    owner_username: String,
    time_zone: String,
    trusted_tailscale_user: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct IssuedOwnerSession {
    session_id: String,
    owner_username: String,
    time_zone: String,
    csrf_token: String,
    trusted_tailscale_user: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct AuthenticatedCollector {
    machine_id: String,
    credential_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct IssuedCollectorCredential {
    machine_id: String,
    credential_id: String,
    token: String,
}

#[derive(Debug, Clone)]
struct ValidatedIngestBatch {
    batch_id: String,
    machine_id: String,
    sync_run: ValidatedSyncRun,
    source_manifests: Vec<ValidatedSourceManifest>,
    checkpoints: Vec<ValidatedCheckpoint>,
    events: Vec<ValidatedCollectorUsageEvent>,
    request_fingerprint: String,
}

#[derive(Debug, Clone)]
struct ValidatedSyncRun {
    sync_run_id: String,
    collector_version: Option<String>,
    started_at: String,
    finished_at: String,
}

#[derive(Debug, Clone)]
struct ValidatedSourceManifest {
    source_key: String,
    agent: String,
    display_path: String,
    item_count: u64,
    cursor: Option<String>,
    manifest_fingerprint: String,
}

#[derive(Debug, Clone)]
struct ValidatedCheckpoint {
    agent: String,
    checkpoint_key: String,
    checkpoint_value: String,
}

#[derive(Debug, Clone)]
struct ValidatedCollectorUsageEvent {
    agent: String,
    collector_event_fingerprint: String,
    occurred_at: String,
    session_key: String,
    project_key: String,
    source_key: String,
    turn_id: Option<String>,
    provider: String,
    model: String,
    reasoning_effort: Option<String>,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    estimated_cost_usd: f64,
    confidence: f64,
    parser_name: String,
    parser_version: String,
    pricing_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageEventWrite {
    Inserted,
    Updated,
    Skipped,
}

fn default_metadata_only_true() -> bool {
    true
}

fn default_credential_label() -> String {
    DEFAULT_CREDENTIAL_LABEL.to_string()
}

impl HubRepository {
    pub fn new(db: Database) -> Self {
        Self {
            db,
            write_guard: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn bootstrap_owner(
        &self,
        request: BootstrapOwnerRequest,
    ) -> Result<IssuedOwnerSession, HubError> {
        let username = validate_identifier(&request.username, "username")?;
        let time_zone = validate_time_zone(&request.time_zone)?;
        let password_hash = hash_password(&request.password)?;
        let issued_at = now_utc();
        let expires_at = plus_seconds(&issued_at, OWNER_SESSION_TTL_SECONDS)?;
        let csrf_token = random_token(24);
        let session_id = random_token(24);
        let csrf_hash = sha256_hex(&csrf_token);
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let existing_owner = tx
            .query_row("SELECT owner_id FROM owners LIMIT 1", [], |row| row.get::<_, String>(0))
            .optional()
            .map_err(HubError::internal)?;
        if existing_owner.is_some() {
            return Err(HubError::conflict(
                "owner-already-bootstrapped",
                "owner bootstrap is only allowed before the first owner exists",
            ));
        }
        let owner_id = random_token(12);
        tx.execute(
            r#"
            INSERT INTO owners(owner_id, username, password_hash, time_zone, created_at, updated_at, password_updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?5)
            "#,
            params![owner_id, username, password_hash, time_zone, issued_at],
        )
        .map_err(HubError::internal)?;
        tx.execute(
            r#"
            INSERT INTO owner_sessions(session_id, owner_id, csrf_token_hash, trusted_tailscale_user, created_at, last_seen_at, expires_at)
            VALUES (?1, ?2, ?3, NULL, ?4, ?4, ?5)
            "#,
            params![session_id, owner_id, csrf_hash, issued_at, expires_at],
        )
        .map_err(HubError::internal)?;
        tx.commit().map_err(HubError::internal)?;
        Ok(IssuedOwnerSession {
            session_id,
            owner_username: request.username,
            time_zone,
            csrf_token,
            trusted_tailscale_user: None,
        })
    }

    pub(crate) fn login_owner(&self, request: OwnerLoginRequest) -> Result<IssuedOwnerSession, HubError> {
        let owner = self.owner_by_username(&request.username)?;
        verify_password(&owner.password_hash, &request.password)?;
        self.issue_owner_session(&owner, None)
    }

    pub(crate) fn login_owner_via_tailscale(
        &self,
        trusted_tailscale_user: &str,
    ) -> Result<IssuedOwnerSession, HubError> {
        let trusted_tailscale_user = validate_non_empty(trusted_tailscale_user, "trusted tailscale user")?;
        let owner = self.first_owner()?;
        self.issue_owner_session(&owner, Some(trusted_tailscale_user))
    }

    pub(crate) fn authenticate_owner_session(
        &self,
        session_id: &str,
    ) -> Result<OwnerSessionRecord, HubError> {
        let session_id = validate_non_empty(session_id, "owner session")?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        let now = now_utc();
        let session = conn
            .query_row(
                r#"
                SELECT s.session_id, s.owner_id, o.username, o.time_zone, s.trusted_tailscale_user
                FROM owner_sessions s
                JOIN owners o ON o.owner_id = s.owner_id
                WHERE s.session_id = ?1
                    AND s.revoked_at IS NULL
                    AND s.expires_at > ?2
                "#,
                params![session_id, now],
                |row| {
                    Ok(OwnerSessionRecord {
                        session_id: row.get(0)?,
                        owner_username: row.get(2)?,
                        time_zone: row.get(3)?,
                        trusted_tailscale_user: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some(session) = session else {
            return Err(HubError::unauthorized(
                "owner-session-required",
                "a valid owner session is required",
            ));
        };
        conn.execute(
            "UPDATE owner_sessions SET last_seen_at = ?2 WHERE session_id = ?1",
            params![session.session_id, now_utc()],
        )
        .map_err(HubError::internal)?;
        Ok(session)
    }

    pub(crate) fn verify_owner_csrf(&self, session_id: &str, csrf_token: &str) -> Result<(), HubError> {
        let csrf_token = validate_non_empty(csrf_token, "csrf token")?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        let expected_hash = conn
            .query_row(
                r#"
                SELECT csrf_token_hash
                FROM owner_sessions
                WHERE session_id = ?1
                    AND revoked_at IS NULL
                "#,
                params![session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(HubError::internal)?
            .ok_or_else(|| {
                HubError::unauthorized("owner-session-required", "a valid owner session is required")
            })?;
        if sha256_hex(&csrf_token) != expected_hash {
            return Err(HubError::forbidden(
                "csrf-mismatch",
                "state-changing admin requests require a matching CSRF token",
            ));
        }
        Ok(())
    }

    pub(crate) fn logout_owner(&self, session_id: &str) -> Result<(), HubError> {
        let conn = self.db.connection().map_err(HubError::internal)?;
        conn.execute(
            "UPDATE owner_sessions SET revoked_at = ?2 WHERE session_id = ?1",
            params![session_id, now_utc()],
        )
        .map_err(HubError::internal)?;
        Ok(())
    }

    pub(crate) fn rotate_collector_credential(
        &self,
        request: RotateCollectorCredentialRequest,
    ) -> Result<IssuedCollectorCredential, HubError> {
        let machine_id = validate_identifier(&request.machine_id, "machine_id")?;
        let display_name = validate_non_empty(&request.display_name, "display_name")?;
        let credential_label = validate_identifier(&request.credential_label, "credential_label")?;
        let credential_id = random_token(12);
        let secret = random_token(24);
        let secret_hash = sha256_hex(&secret);
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        tx.execute(
            r#"
            INSERT INTO machines(machine_id, display_name, enrolled_at, last_seen_at)
            VALUES (?1, ?2, ?3, ?3)
            ON CONFLICT(machine_id) DO UPDATE SET
                display_name = excluded.display_name,
                revoked_at = NULL
            "#,
            params![machine_id, display_name, now],
        )
        .map_err(HubError::internal)?;
        tx.execute(
            r#"
            UPDATE collector_credentials
            SET revoked_at = ?3
            WHERE machine_id = ?1
                AND credential_label = ?2
                AND revoked_at IS NULL
            "#,
            params![machine_id, credential_label, now],
        )
        .map_err(HubError::internal)?;
        tx.execute(
            r#"
            INSERT INTO collector_credentials(
                credential_id, machine_id, credential_label, secret_hash, created_at, rotated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            "#,
            params![credential_id, machine_id, credential_label, secret_hash, now],
        )
        .map_err(HubError::internal)?;
        tx.commit().map_err(HubError::internal)?;
        Ok(IssuedCollectorCredential {
            machine_id,
            credential_id: credential_id.clone(),
            token: format!("ddcol_{credential_id}.{secret}"),
        })
    }

    pub(crate) fn revoke_collector_credential(
        &self,
        request: RevokeCollectorCredentialRequest,
    ) -> Result<(), HubError> {
        let machine_id = validate_identifier(&request.machine_id, "machine_id")?;
        let credential_id = validate_identifier(&request.credential_id, "credential_id")?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        let changed = conn
            .execute(
                r#"
                UPDATE collector_credentials
                SET revoked_at = ?3
                WHERE machine_id = ?1
                    AND credential_id = ?2
                    AND revoked_at IS NULL
                "#,
                params![machine_id, credential_id, now_utc()],
            )
            .map_err(HubError::internal)?;
        if changed == 0 {
            return Err(HubError::not_found(
                "collector-credential-not-found",
                "collector credential was not found or is already revoked",
            ));
        }
        Ok(())
    }

    pub(crate) fn authenticate_collector_bearer(
        &self,
        bearer_token: &str,
    ) -> Result<AuthenticatedCollector, HubError> {
        let bearer_token = validate_non_empty(bearer_token, "collector bearer token")?;
        let token = bearer_token
            .strip_prefix("ddcol_")
            .ok_or_else(|| HubError::unauthorized("collector-auth-required", "collector bearer token is invalid"))?;
        let (credential_id, secret) = token.split_once('.').ok_or_else(|| {
            HubError::unauthorized("collector-auth-required", "collector bearer token is invalid")
        })?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        let record = conn
            .query_row(
                r#"
                SELECT c.machine_id, c.secret_hash
                FROM collector_credentials c
                JOIN machines m ON m.machine_id = c.machine_id
                WHERE c.credential_id = ?1
                    AND c.revoked_at IS NULL
                    AND m.revoked_at IS NULL
                "#,
                params![credential_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(HubError::internal)?
            .ok_or_else(|| {
                HubError::unauthorized("collector-auth-required", "collector bearer token is invalid")
            })?;
        if sha256_hex(secret) != record.1 {
            return Err(HubError::unauthorized(
                "collector-auth-required",
                "collector bearer token is invalid",
            ));
        }
        conn.execute(
            "UPDATE collector_credentials SET last_used_at = ?2 WHERE credential_id = ?1",
            params![credential_id, now_utc()],
        )
        .map_err(HubError::internal)?;
        Ok(AuthenticatedCollector {
            machine_id: record.0,
            credential_id: credential_id.to_string(),
        })
    }

    pub(crate) fn ingest_batch(
        &self,
        auth: &AuthenticatedCollector,
        request: IngestBatchRequest,
    ) -> Result<IngestBatchResponse, HubError> {
        let validated = validate_ingest_batch(request, &auth.machine_id)?;
        let committed_at = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;

        let existing_batch = tx
            .query_row(
                r#"
                SELECT request_fingerprint, event_count, committed_at
                FROM ingest_batches
                WHERE machine_id = ?1 AND batch_id = ?2
                "#,
                params![validated.machine_id, validated.batch_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)? as u64,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(HubError::internal)?;
        if let Some((request_fingerprint, event_count, committed_at)) = existing_batch {
            if request_fingerprint != validated.request_fingerprint {
                return Err(HubError::conflict(
                    "ingest-batch-conflict",
                    "batch_id was already committed with a different payload",
                ));
            }
            tx.commit().map_err(HubError::internal)?;
            return Ok(IngestBatchResponse {
                batch_id: validated.batch_id,
                inserted_events: 0,
                updated_events: 0,
                skipped_events: event_count,
                idempotent_replay: true,
                committed_at,
            });
        }

        tx.execute(
            "UPDATE machines SET last_seen_at = ?2 WHERE machine_id = ?1",
            params![validated.machine_id, committed_at],
        )
        .map_err(HubError::internal)?;

        tx.execute(
            r#"
            INSERT INTO sync_runs(machine_id, sync_run_id, collector_version, started_at, finished_at, status, event_count, batch_id)
            VALUES (?1, ?2, ?3, ?4, ?5, 'committed', ?6, ?7)
            ON CONFLICT(machine_id, sync_run_id) DO UPDATE SET
                collector_version = excluded.collector_version,
                started_at = excluded.started_at,
                finished_at = excluded.finished_at,
                status = excluded.status,
                event_count = excluded.event_count,
                batch_id = excluded.batch_id
            "#,
            params![
                validated.machine_id,
                validated.sync_run.sync_run_id,
                validated.sync_run.collector_version,
                validated.sync_run.started_at,
                validated.sync_run.finished_at,
                validated.events.len() as u64,
                validated.batch_id,
            ],
        )
        .map_err(HubError::internal)?;

        for manifest in &validated.source_manifests {
            tx.execute(
                r#"
                INSERT INTO source_manifests(
                    machine_id, sync_run_id, source_key, agent, display_path, item_count, cursor, manifest_fingerprint, recorded_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(machine_id, sync_run_id, source_key) DO UPDATE SET
                    agent = excluded.agent,
                    display_path = excluded.display_path,
                    item_count = excluded.item_count,
                    cursor = excluded.cursor,
                    manifest_fingerprint = excluded.manifest_fingerprint,
                    recorded_at = excluded.recorded_at
                "#,
                params![
                    validated.machine_id,
                    validated.sync_run.sync_run_id,
                    manifest.source_key,
                    manifest.agent,
                    manifest.display_path,
                    manifest.item_count,
                    manifest.cursor,
                    manifest.manifest_fingerprint,
                    committed_at,
                ],
            )
            .map_err(HubError::internal)?;
        }

        for checkpoint in &validated.checkpoints {
            tx.execute(
                r#"
                INSERT INTO ingest_checkpoints(machine_id, agent, checkpoint_key, checkpoint_value, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(machine_id, agent, checkpoint_key) DO UPDATE SET
                    checkpoint_value = excluded.checkpoint_value,
                    updated_at = excluded.updated_at
                "#,
                params![
                    validated.machine_id,
                    checkpoint.agent,
                    checkpoint.checkpoint_key,
                    checkpoint.checkpoint_value,
                    committed_at,
                ],
            )
            .map_err(HubError::internal)?;
        }

        let mut inserted_events = 0_u64;
        let mut updated_events = 0_u64;
        let mut skipped_events = 0_u64;
        for event in &validated.events {
            match upsert_usage_event_tx(
                &tx,
                &validated.machine_id,
                &validated.batch_id,
                event,
                &committed_at,
            )? {
                UsageEventWrite::Inserted => inserted_events += 1,
                UsageEventWrite::Updated => updated_events += 1,
                UsageEventWrite::Skipped => skipped_events += 1,
            }
        }

        tx.execute(
            r#"
            INSERT INTO ingest_batches(
                machine_id, batch_id, protocol_version, credential_id, request_fingerprint,
                event_count, source_manifest_count, checkpoint_count, sync_run_id, committed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                validated.machine_id,
                validated.batch_id,
                SUPPORTED_PROTOCOL_VERSION,
                auth.credential_id,
                validated.request_fingerprint,
                validated.events.len() as u64,
                validated.source_manifests.len() as u64,
                validated.checkpoints.len() as u64,
                validated.sync_run.sync_run_id,
                committed_at,
            ],
        )
        .map_err(HubError::internal)?;

        tx.commit().map_err(HubError::internal)?;
        Ok(IngestBatchResponse {
            batch_id: validated.batch_id,
            inserted_events,
            updated_events,
            skipped_events,
            idempotent_replay: false,
            committed_at,
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn usage_by_day_in_owner_time_zone(
        &self,
        time_zone: &str,
    ) -> Result<Vec<UsageDayBucket>, HubError> {
        let time_zone = validate_time_zone(time_zone)?;
        let tz: Tz = time_zone.parse().map_err(|_| {
            HubError::unprocessable("invalid-time-zone", "time_zone must be a valid IANA time zone")
        })?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT COALESCE(event_timestamp, imported_at), total_tokens, estimated_cost_usd
                FROM usage_events
                ORDER BY COALESCE(event_timestamp, imported_at)
                "#,
            )
            .map_err(HubError::internal)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, f64>(2)?,
                ))
            })
            .map_err(HubError::internal)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(HubError::internal)?;
        let mut buckets = BTreeMap::<NaiveDate, (u64, f64)>::new();
        for (timestamp, total_tokens, estimated_cost_usd) in rows {
            let utc = parse_utc_timestamp(&timestamp)?;
            let day = utc.with_timezone(&tz).date_naive();
            let entry = buckets.entry(day).or_insert((0, 0.0));
            entry.0 += total_tokens;
            entry.1 += estimated_cost_usd;
        }
        Ok(buckets
            .into_iter()
            .map(|(day, (total_tokens, estimated_cost_usd))| UsageDayBucket {
                day: day.to_string(),
                total_tokens,
                estimated_cost_usd,
            })
            .collect())
    }

    fn first_owner(&self) -> Result<OwnerRecord, HubError> {
        let conn = self.db.connection().map_err(HubError::internal)?;
        conn.query_row(
            r#"
            SELECT owner_id, username, password_hash, time_zone
            FROM owners
            ORDER BY created_at ASC
            LIMIT 1
            "#,
            [],
            |row| {
                Ok(OwnerRecord {
                    owner_id: row.get(0)?,
                    username: row.get(1)?,
                    password_hash: row.get(2)?,
                    time_zone: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(HubError::internal)?
        .ok_or_else(|| {
            HubError::unauthorized(
                "owner-auth-required",
                "no owner account is available for this login path",
            )
        })
    }

    fn owner_by_username(&self, username: &str) -> Result<OwnerRecord, HubError> {
        let username = validate_non_empty(username, "username")?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        conn.query_row(
            r#"
            SELECT owner_id, username, password_hash, time_zone
            FROM owners
            WHERE username = ?1
            "#,
            params![username],
            |row| {
                Ok(OwnerRecord {
                    owner_id: row.get(0)?,
                    username: row.get(1)?,
                    password_hash: row.get(2)?,
                    time_zone: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(HubError::internal)?
        .ok_or_else(|| {
            HubError::unauthorized("owner-auth-required", "owner credentials are invalid")
        })
    }

    fn issue_owner_session(
        &self,
        owner: &OwnerRecord,
        trusted_tailscale_user: Option<String>,
    ) -> Result<IssuedOwnerSession, HubError> {
        let session_id = random_token(24);
        let csrf_token = random_token(24);
        let csrf_hash = sha256_hex(&csrf_token);
        let now = now_utc();
        let expires_at = plus_seconds(&now, OWNER_SESSION_TTL_SECONDS)?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        conn.execute(
            r#"
            INSERT INTO owner_sessions(session_id, owner_id, csrf_token_hash, trusted_tailscale_user, created_at, last_seen_at, expires_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)
            "#,
            params![
                session_id,
                owner.owner_id,
                csrf_hash,
                trusted_tailscale_user,
                now,
                expires_at,
            ],
        )
        .map_err(HubError::internal)?;
        Ok(IssuedOwnerSession {
            session_id,
            owner_username: owner.username.clone(),
            time_zone: owner.time_zone.clone(),
            csrf_token,
            trusted_tailscale_user,
        })
    }
}

pub fn build_router(repo: HubRepository, trust_mode: ListenerTrustMode) -> Router {
    Router::new()
        .route("/api/v1/admin/bootstrap", post(admin_bootstrap))
        .route("/api/v1/admin/session", get(admin_session))
        .route("/api/v1/admin/session/login", post(admin_login))
        .route("/api/v1/admin/session/tailscale", post(admin_tailscale_login))
        .route("/api/v1/admin/session/logout", post(admin_logout))
        .route(
            "/api/v1/admin/collector-credentials/rotate",
            post(admin_rotate_collector_credential),
        )
        .route(
            "/api/v1/admin/collector-credentials/revoke",
            post(admin_revoke_collector_credential),
        )
        .route("/api/v1/ingest/batches", post(collector_ingest_batch))
        .with_state(HubState { repo, trust_mode })
}

async fn admin_bootstrap(
    State(state): State<HubState>,
    Json(request): Json<BootstrapOwnerRequest>,
) -> Result<Response, HubError> {
    let session = state.repo.bootstrap_owner(request)?;
    Ok(session_response(session))
}

async fn admin_login(
    State(state): State<HubState>,
    Json(request): Json<OwnerLoginRequest>,
) -> Result<Response, HubError> {
    let session = state.repo.login_owner(request)?;
    Ok(session_response(session))
}

async fn admin_tailscale_login(
    State(state): State<HubState>,
    headers: HeaderMap,
) -> Result<Response, HubError> {
    let trusted_login = trusted_tailscale_login(&headers, state.trust_mode).ok_or_else(|| {
        HubError::unauthorized(
            "trusted-tailscale-required",
            "trusted Tailscale identity is required on this listener",
        )
    })?;
    let session = state.repo.login_owner_via_tailscale(&trusted_login)?;
    Ok(session_response(session))
}

async fn admin_session(
    State(state): State<HubState>,
    headers: HeaderMap,
) -> Result<Json<CurrentSessionResponse>, HubError> {
    if let Some(session_id) = owner_session_cookie(&headers) {
        let session = state.repo.authenticate_owner_session(&session_id)?;
        return Ok(Json(CurrentSessionResponse {
            authenticated: true,
            owner_username: Some(session.owner_username),
            time_zone: Some(session.time_zone),
            trusted_tailscale_user: session.trusted_tailscale_user,
        }));
    }
    Ok(Json(CurrentSessionResponse {
        authenticated: false,
        owner_username: None,
        time_zone: None,
        trusted_tailscale_user: trusted_tailscale_login(&headers, state.trust_mode),
    }))
}

async fn admin_logout(
    State(state): State<HubState>,
    headers: HeaderMap,
) -> Result<Response, HubError> {
    let session = require_owner_session(&state, &headers, true)?;
    state.repo.logout_owner(&session.session_id)?;
    Ok(logout_response())
}

async fn admin_rotate_collector_credential(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(request): Json<RotateCollectorCredentialRequest>,
) -> Result<Json<RotateCollectorCredentialResponse>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    let issued = state.repo.rotate_collector_credential(request)?;
    Ok(Json(RotateCollectorCredentialResponse {
        machine_id: issued.machine_id,
        credential_id: issued.credential_id,
        token: issued.token,
    }))
}

async fn admin_revoke_collector_credential(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(request): Json<RevokeCollectorCredentialRequest>,
) -> Result<StatusCode, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    state.repo.revoke_collector_credential(request)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn collector_ingest_batch(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(request): Json<IngestBatchRequest>,
) -> Result<Json<IngestBatchResponse>, HubError> {
    let auth = collector_auth(&state.repo, &headers)?;
    let response = state.repo.ingest_batch(&auth, request)?;
    Ok(Json(response))
}

fn require_owner_session(
    state: &HubState,
    headers: &HeaderMap,
    require_csrf: bool,
) -> Result<OwnerSessionRecord, HubError> {
    let session_id = owner_session_cookie(headers).ok_or_else(|| {
        HubError::unauthorized("owner-session-required", "a valid owner session is required")
    })?;
    let session = state.repo.authenticate_owner_session(&session_id)?;
    if require_csrf {
        let csrf = header_value(headers, OWNER_CSRF_HEADER).ok_or_else(|| {
            HubError::forbidden(
                "csrf-mismatch",
                "state-changing admin requests require a matching CSRF token",
            )
        })?;
        state.repo.verify_owner_csrf(&session.session_id, &csrf)?;
    }
    Ok(session)
}

fn collector_auth(repo: &HubRepository, headers: &HeaderMap) -> Result<AuthenticatedCollector, HubError> {
    let auth_header = header_value(headers, header::AUTHORIZATION.as_str()).ok_or_else(|| {
        HubError::unauthorized(
            "collector-auth-required",
            "collector bearer authentication is required",
        )
    })?;
    let bearer = auth_header.strip_prefix("Bearer ").ok_or_else(|| {
        HubError::unauthorized(
            "collector-auth-required",
            "collector bearer authentication is required",
        )
    })?;
    repo.authenticate_collector_bearer(bearer)
}

fn session_response(session: IssuedOwnerSession) -> Response {
    let body = Json(AdminSessionResponse {
        owner_username: session.owner_username,
        time_zone: session.time_zone,
        csrf_token: session.csrf_token,
        trusted_tailscale_user: session.trusted_tailscale_user,
    });
    let mut response = body.into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        session_cookie_header(&session.session_id),
    );
    response
}

fn logout_response() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "dirtydash_owner_session=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0",
        ),
    );
    response
}

fn session_cookie_header(session_id: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{OWNER_SESSION_COOKIE}={session_id}; HttpOnly; Path=/; SameSite=Lax"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("dirtydash_owner_session=invalid; HttpOnly; Path=/; SameSite=Lax"))
}

fn owner_session_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = header_value(headers, header::COOKIE.as_str())?;
    raw.split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(&format!("{OWNER_SESSION_COOKIE}=")))
        .map(ToOwned::to_owned)
}

fn trusted_tailscale_login(headers: &HeaderMap, trust_mode: ListenerTrustMode) -> Option<String> {
    if trust_mode != ListenerTrustMode::PrivateTailscale {
        return None;
    }
    header_value(headers, TAILSCALE_USER_LOGIN)
        .or_else(|| header_value(headers, TAILSCALE_USER_NAME))
}

fn validate_ingest_batch(
    request: IngestBatchRequest,
    authenticated_machine_id: &str,
) -> Result<ValidatedIngestBatch, HubError> {
    if request.protocol_version != SUPPORTED_PROTOCOL_VERSION {
        return Err(HubError::conflict(
            "incompatible-protocol-version",
            "this Hub only accepts protocol_version=1 for /api/v1 requests",
        ));
    }
    let machine_id = validate_identifier(&request.machine_id, "machine_id")?;
    if machine_id != authenticated_machine_id {
        return Err(HubError::unauthorized(
            "collector-machine-mismatch",
            "collector credential is not authorized for the requested machine_id",
        ));
    }
    let batch_id = validate_identifier(&request.batch_id, "batch_id")?;
    let request_fingerprint = sha256_json(&request)?;
    let sync_run = ValidatedSyncRun {
        sync_run_id: validate_identifier(&request.sync_run.sync_run_id, "sync_run_id")?,
        collector_version: request
            .sync_run
            .collector_version
            .as_deref()
            .map(|value| validate_non_empty(value, "collector_version"))
            .transpose()?,
        started_at: normalize_utc_timestamp(&request.sync_run.started_at)?,
        finished_at: normalize_utc_timestamp(&request.sync_run.finished_at)?,
    };

    let mut manifest_keys = BTreeSet::new();
    let mut source_manifests = Vec::with_capacity(request.source_manifests.len());
    for manifest in request.source_manifests {
        let source_key = validate_display_safe_key(&manifest.source_key, "source_key")?;
        if !manifest_keys.insert(source_key.clone()) {
            return Err(HubError::unprocessable(
                "duplicate-source-manifest",
                "source_manifests must not repeat source_key within a batch",
            ));
        }
        source_manifests.push(ValidatedSourceManifest {
            source_key,
            agent: validate_identifier(&manifest.agent, "agent")?,
            display_path: validate_display_safe_key(&manifest.display_path, "display_path")?,
            item_count: manifest.item_count,
            cursor: manifest
                .cursor
                .as_deref()
                .map(|value| validate_non_empty(value, "cursor"))
                .transpose()?,
            manifest_fingerprint: validate_identifier(
                &manifest.manifest_fingerprint,
                "manifest_fingerprint",
            )?,
        });
    }

    let mut checkpoint_keys = BTreeSet::new();
    let mut checkpoints = Vec::with_capacity(request.checkpoints.len());
    for checkpoint in request.checkpoints {
        let agent = validate_identifier(&checkpoint.agent, "checkpoint agent")?;
        let checkpoint_key = validate_identifier(&checkpoint.checkpoint_key, "checkpoint_key")?;
        if !checkpoint_keys.insert(format!("{agent}:{checkpoint_key}")) {
            return Err(HubError::unprocessable(
                "duplicate-checkpoint",
                "checkpoints must be unique per agent and checkpoint_key within a batch",
            ));
        }
        checkpoints.push(ValidatedCheckpoint {
            agent,
            checkpoint_key,
            checkpoint_value: validate_non_empty(&checkpoint.checkpoint_value, "checkpoint_value")?,
        });
    }

    let mut event_identities = BTreeSet::new();
    let mut events = Vec::with_capacity(request.events.len());
    for event in request.events {
        if !event.metadata_only {
            return Err(HubError::unprocessable(
                "metadata-only-required",
                "metadata_only must stay true for /api/v1 ingestion",
            ));
        }
        let agent = validate_identifier(&event.agent, "agent")?;
        let collector_event_fingerprint = validate_identifier(
            &event.collector_event_fingerprint,
            "collector_event_fingerprint",
        )?;
        if !event_identities.insert(format!("{agent}:{collector_event_fingerprint}")) {
            return Err(HubError::unprocessable(
                "duplicate-event-identity",
                "events must be unique by agent and collector_event_fingerprint within a batch",
            ));
        }
        events.push(ValidatedCollectorUsageEvent {
            agent,
            collector_event_fingerprint,
            occurred_at: normalize_utc_timestamp(&event.occurred_at)?,
            session_key: validate_display_safe_key(&event.session_key, "session_key")?,
            project_key: validate_display_safe_key(&event.project_key, "project_key")?,
            source_key: validate_display_safe_key(&event.source_key, "source_key")?,
            turn_id: event
                .turn_id
                .as_deref()
                .map(|value| validate_identifier(value, "turn_id"))
                .transpose()?,
            provider: validate_identifier(&event.provider, "provider")?,
            model: validate_non_empty(&event.model, "model")?,
            reasoning_effort: event
                .reasoning_effort
                .as_deref()
                .map(|value| validate_identifier(value, "reasoning_effort"))
                .transpose()?,
            prompt_tokens: event.prompt_tokens,
            completion_tokens: event.completion_tokens,
            cache_read_tokens: event.cache_read_tokens,
            cache_write_tokens: event.cache_write_tokens,
            reasoning_tokens: event.reasoning_tokens,
            total_tokens: event.total_tokens,
            estimated_cost_usd: event.estimated_cost_usd,
            confidence: event.confidence,
            parser_name: validate_identifier(&event.parser_name, "parser_name")?,
            parser_version: validate_identifier(&event.parser_version, "parser_version")?,
            pricing_version: validate_identifier(&event.pricing_version, "pricing_version")?,
        });
    }

    Ok(ValidatedIngestBatch {
        batch_id,
        machine_id,
        sync_run,
        source_manifests,
        checkpoints,
        events,
        request_fingerprint,
    })
}

fn upsert_usage_event_tx(
    tx: &Transaction<'_>,
    machine_id: &str,
    batch_id: &str,
    event: &ValidatedCollectorUsageEvent,
    imported_at: &str,
) -> Result<UsageEventWrite, HubError> {
    let raw_event_hash = sha256_hex(&format!(
        "{machine_id}\n{}\n{}",
        event.agent, event.collector_event_fingerprint
    ));
    let existing = tx
        .query_row(
            r#"
            SELECT provider, model, turn_id, reasoning_effort, prompt_tokens, completion_tokens,
                cache_read_tokens, cache_write_tokens, reasoning_tokens, total_tokens,
                estimated_cost_usd, confidence, pricing_version, event_timestamp,
                project_path, session_id, raw_path, parser_name, parser_version
            FROM usage_events
            WHERE machine_id = ?1
                AND agent = ?2
                AND collector_event_fingerprint = ?3
            "#,
            params![machine_id, event.agent, event.collector_event_fingerprint],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)? as u64,
                    row.get::<_, i64>(5)? as u64,
                    row.get::<_, i64>(6)? as u64,
                    row.get::<_, i64>(7)? as u64,
                    row.get::<_, i64>(8)? as u64,
                    row.get::<_, i64>(9)? as u64,
                    row.get::<_, f64>(10)?,
                    row.get::<_, f64>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, String>(17)?,
                    row.get::<_, String>(18)?,
                ))
            },
        )
        .optional()
        .map_err(HubError::internal)?;
    if let Some(existing) = existing {
        let matches = existing.0 == event.provider
            && existing.1 == event.model
            && existing.2 == event.turn_id
            && existing.3 == event.reasoning_effort
            && existing.4 == event.prompt_tokens
            && existing.5 == event.completion_tokens
            && existing.6 == event.cache_read_tokens
            && existing.7 == event.cache_write_tokens
            && existing.8 == event.reasoning_tokens
            && existing.9 == event.total_tokens
            && (existing.10 - event.estimated_cost_usd).abs() < 0.0000001
            && (existing.11 - event.confidence).abs() < 0.0000001
            && existing.12 == event.pricing_version
            && existing.13.as_deref() == Some(event.occurred_at.as_str())
            && existing.14 == event.project_key
            && existing.15 == event.session_key
            && existing.16 == event.source_key
            && existing.17 == event.parser_name
            && existing.18 == event.parser_version;
        if matches {
            return Ok(UsageEventWrite::Skipped);
        }
        tx.execute(
            r#"
            UPDATE usage_events
            SET provider = ?4,
                model = ?5,
                turn_id = ?6,
                reasoning_effort = ?7,
                prompt_tokens = ?8,
                completion_tokens = ?9,
                cache_read_tokens = ?10,
                cache_write_tokens = ?11,
                reasoning_tokens = ?12,
                total_tokens = ?13,
                estimated_cost_usd = ?14,
                confidence = ?15,
                event_timestamp = ?16,
                project_path = ?17,
                session_id = ?18,
                raw_path = ?19,
                parser_name = ?20,
                parser_version = ?21,
                imported_at = ?22,
                pricing_version = ?23,
                pricing_mode = 'unpriced',
                metadata_only = 1,
                raw_event_hash = ?24,
                machine = ?1,
                source = ?2,
                ingest_batch_id = ?3
            WHERE machine_id = ?1
                AND agent = ?2
                AND collector_event_fingerprint = ?25
            "#,
            params![
                machine_id,
                event.agent,
                batch_id,
                event.provider,
                event.model,
                event.turn_id,
                event.reasoning_effort,
                event.prompt_tokens,
                event.completion_tokens,
                event.cache_read_tokens,
                event.cache_write_tokens,
                event.reasoning_tokens,
                event.total_tokens,
                event.estimated_cost_usd,
                event.confidence,
                event.occurred_at,
                event.project_key,
                event.session_key,
                event.source_key,
                event.parser_name,
                event.parser_version,
                imported_at,
                event.pricing_version,
                raw_event_hash,
                event.collector_event_fingerprint,
            ],
        )
        .map_err(HubError::internal)?;
        return Ok(UsageEventWrite::Updated);
    }

    tx.execute(
        r#"
        INSERT INTO usage_events(
            machine, source, project_path, session_id, turn_id, provider, model,
            reasoning_effort, prompt_tokens, completion_tokens, cache_read_tokens,
            cache_write_tokens, reasoning_tokens, total_tokens, estimated_cost_usd,
            confidence, event_timestamp, raw_path, raw_span, parser_name, parser_version,
            raw_event_hash, imported_at, pricing_version, pricing_mode, metadata_only,
            machine_id, agent, collector_event_fingerprint, ingest_batch_id
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7,
            ?8, ?9, ?10, ?11,
            ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, NULL, ?19, ?20,
            ?21, ?22, ?23, 'unpriced', 1,
            ?1, ?2, ?24, ?25
        )
        "#,
        params![
            machine_id,
            event.agent,
            event.project_key,
            event.session_key,
            event.turn_id,
            event.provider,
            event.model,
            event.reasoning_effort,
            event.prompt_tokens,
            event.completion_tokens,
            event.cache_read_tokens,
            event.cache_write_tokens,
            event.reasoning_tokens,
            event.total_tokens,
            event.estimated_cost_usd,
            event.confidence,
            event.occurred_at,
            event.source_key,
            event.parser_name,
            event.parser_version,
            raw_event_hash,
            imported_at,
            event.pricing_version,
            event.collector_event_fingerprint,
            batch_id,
        ],
    )
    .map_err(HubError::internal)?;
    Ok(UsageEventWrite::Inserted)
}

impl HubError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal-error",
            error.to_string(),
        )
    }

    fn unauthorized(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, message)
    }

    fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    fn unprocessable(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
    }
}

impl IntoResponse for HubError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

fn hash_password(password: &str) -> Result<String, HubError> {
    if password.len() < 8 {
        return Err(HubError::unprocessable(
            "weak-password",
            "password must be at least 8 characters long",
        ));
    }
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(HubError::internal)
}

fn verify_password(hash: &str, password: &str) -> Result<(), HubError> {
    let parsed = PasswordHash::new(hash).map_err(HubError::internal)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| HubError::unauthorized("owner-auth-required", "owner credentials are invalid"))
}

fn validate_time_zone(time_zone: &str) -> Result<String, HubError> {
    let time_zone = validate_non_empty(time_zone, "time_zone")?;
    time_zone.parse::<Tz>().map_err(|_| {
        HubError::unprocessable("invalid-time-zone", "time_zone must be a valid IANA time zone")
    })?;
    Ok(time_zone)
}

fn validate_identifier(value: &str, field: &str) -> Result<String, HubError> {
    let value = validate_non_empty(value, field)?;
    if value.len() > 200 {
        return Err(HubError::unprocessable(
            "invalid-identifier",
            format!("{field} is too long"),
        ));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '@'))
    {
        return Err(HubError::unprocessable(
            "invalid-identifier",
            format!("{field} contains unsupported characters"),
        ));
    }
    Ok(value)
}

fn validate_non_empty(value: &str, field: &str) -> Result<String, HubError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(HubError::unprocessable(
            "missing-field",
            format!("{field} must not be empty"),
        ));
    }
    Ok(trimmed.to_string())
}

fn validate_display_safe_key(value: &str, field: &str) -> Result<String, HubError> {
    let value = validate_non_empty(value, field)?;
    if looks_like_absolute_path(&value) {
        return Err(HubError::unprocessable(
            "absolute-path-forbidden",
            format!("{field} must be a redacted display-safe identifier, not an absolute path"),
        ));
    }
    Ok(value)
}

fn looks_like_absolute_path(value: &str) -> bool {
    if value.starts_with('/') || value.starts_with('~') {
        return true;
    }
    if value.starts_with("\\\\") {
        return true;
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return true;
    }
    let bytes = value.as_bytes();
    bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'/' || bytes[2] == b'\\')
}

fn normalize_utc_timestamp(raw: &str) -> Result<String, HubError> {
    parse_utc_timestamp(raw).map(|timestamp| timestamp.to_rfc3339())
}

fn parse_utc_timestamp(raw: &str) -> Result<DateTime<Utc>, HubError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| {
            HubError::unprocessable(
                "invalid-timestamp",
                "timestamps must be RFC3339 values that normalize to UTC",
            )
        })
}

fn now_utc() -> String {
    Utc::now().to_rfc3339()
}

fn plus_seconds(timestamp: &str, seconds: i64) -> Result<String, HubError> {
    let current = parse_utc_timestamp(timestamp)?;
    Ok((current + chrono::TimeDelta::seconds(seconds)).to_rfc3339())
}

fn random_token(bytes: usize) -> String {
    let mut raw = vec![0_u8; bytes];
    rand::thread_rng().fill_bytes(&mut raw);
    hex::encode(raw)
}

fn sha256_hex(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn sha256_json<T: Serialize>(value: &T) -> Result<String, HubError> {
    let serialized = serde_json::to_vec(value).map_err(HubError::internal)?;
    Ok(hex::encode(Sha256::digest(serialized)))
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?.to_str().ok().map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use serde_json::{json, Value};
    use tempfile::tempdir;
    use tower::util::ServiceExt;

    fn test_repo() -> HubRepository {
        let dir = tempdir().unwrap();
        let root = dir.keep();
        let db = Database::open(root.join("dirtydash.sqlite3")).unwrap();
        db.migrate().unwrap();
        HubRepository::new(db)
    }

    async fn json_response(response: Response) -> Value {
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap_or_else(|_| json!({}))
    }

    fn bootstrap_body() -> Value {
        json!({
            "username": "owner",
            "password": "correct horse battery staple",
            "time_zone": "America/Los_Angeles"
        })
    }

    async fn bootstrap_session(app: &Router) -> (String, String) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/admin/bootstrap")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(bootstrap_body().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let body = json_response(response).await;
        let csrf = body.get("csrf_token").unwrap().as_str().unwrap().to_string();
        (cookie, csrf)
    }

    async fn rotate_credential(app: &Router, cookie: &str, csrf: &str) -> RotateCollectorCredentialResponse {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/admin/collector-credentials/rotate")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, cookie)
                    .header(OWNER_CSRF_HEADER, csrf)
                    .body(Body::from(
                        json!({
                            "machine_id": "machine-a",
                            "display_name": "Machine A",
                            "credential_label": "default"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_value(json_response(response).await).unwrap()
    }

    fn ingest_request(protocol_version: u32) -> IngestBatchRequest {
        IngestBatchRequest {
            protocol_version,
            batch_id: "batch-1".to_string(),
            machine_id: "machine-a".to_string(),
            sync_run: SyncRunInput {
                sync_run_id: "sync-1".to_string(),
                collector_version: Some("collector-1.0.0".to_string()),
                started_at: "2026-03-09T09:59:00Z".to_string(),
                finished_at: "2026-03-09T10:01:00Z".to_string(),
            },
            source_manifests: vec![SourceManifestInput {
                source_key: "src-alpha".to_string(),
                agent: "codex".to_string(),
                display_path: "project-alpha/session-bucket".to_string(),
                item_count: 1,
                cursor: Some("cursor-1".to_string()),
                manifest_fingerprint: "manifest-1".to_string(),
            }],
            checkpoints: vec![CheckpointInput {
                agent: "codex".to_string(),
                checkpoint_key: "cursor".to_string(),
                checkpoint_value: "cursor-1".to_string(),
            }],
            events: vec![CollectorUsageEvent {
                agent: "codex".to_string(),
                collector_event_fingerprint: "fingerprint-1".to_string(),
                occurred_at: "2026-03-09T09:59:30Z".to_string(),
                session_key: "session-alpha".to_string(),
                project_key: "project-alpha".to_string(),
                source_key: "src-alpha".to_string(),
                turn_id: Some("turn-1".to_string()),
                provider: "openai-codex".to_string(),
                model: "gpt-5.5".to_string(),
                reasoning_effort: Some("low".to_string()),
                prompt_tokens: 100,
                completion_tokens: 50,
                cache_read_tokens: 10,
                cache_write_tokens: 0,
                reasoning_tokens: 5,
                total_tokens: 165,
                estimated_cost_usd: 0.0123,
                confidence: 0.9,
                parser_name: "codex".to_string(),
                parser_version: "v1".to_string(),
                pricing_version: "pricing-v1".to_string(),
                metadata_only: true,
            }],
        }
    }

    async fn ingest(app: &Router, token: &str, request: &IngestBatchRequest) -> Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/ingest/batches")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(serde_json::to_vec(request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn ingest_raw(app: &Router, token: &str, body: Value) -> Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/ingest/batches")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn bootstrap_login_and_csrf_protect_admin_routes() {
        let repo = test_repo();
        let app = build_router(repo, ListenerTrustMode::Public);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/admin/bootstrap")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(bootstrap_body().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().split(';').next().unwrap().to_string();
        let body = json_response(response).await;
        let csrf = body.get("csrf_token").unwrap().as_str().unwrap().to_string();

        let rejected = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/admin/collector-credentials/rotate")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(
                        json!({
                            "machine_id": "machine-a",
                            "display_name": "Machine A"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

        let allowed = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/admin/collector-credentials/rotate")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .header(OWNER_CSRF_HEADER, &csrf)
                    .body(Body::from(
                        json!({
                            "machine_id": "machine-a",
                            "display_name": "Machine A"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(allowed.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn public_listener_ignores_forged_tailscale_headers() {
        let repo = test_repo();
        let app = build_router(repo, ListenerTrustMode::Public);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/admin/session/tailscale")
                    .header(TAILSCALE_USER_LOGIN, "attacker@example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn private_listener_accepts_trusted_tailscale_headers() {
        let repo = test_repo();
        let app = build_router(repo, ListenerTrustMode::PrivateTailscale);
        let _ = bootstrap_session(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/admin/session/tailscale")
                    .header(TAILSCALE_USER_LOGIN, "owner@example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_response(response).await;
        assert_eq!(body.get("trusted_tailscale_user").unwrap(), "owner@example.com");
    }

    #[tokio::test]
    async fn collector_credential_rotation_and_revocation_work() {
        let repo = test_repo();
        let app = build_router(repo, ListenerTrustMode::Public);
        let (cookie, csrf) = bootstrap_session(&app).await;
        let first = rotate_credential(&app, &cookie, &csrf).await;
        let second = rotate_credential(&app, &cookie, &csrf).await;
        assert_ne!(first.credential_id, second.credential_id);

        let initial = ingest_request(SUPPORTED_PROTOCOL_VERSION);
        let first_rejected = ingest(&app, &first.token, &initial).await;
        assert_eq!(first_rejected.status(), StatusCode::UNAUTHORIZED);

        let second_ok = ingest(&app, &second.token, &initial).await;
        assert_eq!(second_ok.status(), StatusCode::OK);

        let revoke = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/admin/collector-credentials/revoke")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .header(OWNER_CSRF_HEADER, &csrf)
                    .body(Body::from(
                        json!({
                            "machine_id": "machine-a",
                            "credential_id": second.credential_id
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoke.status(), StatusCode::NO_CONTENT);

        let revoked = ingest(&app, &second.token, &initial).await;
        assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn incompatible_protocol_versions_fail_explicitly() {
        let repo = test_repo();
        let app = build_router(repo, ListenerTrustMode::Public);
        let (cookie, csrf) = bootstrap_session(&app).await;
        let issued = rotate_credential(&app, &cookie, &csrf).await;
        let response = ingest(&app, &issued.token, &ingest_request(2)).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = json_response(response).await;
        assert_eq!(body.get("code").unwrap(), "incompatible-protocol-version");

        let missing_version = ingest_raw(
            &app,
            &issued.token,
            json!({
                "batch_id": "batch-missing-version",
                "machine_id": "machine-a",
                "sync_run": {
                    "sync_run_id": "sync-missing-version",
                    "collector_version": "collector-1.0.0",
                    "started_at": "2026-03-09T09:59:00Z",
                    "finished_at": "2026-03-09T10:01:00Z"
                },
                "source_manifests": [],
                "checkpoints": [],
                "events": []
            }),
        )
        .await;
        assert_eq!(missing_version.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn duplicate_batches_and_retries_are_idempotent() {
        let repo = test_repo();
        let app = build_router(repo.clone(), ListenerTrustMode::Public);
        let (cookie, csrf) = bootstrap_session(&app).await;
        let issued = rotate_credential(&app, &cookie, &csrf).await;
        let request = ingest_request(SUPPORTED_PROTOCOL_VERSION);

        let first = ingest(&app, &issued.token, &request).await;
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = json_response(first).await;
        assert_eq!(first_body.get("inserted_events").unwrap(), 1);
        assert_eq!(first_body.get("idempotent_replay").unwrap(), false);

        let second = ingest(&app, &issued.token, &request).await;
        assert_eq!(second.status(), StatusCode::OK);
        let second_body = json_response(second).await;
        assert_eq!(second_body.get("idempotent_replay").unwrap(), true);
        assert_eq!(second_body.get("skipped_events").unwrap(), 1);

        let mut conflicting = ingest_request(SUPPORTED_PROTOCOL_VERSION);
        conflicting.events[0].total_tokens = 999;
        let conflict = ingest(&app, &issued.token, &conflicting).await;
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        let conflict_body = json_response(conflict).await;
        assert_eq!(conflict_body.get("code").unwrap(), "ingest-batch-conflict");

        let conn = repo.db.connection().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn partial_batch_failures_roll_back_everything() {
        let repo = test_repo();
        let app = build_router(repo.clone(), ListenerTrustMode::Public);
        let (cookie, csrf) = bootstrap_session(&app).await;
        let issued = rotate_credential(&app, &cookie, &csrf).await;
        let mut request = ingest_request(SUPPORTED_PROTOCOL_VERSION);
        request.events.push(CollectorUsageEvent {
            project_key: "/private/path".to_string(),
            collector_event_fingerprint: "fingerprint-2".to_string(),
            session_key: "session-beta".to_string(),
            source_key: "src-beta".to_string(),
            ..request.events[0].clone()
        });

        let response = ingest(&app, &issued.token, &request).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let false_metadata = ingest_raw(
            &app,
            &issued.token,
            json!({
                "protocol_version": 1,
                "batch_id": "batch-false-metadata",
                "machine_id": "machine-a",
                "sync_run": {
                    "sync_run_id": "sync-false-metadata",
                    "collector_version": "collector-1.0.0",
                    "started_at": "2026-03-09T09:59:00Z",
                    "finished_at": "2026-03-09T10:01:00Z"
                },
                "source_manifests": [],
                "checkpoints": [],
                "events": [{
                    "agent": "codex",
                    "collector_event_fingerprint": "fingerprint-false-metadata",
                    "occurred_at": "2026-03-09T09:59:30Z",
                    "session_key": "session-alpha",
                    "project_key": "project-alpha",
                    "source_key": "src-alpha",
                    "provider": "openai-codex",
                    "model": "gpt-5.5",
                    "reasoning_effort": "low",
                    "prompt_tokens": 100,
                    "completion_tokens": 50,
                    "cache_read_tokens": 10,
                    "cache_write_tokens": 0,
                    "reasoning_tokens": 5,
                    "total_tokens": 165,
                    "estimated_cost_usd": 0.0123,
                    "confidence": 0.9,
                    "parser_name": "codex",
                    "parser_version": "v1",
                    "pricing_version": "pricing-v1",
                    "metadata_only": false,
                    "raw_prompt": "forbidden"
                }]
            }),
        )
        .await;
        assert_eq!(false_metadata.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let conn = repo.db.connection().unwrap();
        let usage_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_events", [], |row| row.get(0))
            .unwrap();
        let batch_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ingest_batches", [], |row| row.get(0))
            .unwrap();
        assert_eq!(usage_count, 0);
        assert_eq!(batch_count, 0);
    }

    #[test]
    fn migration_upgrades_existing_v1_schema_additively() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("dirtydash.sqlite3")).unwrap();
        let conn = db.connection().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );
            CREATE TABLE usage_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                machine TEXT NOT NULL,
                source TEXT NOT NULL,
                project_path TEXT NOT NULL,
                session_id TEXT NOT NULL,
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost_usd REAL NOT NULL DEFAULT 0,
                confidence REAL NOT NULL DEFAULT 0,
                event_timestamp TEXT,
                raw_path TEXT NOT NULL,
                raw_span TEXT,
                parser_name TEXT NOT NULL,
                parser_version TEXT NOT NULL,
                raw_event_hash TEXT NOT NULL UNIQUE,
                imported_at TEXT NOT NULL,
                pricing_version TEXT NOT NULL,
                metadata_only INTEGER NOT NULL DEFAULT 1
            );
            INSERT INTO usage_events(
                machine, source, project_path, session_id, provider, model, prompt_tokens,
                completion_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens,
                total_tokens, estimated_cost_usd, confidence, event_timestamp, raw_path,
                raw_span, parser_name, parser_version, raw_event_hash, imported_at, pricing_version, metadata_only
            ) VALUES (
                'legacy-machine', 'codex', 'project', 'session', 'openai-codex', 'gpt-5.5', 10,
                2, 0, 0, 0, 12, 0.5, 0.8, '2026-03-09T09:59:30Z', 'source',
                NULL, 'codex', 'v1', 'legacy-hash', '2026-03-09T10:00:00Z', 'pricing-v1', 1
            );
            "#,
        )
        .unwrap();
        drop(conn);

        db.migrate().unwrap();
        let conn = db.connection().unwrap();
        let (machine_id, agent, fingerprint): (String, String, String) = conn
            .query_row(
                "SELECT machine_id, agent, collector_event_fingerprint FROM usage_events WHERE raw_event_hash = 'legacy-hash'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(machine_id, "legacy-machine");
        assert_eq!(agent, "codex");
        assert_eq!(fingerprint, "legacy-hash");

        let owner_sessions_exists: String = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'owner_sessions'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owner_sessions_exists, "owner_sessions");
    }

    #[test]
    fn owner_time_zone_aggregation_rebuckets_midnight_boundaries() {
        let repo = test_repo();
        let conn = repo.db.connection().unwrap();
        for (timestamp, fingerprint, tokens, cost) in [
            ("2026-01-15T07:59:00Z", "midnight-a", 100_i64, 1.0_f64),
            ("2026-01-15T08:01:00Z", "midnight-b", 200_i64, 2.0_f64),
        ] {
            conn.execute(
                r#"
                INSERT INTO usage_events(
                    machine, source, project_path, session_id, turn_id, provider, model,
                    reasoning_effort, prompt_tokens, completion_tokens, cache_read_tokens,
                    cache_write_tokens, reasoning_tokens, total_tokens, estimated_cost_usd,
                    confidence, event_timestamp, raw_path, raw_span, parser_name, parser_version,
                    raw_event_hash, imported_at, pricing_version, pricing_mode, metadata_only,
                    machine_id, agent, collector_event_fingerprint, ingest_batch_id
                ) VALUES (
                    'machine-a', 'codex', 'project-a', ?1, NULL, 'openai-codex', 'gpt-5.5',
                    NULL, 0, 0, 0, 0, 0, ?2, ?3,
                    0.9, ?4, 'source-a', NULL, 'codex', 'v1',
                    ?5, ?4, 'pricing-v1', 'unpriced', 1,
                    'machine-a', 'codex', ?5, 'batch-midnight'
                )
                "#,
                params![format!("session-{fingerprint}"), tokens, cost, timestamp, fingerprint],
            )
            .unwrap();
        }
        drop(conn);

        let buckets = repo
            .usage_by_day_in_owner_time_zone("America/Los_Angeles")
            .unwrap();
        assert_eq!(
            buckets,
            vec![
                UsageDayBucket {
                    day: "2026-01-14".to_string(),
                    total_tokens: 100,
                    estimated_cost_usd: 1.0,
                },
                UsageDayBucket {
                    day: "2026-01-15".to_string(),
                    total_tokens: 200,
                    estimated_cost_usd: 2.0,
                }
            ]
        );
    }

    #[test]
    fn owner_time_zone_aggregation_rebuckets_dst_gap_boundaries() {
        let repo = test_repo();
        let conn = repo.db.connection().unwrap();
        for (timestamp, fingerprint, tokens, cost) in [
            ("2026-03-08T09:59:00Z", "gap-a", 100_i64, 1.0_f64),
            ("2026-03-08T10:01:00Z", "gap-b", 200_i64, 2.0_f64),
        ] {
            conn.execute(
                r#"
                INSERT INTO usage_events(
                    machine, source, project_path, session_id, turn_id, provider, model,
                    reasoning_effort, prompt_tokens, completion_tokens, cache_read_tokens,
                    cache_write_tokens, reasoning_tokens, total_tokens, estimated_cost_usd,
                    confidence, event_timestamp, raw_path, raw_span, parser_name, parser_version,
                    raw_event_hash, imported_at, pricing_version, pricing_mode, metadata_only,
                    machine_id, agent, collector_event_fingerprint, ingest_batch_id
                ) VALUES (
                    'machine-a', 'codex', 'project-a', ?1, NULL, 'openai-codex', 'gpt-5.5',
                    NULL, 0, 0, 0, 0, 0, ?2, ?3,
                    0.9, ?4, 'source-a', NULL, 'codex', 'v1',
                    ?5, ?4, 'pricing-v1', 'unpriced', 1,
                    'machine-a', 'codex', ?5, 'batch-gap'
                )
                "#,
                params![format!("session-{fingerprint}"), tokens, cost, timestamp, fingerprint],
            )
            .unwrap();
        }
        drop(conn);

        let buckets = repo
            .usage_by_day_in_owner_time_zone("America/Los_Angeles")
            .unwrap();
        assert_eq!(
            buckets,
            vec![UsageDayBucket {
                day: "2026-03-08".to_string(),
                total_tokens: 300,
                estimated_cost_usd: 3.0,
            }]
        );
    }

    #[test]
    fn owner_time_zone_aggregation_handles_dst_folds_without_double_counting() {
        let repo = test_repo();
        let conn = repo.db.connection().unwrap();
        for (timestamp, fingerprint, tokens, cost) in [
            ("2026-11-01T08:30:00Z", "fold-a", 111_i64, 1.11_f64),
            ("2026-11-01T09:30:00Z", "fold-b", 222_i64, 2.22_f64),
        ] {
            conn.execute(
                r#"
                INSERT INTO usage_events(
                    machine, source, project_path, session_id, turn_id, provider, model,
                    reasoning_effort, prompt_tokens, completion_tokens, cache_read_tokens,
                    cache_write_tokens, reasoning_tokens, total_tokens, estimated_cost_usd,
                    confidence, event_timestamp, raw_path, raw_span, parser_name, parser_version,
                    raw_event_hash, imported_at, pricing_version, pricing_mode, metadata_only,
                    machine_id, agent, collector_event_fingerprint, ingest_batch_id
                ) VALUES (
                    'machine-b', 'codex', 'project-b', ?1, NULL, 'openai-codex', 'gpt-5.5',
                    NULL, 0, 0, 0, 0, 0, ?2, ?3,
                    0.9, ?4, 'source-b', NULL, 'codex', 'v1',
                    ?5, ?4, 'pricing-v1', 'unpriced', 1,
                    'machine-b', 'codex', ?5, 'batch-fold'
                )
                "#,
                params![format!("session-{fingerprint}"), tokens, cost, timestamp, fingerprint],
            )
            .unwrap();
        }
        drop(conn);

        let buckets = repo
            .usage_by_day_in_owner_time_zone("America/Los_Angeles")
            .unwrap();
        assert_eq!(
            buckets,
            vec![UsageDayBucket {
                day: "2026-11-01".to_string(),
                total_tokens: 333,
                estimated_cost_usd: 3.33,
            }]
        );
    }

    #[test]
    fn concurrent_collectors_share_wal_database_without_duplicates() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("dirtydash.sqlite3")).unwrap();
        db.migrate().unwrap();
        let base_repo = HubRepository::new(db.clone());
        let repo_a = base_repo.clone();
        let repo_b = base_repo.clone();
        repo_a
            .bootstrap_owner(BootstrapOwnerRequest {
                username: "owner".to_string(),
                password: "correct horse battery staple".to_string(),
                time_zone: "UTC".to_string(),
            })
            .unwrap();
        let issued = repo_a
            .rotate_collector_credential(RotateCollectorCredentialRequest {
                machine_id: "machine-a".to_string(),
                display_name: "Machine A".to_string(),
                credential_label: "default".to_string(),
            })
            .unwrap();
        let auth_a = repo_a.authenticate_collector_bearer(&issued.token).unwrap();
        let auth_b = repo_b.authenticate_collector_bearer(&issued.token).unwrap();

        let batch_a = ingest_request(SUPPORTED_PROTOCOL_VERSION);
        let mut batch_b = ingest_request(SUPPORTED_PROTOCOL_VERSION);
        batch_b.batch_id = "batch-2".to_string();
        batch_b.events[0].collector_event_fingerprint = "fingerprint-2".to_string();
        batch_b.events[0].session_key = "session-beta".to_string();

        let handle_a = std::thread::spawn(move || repo_a.ingest_batch(&auth_a, batch_a));
        let handle_b = std::thread::spawn(move || repo_b.ingest_batch(&auth_b, batch_b));
        let response_a = handle_a.join().unwrap().unwrap();
        let response_b = handle_b.join().unwrap().unwrap();
        assert_eq!(response_a.inserted_events, 1);
        assert_eq!(response_b.inserted_events, 1);

        let conn = db.connection().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn fleet_identity_distinguishes_machine_and_agent_dimensions() {
        let repo = test_repo();
        let _session = repo
            .bootstrap_owner(BootstrapOwnerRequest {
                username: "owner".to_string(),
                password: "correct horse battery staple".to_string(),
                time_zone: "UTC".to_string(),
            })
            .unwrap();
        let machine_a = repo
            .rotate_collector_credential(RotateCollectorCredentialRequest {
                machine_id: "machine-a".to_string(),
                display_name: "Machine A".to_string(),
                credential_label: "default".to_string(),
            })
            .unwrap();
        let machine_b = repo
            .rotate_collector_credential(RotateCollectorCredentialRequest {
                machine_id: "machine-b".to_string(),
                display_name: "Machine B".to_string(),
                credential_label: "default".to_string(),
            })
            .unwrap();
        let auth_a = repo.authenticate_collector_bearer(&machine_a.token).unwrap();
        let auth_b = repo.authenticate_collector_bearer(&machine_b.token).unwrap();

        repo.ingest_batch(&auth_a, ingest_request(1)).unwrap();

        let mut machine_variant = ingest_request(1);
        machine_variant.batch_id = "batch-machine-b".to_string();
        machine_variant.machine_id = "machine-b".to_string();
        machine_variant.sync_run.sync_run_id = "sync-machine-b".to_string();
        repo.ingest_batch(&auth_b, machine_variant).unwrap();

        let mut agent_variant = ingest_request(1);
        agent_variant.batch_id = "batch-agent-variant".to_string();
        agent_variant.sync_run.sync_run_id = "sync-agent-variant".to_string();
        agent_variant.events[0].agent = "claude-code".to_string();
        repo.ingest_batch(&auth_a, agent_variant).unwrap();

        let conn = repo.db.connection().unwrap();
        let identities = conn
            .prepare(
                "SELECT machine_id, agent, collector_event_fingerprint FROM usage_events ORDER BY machine_id, agent",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            identities,
            vec![
                (
                    "machine-a".to_string(),
                    "claude-code".to_string(),
                    "fingerprint-1".to_string(),
                ),
                (
                    "machine-a".to_string(),
                    "codex".to_string(),
                    "fingerprint-1".to_string(),
                ),
                (
                    "machine-b".to_string(),
                    "codex".to_string(),
                    "fingerprint-1".to_string(),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn loopback_server_contract_stays_unchanged() {
        let repo = test_repo();
        let app = build_router(repo, ListenerTrustMode::Public);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/admin/session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_response(response).await;
        assert_eq!(body.get("authenticated").unwrap(), false);
    }
}
