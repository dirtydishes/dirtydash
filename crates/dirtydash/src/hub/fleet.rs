//! Durable fleet lifecycle, health, and staged update seams.
//!
//! This module is deliberately additive to the Phase 2-4 Hub tables and
//! command protocol.  A Machine row is the lifecycle root; usage, syncs,
//! credentials, commands, and update attempts remain separate records so
//! archiving never erases history and one failed Collector cannot fail a run.

use super::*;
use anyhow::bail;

type HubResult<T> = std::result::Result<T, HubError>;
type AnyResult<T> = anyhow::Result<T>;
type FleetTargetRow = (
    String,
    Option<String>,
    Option<u32>,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
);
type FleetMachineStateRow = (Option<String>, Option<String>, Option<u32>, i64);
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Current Collector wire protocol accepted by the Hub.
pub const CURRENT_COLLECTOR_PROTOCOL_VERSION: u32 = API_V1_PROTOCOL_VERSION;
/// Previous Collector wire protocol retained during staged fleet updates.
pub const PREVIOUS_COLLECTOR_PROTOCOL_VERSION: u32 = API_PREVIOUS_PROTOCOL_VERSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MachineLifecycle {
    Active,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MachineHealth {
    Online,
    Syncing,
    Stale,
    Offline,
    UpdateAvailable,
    ActionRequired,
    Archived,
}

/// Compatibility is intentionally a closed vocabulary.  Unknown protocol
/// values are never silently treated as the current version.
pub type MachineStatus = MachineHealth;
pub type CollectorCompatibility = ProtocolCompatibility;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProtocolCompatibility {
    Current,
    Previous,
    Unsupported,
    Unknown,
}

impl ProtocolCompatibility {
    pub fn for_version(version: Option<u32>) -> Self {
        match version {
            Some(CURRENT_COLLECTOR_PROTOCOL_VERSION) => Self::Current,
            Some(PREVIOUS_COLLECTOR_PROTOCOL_VERSION) => Self::Previous,
            Some(_) => Self::Unsupported,
            None => Self::Unknown,
        }
    }

    pub fn is_supported(self) -> bool {
        matches!(self, Self::Current | Self::Previous)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MachineDiagnostics {
    pub watcher_degraded: bool,
    pub credential_rotation_pending: bool,
    pub terminal_outbox: u64,
    pub pending_outbox: u64,
    pub last_reconciliation_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MachineRecord {
    pub machine_id: String,
    pub display_name: String,
    pub lifecycle: MachineLifecycle,
    pub status: MachineHealth,
    pub status_reason: String,
    pub enrolled_at: String,
    pub archived_at: Option<String>,
    pub last_seen_at: Option<String>,
    pub last_sync_at: Option<String>,
    pub collector_version: Option<String>,
    pub desired_version: Option<String>,
    pub collector_protocol_version: Option<u32>,
    pub protocol_compatibility: ProtocolCompatibility,
    pub diagnostics_status: Option<String>,
    pub diagnostics_at: Option<String>,
    pub diagnostics: Option<MachineDiagnostics>,
    pub credentials_active: u64,
    pub credentials_total: u64,
    pub pending_action: Option<String>,
    pub usage_event_count: u64,
    pub state_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MachineActionResponse {
    pub machine_id: String,
    pub command_id: String,
    pub action: String,
    pub state_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MachineLifecycleRequest {
    pub expected_state_revision: i64,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PermanentDeleteMachineRequest {
    pub expected_state_revision: i64,
    pub display_name: String,
    /// The UI renders `DELETE <display_name>` and requires exact entry.
    pub confirmation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FleetUpdateRequest {
    pub version: String,
    pub artifact_sha256: String,
    pub publisher_key_id: String,
    pub publisher_fingerprint: String,
    pub manifest_sha256: String,
    /// The signed manifest is verified at the owner API boundary before this
    /// request is persisted. Keeping it in the request also makes a plan
    /// auditable without persisting artifact bytes.
    #[serde(default)]
    pub signed_manifest: Option<crate::deployment::SignedArtifactManifest>,
    #[serde(default)]
    pub machine_ids: Vec<String>,
}

impl FleetUpdateRequest {
    pub fn from_verified_artifact(
        artifact: &crate::deployment::VerifiedArtifact,
        signed_manifest: crate::deployment::SignedArtifactManifest,
        machine_ids: Vec<String>,
    ) -> Self {
        let evidence = FleetUpdateEvidence::from_verified_artifact(artifact);
        Self {
            version: evidence.version,
            artifact_sha256: evidence.artifact_sha256,
            publisher_key_id: evidence.publisher_key_id,
            publisher_fingerprint: evidence.publisher_fingerprint,
            manifest_sha256: evidence.manifest_sha256,
            signed_manifest: Some(signed_manifest),
            machine_ids,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FleetUpdateEvidence {
    pub version: String,
    pub artifact_sha256: String,
    pub publisher_key_id: String,
    pub publisher_fingerprint: String,
    pub manifest_sha256: String,
    /// Set only by the server after the configured publisher policy verifies
    /// the signed manifest. This field is never accepted as request input.
    #[serde(default, skip_deserializing)]
    pub publisher_verified: bool,
}

impl FleetUpdateEvidence {
    /// Build update evidence only from Deployment's already verified artifact;
    /// callers should not hand-author the `publisher_verified` assertion.
    pub fn from_verified_artifact(artifact: &crate::deployment::VerifiedArtifact) -> Self {
        Self {
            version: artifact.manifest().manifest().release.clone(),
            artifact_sha256: artifact.descriptor().sha256.clone(),
            publisher_key_id: artifact.manifest().key_id().to_string(),
            publisher_fingerprint: artifact.manifest().publisher_fingerprint().to_string(),
            manifest_sha256: artifact.manifest().manifest_sha256().to_string(),
            publisher_verified: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetUpdateRun {
    pub update_id: String,
    pub version: String,
    pub artifact_sha256: String,
    pub publisher_key_id: String,
    pub publisher_fingerprint: String,
    pub manifest_sha256: String,
    pub status: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub hub_snapshot_at: Option<String>,
    pub hub_restart_requested_at: Option<String>,
    pub hub_updated_at: Option<String>,
    pub hub_health_at: Option<String>,
    pub completed_at: Option<String>,
    pub failure_reason: Option<String>,
    pub attempts: i64,
    pub state_revision: i64,
    pub nodes: Vec<FleetUpdateNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetUpdateNode {
    pub update_id: String,
    pub machine_id: String,
    pub status: String,
    pub previous_version: Option<String>,
    pub previous_desired_version: Option<String>,
    pub previous_runtime_generation: Option<String>,
    pub snapshot_at: Option<String>,
    pub update_started_at: Option<String>,
    pub restarted_at: Option<String>,
    pub health_checked_at: Option<String>,
    pub rolled_back_at: Option<String>,
    pub failure_reason: Option<String>,
    pub collector_protocol_version: Option<u32>,
    pub receipt: Option<CollectorUpdateReceipt>,
    pub attempts: i64,
    pub state_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FleetUpdateNodeCompletion {
    pub expected_state_revision: i64,
    pub collector_version: String,
    pub protocol_version: u32,
    pub restarted: bool,
    pub health_checked: bool,
    pub signed_evidence: FleetUpdateEvidence,
    #[serde(default)]
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FleetHubHealthRequest {
    pub expected_state_revision: i64,
    pub healthy: bool,
    pub restarted: bool,
    pub health_checked: bool,
    pub hub_version: String,
    pub evidence: FleetUpdateEvidence,
    #[serde(default)]
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetUpdatePlanResponse {
    pub update: FleetUpdateRun,
    pub ordered_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetUpdateSnapshotResponse {
    pub update_id: String,
    pub status: String,
    pub state_revision: i64,
    pub hub_snapshot_at: String,
}

/// Inputs used by [`derive_machine_health`].  Keeping this pure makes status
/// semantics testable without a clock or database fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineHealthInput<'a> {
    pub archived: bool,
    pub last_seen_at: Option<&'a str>,
    pub last_sync_at: Option<&'a str>,
    pub pending_action: bool,
    pub diagnostics_action_required: bool,
    pub credentials_active: u64,
    pub protocol: ProtocolCompatibility,
    pub desired_version: Option<&'a str>,
    pub current_version: Option<&'a str>,
}

/// Derive a display state from observable evidence, never from one persisted
/// opaque enum.  Thresholds are intentionally conservative: a sync newer
/// than the observation window can still be shown as syncing when a command
/// is pending, while protocol/diagnostic/credential failures take priority.
pub fn derive_machine_health(input: MachineHealthInput<'_>, now: DateTime<Utc>) -> MachineHealth {
    if input.archived {
        return MachineHealth::Archived;
    }
    if input.diagnostics_action_required
        || input.credentials_active == 0
        || !input.protocol.is_supported()
    {
        return MachineHealth::ActionRequired;
    }
    if versions_differ(input.desired_version, input.current_version) {
        return MachineHealth::UpdateAvailable;
    }
    if input.pending_action {
        return MachineHealth::Syncing;
    }
    let Some(last_seen) = input
        .last_seen_at
        .or(input.last_sync_at)
        .and_then(parse_timestamp)
    else {
        return MachineHealth::Offline;
    };
    let age = now.signed_duration_since(last_seen);
    if age > Duration::hours(1) {
        MachineHealth::Offline
    } else if age > Duration::minutes(5) {
        MachineHealth::Stale
    } else {
        MachineHealth::Online
    }
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn versions_differ(desired: Option<&str>, current: Option<&str>) -> bool {
    match (desired, current) {
        (Some(desired), Some(current)) => desired != current,
        (Some(_), None) => true,
        _ => false,
    }
}

fn version_is_safe(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
}

fn digest_is_safe(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|character| character.is_ascii_hexdigit())
}

fn update_id() -> String {
    format!("update-{}", random_token(12))
}

fn command_id(action: &str) -> String {
    format!("fleet-{action}-{}", random_token(12))
}

pub(crate) fn fleet_update_command_id(update_id: &str, machine_id: &str) -> String {
    let digest = sha256_hex(&format!("{update_id}:{machine_id}"));
    format!("fleet-update-{}", &digest[..24])
}

pub(crate) fn fleet_rollback_command_id(update_id: &str, machine_id: &str) -> String {
    let digest = sha256_hex(&format!("rollback:{update_id}:{machine_id}"));
    format!("fleet-rollback-{}", &digest[..24])
}

const COLLECTOR_UPDATE_TIMEOUT: Duration = Duration::minutes(5);
const HUB_UPDATE_TIMEOUT: Duration = Duration::minutes(5);
const MAX_FLEET_UPDATE_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;

impl HubRepository {
    pub(crate) fn list_machines(&self) -> HubResult<Vec<MachineRecord>> {
        let conn = self.db.connection().map_err(HubError::internal)?;
        let mut statement = conn
            .prepare(
                r#"
                SELECT m.machine_id, m.display_name, m.enrolled_at, m.revoked_at,
                    m.last_seen_at, m.archived_at, m.desired_version,
                    COALESCE(m.collector_version, (
                        SELECT s.collector_version FROM sync_runs s
                        WHERE s.machine_id = m.machine_id
                        ORDER BY s.finished_at DESC LIMIT 1
                    )),
                    COALESCE(m.collector_protocol_version, (
                        SELECT i.protocol_version FROM ingest_batches i
                        WHERE i.machine_id = m.machine_id
                        ORDER BY i.committed_at DESC LIMIT 1
                    )),
                    m.last_sync_at,
                    m.diagnostics_json, m.diagnostics_status, m.diagnostics_at,
                    m.state_revision,
                    (SELECT COUNT(*) FROM collector_credentials c
                        WHERE c.machine_id = m.machine_id AND c.revoked_at IS NULL),
                    (SELECT COUNT(*) FROM collector_credentials c
                        WHERE c.machine_id = m.machine_id),
                    (SELECT c.command_json FROM collector_commands c
                        WHERE c.machine_id = m.machine_id AND c.acknowledged_at IS NULL
                        ORDER BY c.created_at LIMIT 1),
                    (SELECT COUNT(*) FROM usage_events e WHERE e.machine_id = m.machine_id)
                FROM machines m
                ORDER BY m.archived_at IS NOT NULL, m.display_name, m.machine_id
                "#,
            )
            .map_err(HubError::internal)?;
        let raw = statement
            .query_map([], machine_row)
            .map_err(HubError::internal)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(HubError::internal)?;
        let now = Utc::now();
        Ok(raw.into_iter().map(|row| row.into_record(now)).collect())
    }

    pub(crate) fn machine(&self, machine_id: &str) -> HubResult<MachineRecord> {
        let machine_id = validate_identifier(machine_id, "machine_id")?;
        self.list_machines()?
            .into_iter()
            .find(|machine| machine.machine_id == machine_id)
            .ok_or_else(|| HubError::not_found("machine-not-found", "Machine was not found"))
    }

    pub(crate) fn archive_machine(
        &self,
        machine_id: &str,
        request: MachineLifecycleRequest,
    ) -> HubResult<MachineRecord> {
        self.transition_machine(machine_id, request, false)
    }

    pub(crate) fn remove_machine(
        &self,
        machine_id: &str,
        request: MachineLifecycleRequest,
    ) -> HubResult<MachineRecord> {
        // Remove is the reversible administrative action: revoke the Collector
        // and retain both credentials and history in an archived Machine root.
        self.transition_machine(machine_id, request, false)
    }

    fn transition_machine(
        &self,
        machine_id: &str,
        request: MachineLifecycleRequest,
        _permanent: bool,
    ) -> HubResult<MachineRecord> {
        let machine_id = validate_identifier(machine_id, "machine_id")?;
        let display_name = validate_non_empty(&request.display_name, "display_name")?;
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let row: Option<(String, Option<String>, Option<String>, i64)> = tx
            .query_row(
                "SELECT display_name, revoked_at, archived_at, state_revision FROM machines WHERE machine_id = ?1",
                params![machine_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some((stored_name, _, _, revision)) = row else {
            return Err(HubError::not_found(
                "machine-not-found",
                "Machine was not found",
            ));
        };
        if stored_name != display_name || revision != request.expected_state_revision {
            return Err(HubError::conflict(
                "machine-state-conflict",
                "Machine name or state revision changed; reload before retrying",
            ));
        }
        self.reject_active_machine_update(&tx, &machine_id)?;
        let changed = tx
            .execute(
                r#"
                UPDATE machines
                SET revoked_at = COALESCE(revoked_at, ?2),
                    archived_at = COALESCE(archived_at, ?2),
                    state_revision = state_revision + 1
                WHERE machine_id = ?1 AND display_name = ?3 AND state_revision = ?4
                "#,
                params![
                    machine_id,
                    now,
                    display_name,
                    request.expected_state_revision
                ],
            )
            .map_err(HubError::internal)?;
        if changed == 0 {
            return Err(HubError::conflict(
                "machine-state-conflict",
                "Machine changed during archive; reload before retrying",
            ));
        }
        tx.execute(
            "UPDATE collector_credentials SET revoked_at = COALESCE(revoked_at, ?2) WHERE machine_id = ?1 AND revoked_at IS NULL",
            params![machine_id, now],
        )
        .map_err(HubError::internal)?;
        tx.commit().map_err(HubError::internal)?;
        self.machine(&machine_id)
    }

    pub(crate) fn permanent_delete_machine(
        &self,
        machine_id: &str,
        request: PermanentDeleteMachineRequest,
    ) -> HubResult<()> {
        let machine_id = validate_identifier(machine_id, "machine_id")?;
        let expected = format!("DELETE {}", request.display_name);
        if request.confirmation != expected {
            return Err(HubError::unprocessable(
                "typed-confirmation-required",
                format!("type {expected} exactly to permanently delete this Machine"),
            ));
        }
        let display_name = validate_non_empty(&request.display_name, "display_name")?;
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let row: Option<(String, Option<String>, i64)> = tx
            .query_row(
                "SELECT display_name, archived_at, state_revision FROM machines WHERE machine_id = ?1",
                params![machine_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some((stored_name, archived_at, revision)) = row else {
            return Err(HubError::not_found(
                "machine-not-found",
                "Machine was not found",
            ));
        };
        if stored_name != display_name
            || archived_at.is_none()
            || revision != request.expected_state_revision
        {
            return Err(HubError::conflict(
                "machine-state-conflict",
                "Machine must be archived and unchanged before permanent deletion",
            ));
        }
        self.reject_active_machine_update(&tx, &machine_id)?;
        let audit_rows: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM fleet_update_nodes WHERE machine_id = ?1",
                params![machine_id],
                |row| row.get(0),
            )
            .map_err(HubError::internal)?;
        if audit_rows > 0 {
            return Err(HubError::conflict(
                "machine-update-audit-retained",
                "Machine update history must be retained; archive the Machine instead of deleting it",
            ));
        }
        let enrollment_rows: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM enrollment_credentials WHERE machine_id = ?1",
                params![machine_id],
                |row| row.get(0),
            )
            .map_err(HubError::internal)?;
        if enrollment_rows > 0 {
            return Err(HubError::conflict(
                "machine-enrollment-audit-retained",
                "Machine enrollment credential history must be retained; archive the Machine instead of deleting it",
            ));
        }
        // Revoke inside the same immediate transaction before the cascading
        // delete.  This closes the authenticate/archive/delete race and makes
        // the audit intent explicit even when foreign-key cleanup follows.
        tx.execute(
            "UPDATE collector_credentials SET revoked_at = COALESCE(revoked_at, ?2) WHERE machine_id = ?1",
            params![machine_id, now],
        )
        .map_err(HubError::internal)?;
        let changed = tx
            .execute(
                "UPDATE machines SET revoked_at = COALESCE(revoked_at, ?2), state_revision = state_revision + 1 WHERE machine_id = ?1 AND display_name = ?3 AND state_revision = ?4",
                params![machine_id, now, display_name, request.expected_state_revision],
            )
            .map_err(HubError::internal)?;
        if changed == 0 {
            return Err(HubError::conflict(
                "machine-state-conflict",
                "Machine changed during permanent deletion; reload before retrying",
            ));
        }
        tx.execute(
            "DELETE FROM usage_events WHERE machine_id = ?1",
            params![machine_id],
        )
        .map_err(HubError::internal)?;
        tx.execute(
            "DELETE FROM machines WHERE machine_id = ?1 AND display_name = ?2",
            params![machine_id, display_name],
        )
        .map_err(HubError::internal)?;
        tx.commit().map_err(HubError::internal)
    }

    pub(crate) fn reject_active_machine_update(
        &self,
        tx: &rusqlite::Transaction<'_>,
        machine_id: &str,
    ) -> HubResult<()> {
        let active: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM fleet_update_nodes n JOIN fleet_update_runs r ON r.update_id = n.update_id WHERE n.machine_id = ?1 AND n.status IN ('queued', 'updating', 'rolling-back') AND r.status NOT IN ('completed', 'completed-with-failures', 'failed'))",
                params![machine_id],
                |row| row.get(0),
            )
            .map_err(HubError::internal)?;
        if active {
            return Err(HubError::conflict(
                "machine-update-active",
                "Machine lifecycle, repair, rotation, and deletion actions are blocked while an update is active",
            ));
        }
        Ok(())
    }

    pub(crate) fn record_collector_diagnostics(
        &self,
        machine_id: &str,
        diagnostics: &CollectorDiagnosticsReceipt,
    ) -> HubResult<()> {
        let machine_id = validate_identifier(machine_id, "machine_id")?;
        if diagnostics.machine_id != machine_id {
            return Err(HubError::unauthorized(
                "collector-machine-mismatch",
                "diagnostics receipt does not belong to the authenticated Machine",
            ));
        }
        CollectorCommandResult::Diagnostics {
            diagnostics: diagnostics.clone(),
        }
        .validate()?;
        let diagnostics_json = serde_json::to_string(diagnostics).map_err(HubError::internal)?;
        let status = if diagnostics.watcher.degraded
            || diagnostics.credential_rotation_pending
            || diagnostics.terminal_outbox > 0
        {
            "degraded"
        } else {
            "healthy"
        };
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let conn = self.db.connection().map_err(HubError::internal)?;
        let changed = conn
            .execute(
                "UPDATE machines SET diagnostics_json = ?2, diagnostics_status = ?3, diagnostics_at = ?4, last_seen_at = ?4, state_revision = state_revision + 1 WHERE machine_id = ?1 AND archived_at IS NULL AND revoked_at IS NULL",
                params![machine_id, diagnostics_json, status, now],
            )
            .map_err(HubError::internal)?;
        if changed == 0 {
            return Err(HubError::not_found(
                "machine-not-found",
                "diagnostics target Machine was not found",
            ));
        }
        Ok(())
    }

    pub(crate) fn queue_machine_action(
        &self,
        machine_id: &str,
        action: &str,
        expected_state_revision: i64,
    ) -> HubResult<MachineActionResponse> {
        let machine_id = validate_identifier(machine_id, "machine_id")?;
        let (command, action_name) = match action {
            "refresh" => (
                OwnerCommand::Refresh {
                    command_id: command_id("refresh"),
                },
                "refresh",
            ),
            "repair" => (
                OwnerCommand::Repair {
                    command_id: command_id("repair"),
                },
                "repair",
            ),
            "rotate" => (
                OwnerCommand::RotateCredential {
                    command_id: command_id("rotate"),
                    rotation_id: format!("rotation-{}", random_token(12)),
                },
                "rotate",
            ),
            "diagnostics" => (
                OwnerCommand::Diagnostics {
                    command_id: command_id("diagnostics"),
                },
                "diagnostics",
            ),
            _ => {
                return Err(HubError::unprocessable(
                    "invalid-machine-action",
                    "unknown Machine action",
                ))
            }
        };
        let response = self.issue_collector_command(IssueCollectorCommandRequest {
            machine_id,
            command: command.clone(),
            expected_state_revision: Some(expected_state_revision),
        })?;
        let response_machine_id = response.machine_id.clone();
        Ok(MachineActionResponse {
            machine_id: response_machine_id.clone(),
            command_id: response.command_id,
            action: action_name.to_string(),
            state_revision: self.machine(&response_machine_id)?.state_revision,
        })
    }

    pub(crate) fn create_fleet_update(
        &self,
        request: FleetUpdateRequest,
    ) -> HubResult<FleetUpdatePlanResponse> {
        validate_update_request(&request)?;
        let mut machine_ids = request.machine_ids.clone();
        if machine_ids.is_empty() {
            machine_ids = self
                .list_machines()?
                .into_iter()
                .filter(|machine| matches!(machine.lifecycle, MachineLifecycle::Active))
                .map(|machine| machine.machine_id)
                .collect();
        }
        if machine_ids.is_empty() {
            return Err(HubError::unprocessable(
                "empty-update-target",
                "fleet update requires at least one active Machine target",
            ));
        }
        let unique = machine_ids.iter().collect::<BTreeSet<_>>();
        if unique.len() != machine_ids.len() {
            return Err(HubError::unprocessable(
                "duplicate-machine",
                "update targets must be unique",
            ));
        }
        let update_id = update_id();
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        tx.execute(
            r#"INSERT INTO fleet_update_runs(
                update_id, version, artifact_sha256, publisher_key_id,
                publisher_fingerprint, manifest_sha256, status, created_at,
                state_revision
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'planned', ?7, 0)"#,
            params![
                update_id,
                request.version,
                request.artifact_sha256.to_ascii_lowercase(),
                request.publisher_key_id,
                request.publisher_fingerprint,
                request.manifest_sha256.to_ascii_lowercase(),
                now
            ],
        )
        .map_err(HubError::internal)?;
        for machine_id in &machine_ids {
            let overlapping: bool = tx
                .query_row(
                    r#"SELECT EXISTS(
                        SELECT 1 FROM fleet_update_nodes n
                        JOIN fleet_update_runs r ON r.update_id = n.update_id
                        WHERE n.machine_id = ?1
                          AND n.status IN ('queued', 'updating', 'rolling-back')
                          AND r.status NOT IN ('completed', 'completed-with-failures', 'failed')
                    )"#,
                    params![machine_id],
                    |row| row.get(0),
                )
                .map_err(HubError::internal)?;
            if overlapping {
                return Err(HubError::conflict(
                    "machine-update-overlap",
                    "an active fleet update already owns this Machine",
                ));
            }
            let current: Option<FleetTargetRow> = tx
                .query_row(
                    "SELECT display_name, collector_version, collector_protocol_version, archived_at, state_revision, desired_version, collector_runtime_generation FROM machines WHERE machine_id = ?1",
                    params![machine_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
                )
                .optional()
                .map_err(HubError::internal)?;
            let Some((
                _,
                current_version,
                protocol,
                archived_at,
                revision,
                desired_version,
                runtime_generation,
            )) = current
            else {
                return Err(HubError::not_found(
                    "machine-not-found",
                    "update target Machine was not found",
                ));
            };
            if archived_at.is_some() {
                return Err(HubError::conflict(
                    "machine-archived",
                    "archived Machines cannot receive updates",
                ));
            }
            tx.execute(
                r#"INSERT INTO fleet_update_nodes(
                    update_id, machine_id, status, previous_version,
                    previous_desired_version, previous_runtime_generation, collector_protocol_version, state_revision
                ) VALUES (?1, ?2, 'queued', ?3, ?4, ?5, ?6, 0)"#,
                params![update_id, machine_id, current_version, desired_version, runtime_generation, protocol],
            )
            .map_err(HubError::internal)?;
            let revision_changed = tx
                .execute(
                    "UPDATE machines SET desired_version = ?2, state_revision = state_revision + 1 WHERE machine_id = ?1 AND state_revision = ?3 AND archived_at IS NULL",
                    params![machine_id, request.version, revision],
                )
                .map_err(HubError::internal)?;
            if revision_changed == 0 {
                return Err(HubError::conflict(
                    "machine-state-conflict",
                    "Machine changed while planning the fleet update",
                ));
            }
        }
        tx.commit().map_err(HubError::internal)?;
        let update = self.fleet_update(&update_id)?;
        Ok(FleetUpdatePlanResponse {
            update,
            ordered_steps: vec![
                "snapshot Hub rollback state".to_string(),
                "update Hub".to_string(),
                "health-check Hub".to_string(),
                "snapshot each Collector".to_string(),
                "update and restart each Collector".to_string(),
                "health-check and independently roll back failures".to_string(),
            ],
        })
    }

    pub(crate) fn list_fleet_updates(&self) -> HubResult<Vec<FleetUpdateRun>> {
        let conn = self.db.connection().map_err(HubError::internal)?;
        let ids = conn
            .prepare("SELECT update_id FROM fleet_update_runs ORDER BY created_at DESC")
            .map_err(HubError::internal)?
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(HubError::internal)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(HubError::internal)?;
        ids.into_iter().map(|id| self.fleet_update(&id)).collect()
    }

    pub(crate) fn fleet_update(&self, update_id: &str) -> HubResult<FleetUpdateRun> {
        let update_id = validate_identifier(update_id, "update_id")?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        let run = conn
            .query_row(
                r#"SELECT update_id, version, artifact_sha256, publisher_key_id,
                    publisher_fingerprint, manifest_sha256, status, created_at,
                    started_at, hub_snapshot_at, hub_restart_requested_at, hub_updated_at, hub_health_at,
                    completed_at, failure_reason, attempts, state_revision
                FROM fleet_update_runs WHERE update_id = ?1"#,
                params![update_id],
                update_run_from_row,
            )
            .optional()
            .map_err(HubError::internal)?
            .ok_or_else(|| HubError::not_found("update-not-found", "fleet update was not found"))?;
        let mut nodes = conn
            .prepare(
                r#"SELECT update_id, machine_id, status, previous_version,
                    previous_desired_version, previous_runtime_generation, snapshot_at, update_started_at, restarted_at, health_checked_at,
                    rolled_back_at, failure_reason, collector_protocol_version,
                    evidence_json, attempts, state_revision
                FROM fleet_update_nodes WHERE update_id = ?1 ORDER BY machine_id"#,
            )
            .map_err(HubError::internal)?
            .query_map(params![update_id], update_node_from_row)
            .map_err(HubError::internal)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(HubError::internal)?;
        let mut run = run;
        run.nodes.append(&mut nodes);
        Ok(run)
    }

    pub(crate) fn record_hub_snapshot_server(
        &self,
        update_id: &str,
    ) -> HubResult<FleetUpdateSnapshotResponse> {
        let update_id = validate_identifier(update_id, "update_id")?;
        let now = now_utc();
        let snapshot_path = self.hub_snapshot_path(&update_id)?;
        if snapshot_path.exists() {
            return Err(HubError::conflict(
                "hub-snapshot-exists",
                "the durable Hub snapshot already exists for this update",
            ));
        }
        if let Some(parent) = snapshot_path.parent() {
            fs::create_dir_all(parent).map_err(HubError::internal)?;
            set_private_directory(parent).map_err(HubError::internal)?;
        }
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let changed = tx
            .execute(
                r#"UPDATE fleet_update_runs SET status = 'hub-updating', started_at = COALESCE(started_at, ?2),
                    hub_snapshot_at = ?2, attempts = attempts + 1, state_revision = state_revision + 1
                    WHERE update_id = ?1 AND status = 'planned'"#,
                params![update_id, now],
            )
            .map_err(HubError::internal)?;
        if changed == 0 {
            return Err(HubError::conflict(
                "hub-snapshot-order",
                "Hub snapshot is only valid as the first fleet update mutation",
            ));
        }
        tx.commit().map_err(HubError::internal)?;
        // VACUUM INTO is SQLite's backup primitive and includes a consistent
        // WAL view. It is intentionally performed after the state transition;
        // a crash before this write is reconciled as a failed-start run.
        let conn = self.db.connection().map_err(HubError::internal)?;
        conn.execute(
            "VACUUM INTO ?1",
            params![snapshot_path.to_string_lossy().to_string()],
        )
        .map_err(HubError::internal)?;
        set_private_file(&snapshot_path).map_err(HubError::internal)?;
        sync_file(&snapshot_path).map_err(HubError::internal)?;
        if let Some(parent) = snapshot_path.parent() {
            sync_parent_directory(parent).map_err(HubError::internal)?;
        }
        let bytes = fs::read(&snapshot_path).map_err(HubError::internal)?;
        let digest = hex::encode(Sha256::digest(&bytes));
        let conn = self.db.connection().map_err(HubError::internal)?;
        if let Err(error) = conn.execute(
            "INSERT INTO fleet_update_snapshots(update_id, target_kind, machine_id, snapshot_path, snapshot_sha256, created_at) VALUES (?1, 'hub', NULL, ?2, ?3, ?4)",
            params![update_id, snapshot_path.to_string_lossy().to_string(), digest, now],
        ) {
            let _ = fs::remove_file(&snapshot_path);
            return Err(HubError::internal(error));
        }
        self.fleet_update(&update_id)
            .map(|update| FleetUpdateSnapshotResponse {
                update_id,
                status: update.status,
                state_revision: update.state_revision,
                hub_snapshot_at: update.hub_snapshot_at.unwrap_or(now),
            })
    }

    fn hub_snapshot_path(&self, update_id: &str) -> HubResult<PathBuf> {
        let root = self
            .db
            .path()
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(root
            .join("fleet-updates")
            .join(update_id)
            .join("hub.sqlite3"))
    }

    #[allow(dead_code)]
    pub(crate) fn record_hub_snapshot(
        &self,
        update_id: &str,
        evidence: &FleetUpdateEvidence,
    ) -> HubResult<FleetUpdateSnapshotResponse> {
        self.validate_evidence(update_id, evidence)?;
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let conn = self.db.connection().map_err(HubError::internal)?;
        let changed = conn
            .execute(
                r#"UPDATE fleet_update_runs SET status = 'hub-updating', started_at = COALESCE(started_at, ?2),
                    hub_snapshot_at = ?2, attempts = attempts + 1, state_revision = state_revision + 1
                    WHERE update_id = ?1 AND status = 'planned'"#,
                params![update_id, now],
            )
            .map_err(HubError::internal)?;
        if changed == 0 {
            return Err(HubError::conflict(
                "hub-snapshot-order",
                "Hub snapshot is only valid as the first fleet update mutation",
            ));
        }
        Ok(FleetUpdateSnapshotResponse {
            update_id: update_id.to_string(),
            status: "hub-updating".to_string(),
            state_revision: self.fleet_update(update_id)?.state_revision,
            hub_snapshot_at: now,
        })
    }

    pub(crate) fn mark_hub_runtime_started(&self, runtime_version: &str) -> HubResult<()> {
        let runtime_version = validate_non_empty(runtime_version, "runtime_version")?;
        let now = now_utc();
        let generation = format!("hub-runtime-{}", random_token(12));
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let conn = self.db.connection().map_err(HubError::internal)?;
        conn.execute(
            "INSERT INTO hub_runtime_state(singleton, runtime_generation, runtime_version, started_at, health_checked_at) VALUES (1, ?1, ?2, ?3, ?3) ON CONFLICT(singleton) DO UPDATE SET runtime_generation = excluded.runtime_generation, runtime_version = excluded.runtime_version, started_at = excluded.started_at, health_checked_at = excluded.health_checked_at",
            params![generation, runtime_version, now],
        )
        .map_err(HubError::internal)?;
        Ok(())
    }

    pub(crate) fn record_hub_health_server(&self, update_id: &str) -> HubResult<FleetUpdateRun> {
        let update_id = validate_identifier(update_id, "update_id")?;
        let update = self.fleet_update(&update_id)?;
        if update.status == "hub-updating"
            && update
                .started_at
                .as_deref()
                .and_then(parse_timestamp)
                .is_some_and(|started| Utc::now() > started + HUB_UPDATE_TIMEOUT)
        {
            return self.fail_hub_update(&update_id, "Hub restart/health reconciliation timed out");
        }
        let runtime: Option<(String, String, String, String)> = self
            .db
            .connection()
            .map_err(HubError::internal)?
            .query_row(
                "SELECT runtime_generation, runtime_version, health_checked_at, started_at FROM hub_runtime_state WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        let snapshot_exists: bool = self
            .db
            .connection()
            .map_err(HubError::internal)?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM fleet_update_snapshots WHERE update_id = ?1 AND target_kind = 'hub' AND machine_id IS NULL)",
                params![update_id],
                |row| row.get(0),
            )
            .map_err(HubError::internal)?;
        if !snapshot_exists {
            return self.fail_hub_update(&update_id, "durable Hub snapshot proof did not complete");
        }
        let Some((generation, runtime_version, health_checked_at, runtime_started_at)) = runtime
        else {
            // The request that applied the artifact may be terminating before
            // the replacement Hub records its new process generation. Keep the
            // durable run resumable instead of treating the old generation as
            // a client-confirmed health failure.
            return self.fleet_update(&update_id);
        };
        let update_started_at = update
            .started_at
            .as_deref()
            .or(update.hub_snapshot_at.as_deref())
            .ok_or_else(|| HubError::internal("Hub update start timestamp is missing"))?;
        let runtime_is_newer = parse_utc_timestamp(&runtime_started_at)?
            > parse_utc_timestamp(update_started_at)?
            && parse_utc_timestamp(&health_checked_at)? > parse_utc_timestamp(update_started_at)?;
        if !runtime_is_newer {
            return self.fleet_update(&update_id);
        }
        if runtime_version != update.version || generation.is_empty() {
            return self.fail_hub_update(
                &update_id,
                "Hub restart generation is new but its version proof did not match the plan",
            );
        }
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let conn = self.db.connection().map_err(HubError::internal)?;
        let changed = conn
            .execute(
                "UPDATE fleet_update_runs SET status = 'collectors-queued', hub_updated_at = ?2, hub_health_at = ?3, attempts = attempts + 1, state_revision = state_revision + 1 WHERE update_id = ?1 AND status = 'hub-updating'",
                params![update_id, now, health_checked_at],
            )
            .map_err(HubError::internal)?;
        if changed == 0 {
            return Err(HubError::conflict(
                "hub-health-order",
                "Hub health has already been recorded or the run is not updating",
            ));
        }
        drop(_guard);
        self.finish_update_if_terminal(&update_id)
    }

    pub(crate) fn execute_server_fleet_update(
        &self,
        update_id: &str,
        artifact_dir: Option<&PathBuf>,
        update_target: Option<&PathBuf>,
        service_manager: Option<&str>,
    ) -> HubResult<FleetUpdateRun> {
        let update = self.fleet_update(update_id)?;
        if update.status == "planned" {
            if let Err(error) = self.record_hub_snapshot_server(update_id) {
                let _ = self.fail_hub_update(update_id, "Hub snapshot creation failed");
                if let Ok(path) = self.hub_snapshot_path(update_id) {
                    let _ = fs::remove_file(path);
                }
                return Err(error);
            }
            if let Err(error) = self.apply_hub_update_artifact(
                &update,
                artifact_dir,
                update_target,
                service_manager,
            ) {
                let _ = self.fail_hub_update(update_id, "Hub artifact application failed");
                let _ = self.restore_hub_update_target(update_id, update_target);
                return Err(error);
            }
            // Restart is deliberately delegated to one fixed service manager;
            // completion is reconciled after the next Hub process generation
            // records its runtime proof. This request never asserts health.
            if let Err(error) =
                self.request_hub_restart(service_manager, update_id, update_target.cloned())
            {
                let _ = self.fail_hub_update(update_id, "Hub restart request failed");
                let _ = self.restore_hub_update_target(update_id, update_target);
                return Err(error);
            }
        } else if update.status == "hub-updating"
            && !self.hub_runtime_ready(&update)?
            && update
                .started_at
                .as_deref()
                .and_then(parse_timestamp)
                .is_none_or(|started| Utc::now() <= started + HUB_UPDATE_TIMEOUT)
        {
            // A process crash can strand the durable run after the snapshot or
            // binary replacement but before the restart request. Reconcile the
            // same target without replacing the original rollback snapshot.
            if let Err(error) = self.apply_hub_update_artifact(
                &update,
                artifact_dir,
                update_target,
                service_manager,
            ) {
                let _ = self.fail_hub_update(update_id, "Hub artifact recovery failed");
                let _ = self.restore_hub_update_target(update_id, update_target);
                return Err(error);
            }
            self.request_hub_restart(service_manager, update_id, update_target.cloned())?;
        }
        self.reconcile_server_fleet_update(update_id, update_target)
    }

    pub(crate) fn reconcile_server_fleet_update_with_runtime(
        &self,
        update_id: &str,
        artifact_dir: Option<&PathBuf>,
        update_target: Option<&PathBuf>,
        service_manager: Option<&str>,
    ) -> HubResult<FleetUpdateRun> {
        let update = self.fleet_update(update_id)?;
        if update.status == "hub-updating"
            && !self.hub_runtime_ready(&update)?
            && update
                .started_at
                .as_deref()
                .and_then(parse_timestamp)
                .is_none_or(|started| Utc::now() <= started + HUB_UPDATE_TIMEOUT)
        {
            self.apply_hub_update_artifact(&update, artifact_dir, update_target, service_manager)?;
            self.request_hub_restart(service_manager, update_id, update_target.cloned())?;
        }
        self.reconcile_server_fleet_update(update_id, update_target)
    }

    pub(crate) fn reconcile_server_fleet_update(
        &self,
        update_id: &str,
        update_target: Option<&PathBuf>,
    ) -> HubResult<FleetUpdateRun> {
        self.mark_stranded_collector_updates(update_id)?;
        self.reconcile_collector_update_commands(update_id)?;
        let update = self.fleet_update(update_id)?;
        if update.status == "hub-updating" {
            let update = self.record_hub_health_server(update_id)?;
            if update.status != "collectors-queued" {
                if update.status == "failed" {
                    self.restore_hub_update_target(update_id, update_target)?;
                }
                return Ok(update);
            }
        }
        let update = self.fleet_update(update_id)?;
        if update.status == "collectors-queued" {
            for node in update.nodes.iter().filter(|node| node.status == "queued") {
                if let Err(_error) = self.start_collector_update(update_id, &node.machine_id) {
                    self.rollback_queued_collector_update(
                        update_id,
                        &node.machine_id,
                        "Collector update could not start; node was rolled back",
                    )?;
                }
            }
        }
        self.finish_update_if_terminal(update_id)
    }

    fn rollback_queued_collector_update(
        &self,
        update_id: &str,
        machine_id: &str,
        reason: &str,
    ) -> HubResult<()> {
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        tx.execute(
            "UPDATE fleet_update_nodes SET status = 'rolled-back', rolled_back_at = ?3, failure_reason = ?4, attempts = attempts + 1, state_revision = state_revision + 1 WHERE update_id = ?1 AND machine_id = ?2 AND status = 'queued'",
            params![update_id, machine_id, now_utc(), reason],
        )
        .map_err(HubError::internal)?;
        tx.execute(
            "UPDATE machines SET desired_version = (SELECT previous_desired_version FROM fleet_update_nodes WHERE update_id = ?1 AND machine_id = ?2), state_revision = state_revision + 1 WHERE machine_id = ?2 AND archived_at IS NULL AND revoked_at IS NULL",
            params![update_id, machine_id],
        )
        .map_err(HubError::internal)?;
        tx.commit().map_err(HubError::internal)
    }

    fn hub_runtime_ready(&self, update: &FleetUpdateRun) -> HubResult<bool> {
        let Some(started_at) = update.started_at.as_deref() else {
            return Ok(false);
        };
        let runtime: Option<(String, String, String)> = self
            .db
            .connection()
            .map_err(HubError::internal)?
            .query_row(
                "SELECT runtime_version, started_at, health_checked_at FROM hub_runtime_state WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some((version, runtime_started_at, health_checked_at)) = runtime else {
            return Ok(false);
        };
        Ok(version == update.version
            && parse_utc_timestamp(&runtime_started_at)? > parse_utc_timestamp(started_at)?
            && parse_utc_timestamp(&health_checked_at)? > parse_utc_timestamp(started_at)?)
    }

    fn apply_hub_update_artifact(
        &self,
        update: &FleetUpdateRun,
        artifact_dir: Option<&PathBuf>,
        update_target: Option<&PathBuf>,
        service_manager: Option<&str>,
    ) -> HubResult<()> {
        let (Some(artifact_dir), Some(update_target), Some(_)) =
            (artifact_dir, update_target, service_manager)
        else {
            return Err(HubError::forbidden(
                "fleet-runtime-required",
                "server-owned fleet updates require configured local artifact and service-manager trust",
            ));
        };
        let artifact_path = artifact_dir.join(format!("{}.artifact", update.version));
        let artifact =
            read_bounded_executable_update_artifact(&artifact_path, &update.artifact_sha256)?;
        let parent = update_target.parent().ok_or_else(|| {
            HubError::unprocessable("fleet-runtime-invalid", "Hub update target has no parent")
        })?;
        fs::create_dir_all(parent).map_err(HubError::internal)?;
        let target_metadata = optional_safe_file_metadata(update_target, "Hub update target")?;
        let backup = hub_binary_backup_path(update_target, &update.update_id);
        let missing = hub_binary_missing_marker(update_target, &update.update_id);
        let backup_exists =
            optional_safe_file_metadata(&backup, "Hub rollback snapshot")?.is_some();
        let missing_exists =
            optional_safe_file_metadata(&missing, "Hub rollback missing marker")?.is_some();
        let existing_digest = target_metadata
            .as_ref()
            .and_then(|_| fs::read(update_target).ok())
            .map(|bytes| hex::encode(Sha256::digest(bytes)));
        if existing_digest
            .as_deref()
            .is_some_and(|digest| digest.eq_ignore_ascii_case(&update.artifact_sha256))
        {
            set_executable_mode(update_target).map_err(HubError::internal)?;
            sync_file(update_target).map_err(HubError::internal)?;
            sync_parent_directory(parent).map_err(HubError::internal)?;
            return Ok(());
        }
        if target_metadata.is_some() && !backup_exists {
            copy_file_durable(update_target, &backup, true)?;
        } else if target_metadata.is_none() && !backup_exists && !missing_exists {
            create_private_marker_durable(&missing)?;
        }
        let temporary = parent.join(format!(".fleet-{}.tmp", random_token(8)));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(HubError::internal)?;
        use std::io::Write;
        let write_result = (|| -> std::io::Result<()> {
            file.write_all(&artifact)?;
            set_executable_mode(&temporary).map_err(std::io::Error::other)?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(HubError::internal(error));
        }
        drop(file);
        fs::rename(&temporary, update_target).map_err(HubError::internal)?;
        if let Err(error) = sync_parent_directory(parent) {
            let _ = self.restore_hub_update_target(&update.update_id, Some(update_target));
            return Err(HubError::internal(error));
        }
        Ok(())
    }

    fn mark_stranded_collector_updates(&self, update_id: &str) -> HubResult<()> {
        let update_id = validate_identifier(update_id, "update_id")?;
        let cutoff = (Utc::now() - COLLECTOR_UPDATE_TIMEOUT).to_rfc3339();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        tx.execute(
            "UPDATE fleet_update_nodes SET status = 'rolling-back', failure_reason = COALESCE(failure_reason, 'Collector update receipt timed out; rollback requested'), attempts = attempts + 1, state_revision = state_revision + 1 WHERE update_id = ?1 AND status = 'updating' AND update_started_at IS NOT NULL AND update_started_at <= ?2",
            params![update_id, cutoff],
        )
        .map_err(HubError::internal)?;
        tx.execute(
            "UPDATE machines SET desired_version = (SELECT previous_desired_version FROM fleet_update_nodes n WHERE n.update_id = ?1 AND n.machine_id = machines.machine_id), state_revision = state_revision + 1 WHERE machine_id IN (SELECT machine_id FROM fleet_update_nodes WHERE update_id = ?1 AND status = 'rolling-back') AND archived_at IS NULL AND COALESCE(desired_version, '') <> COALESCE((SELECT previous_desired_version FROM fleet_update_nodes n WHERE n.update_id = ?1 AND n.machine_id = machines.machine_id), '')",
            params![update_id],
        )
        .map_err(HubError::internal)?;
        tx.commit().map_err(HubError::internal)?;
        Ok(())
    }

    pub(crate) fn reconcile_collector_update_commands(&self, update_id: &str) -> HubResult<()> {
        let update_id = validate_identifier(update_id, "update_id")?;
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let update: Option<(String, String, String)> = tx
            .query_row(
                "SELECT status, version, artifact_sha256 FROM fleet_update_runs WHERE update_id = ?1",
                params![update_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some((status, version, sha256)) = update else {
            return Err(HubError::not_found(
                "update-not-found",
                "fleet update was not found",
            ));
        };
        if status != "collectors-queued" {
            tx.commit().map_err(HubError::internal)?;
            return Ok(());
        }
        let nodes = tx
            .prepare("SELECT machine_id, status, state_revision FROM fleet_update_nodes WHERE update_id = ?1 AND status IN ('updating', 'rolling-back')")
            .map_err(HubError::internal)?
            .query_map(params![update_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
            })
            .map_err(HubError::internal)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(HubError::internal)?;
        for (machine_id, node_status, node_revision) in &nodes {
            let (command_id, command) = if node_status == "rolling-back" {
                let command_id = fleet_rollback_command_id(&update_id, machine_id);
                let command = OwnerCommand::RollbackUpdate {
                    command_id: command_id.clone(),
                    update_id: update_id.clone(),
                    version: version.clone(),
                    sha256: sha256.clone(),
                    expected_state_revision: *node_revision,
                };
                (command_id, command)
            } else {
                let command_id = fleet_update_command_id(&update_id, machine_id);
                let command = OwnerCommand::ApprovedUpdate {
                    command_id: command_id.clone(),
                    update_id: update_id.clone(),
                    version: version.clone(),
                    sha256: sha256.clone(),
                    expected_state_revision: *node_revision,
                };
                (command_id, command)
            };
            let command_json = serde_json::to_string(&command).map_err(HubError::internal)?;
            tx.execute(
                "INSERT OR IGNORE INTO collector_commands(command_id, machine_id, command_json, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![command_id, machine_id, command_json, now_utc()],
            )
            .map_err(HubError::internal)?;
        }
        tx.commit().map_err(HubError::internal)?;
        for (machine_id, _, _) in nodes {
            self.notify_machine_command(&machine_id);
        }
        Ok(())
    }

    pub(crate) fn restore_hub_update_target(
        &self,
        update_id: &str,
        update_target: Option<&PathBuf>,
    ) -> HubResult<()> {
        let Some(update_target) = update_target else {
            return Ok(());
        };
        let parent = update_target.parent().ok_or_else(|| {
            HubError::unprocessable("fleet-runtime-invalid", "Hub update target has no parent")
        })?;
        let backup = hub_binary_backup_path(update_target, update_id);
        let missing = hub_binary_missing_marker(update_target, update_id);
        let backup_exists =
            optional_safe_file_metadata(&backup, "Hub rollback snapshot")?.is_some();
        let missing_exists =
            optional_safe_file_metadata(&missing, "Hub rollback missing marker")?.is_some();
        if backup_exists {
            fs::rename(&backup, update_target).map_err(HubError::internal)?;
            set_executable_mode(update_target).map_err(HubError::internal)?;
            sync_file(update_target).map_err(HubError::internal)?;
            if missing_exists {
                fs::remove_file(&missing).map_err(HubError::internal)?;
            }
            sync_parent_directory(parent).map_err(HubError::internal)?;
        } else if missing_exists {
            if optional_path_metadata(update_target)
                .map_err(HubError::internal)?
                .is_some()
            {
                fs::remove_file(update_target).map_err(HubError::internal)?;
            }
            fs::remove_file(&missing).map_err(HubError::internal)?;
            sync_parent_directory(parent).map_err(HubError::internal)?;
        }
        Ok(())
    }

    fn request_hub_restart(
        &self,
        service_manager: Option<&str>,
        update_id: &str,
        update_target: Option<PathBuf>,
    ) -> HubResult<()> {
        let update_id = validate_identifier(update_id, "update_id")?;
        let (program, args): (&str, Vec<String>) = match service_manager {
            Some("systemd-user") => (
                "systemctl",
                vec![
                    "--user".to_string(),
                    "restart".to_string(),
                    "dirtydash-hub.service".to_string(),
                ],
            ),
            Some("launchd") => {
                let domain = format!("gui/{}", unsafe { libc::geteuid() });
                (
                    "launchctl",
                    vec![
                        "kickstart".to_string(),
                        "-k".to_string(),
                        format!("{domain}/dev.dirtydash.hub"),
                    ],
                )
            }
            _ => {
                return Err(HubError::forbidden(
                    "fleet-runtime-required",
                    "Hub restart requires a configured fixed service manager",
                ))
            }
        };
        let now = now_utc();
        let retry_cutoff = (Utc::now() - Duration::seconds(30)).to_rfc3339();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let conn = self.db.connection().map_err(HubError::internal)?;
        let changed = conn
            .execute(
                "UPDATE fleet_update_runs SET hub_restart_requested_at = ?2, attempts = attempts + 1, state_revision = state_revision + 1 WHERE update_id = ?1 AND status = 'hub-updating' AND (hub_restart_requested_at IS NULL OR hub_restart_requested_at <= ?3)",
                params![update_id, now, retry_cutoff],
            )
            .map_err(HubError::internal)?;
        drop(conn);
        drop(_guard);
        if changed == 0 {
            return Ok(());
        }
        let repository = self.clone();
        std::thread::Builder::new()
            .name("dirtydash-hub-restart".to_string())
            .spawn(move || {
                let succeeded = std::process::Command::new(program)
                    .args(args)
                    .status()
                    .is_ok_and(|status| status.success());
                if !succeeded {
                    let _ = repository
                        .fail_hub_update(&update_id, "Hub service manager restart failed");
                    let _ =
                        repository.restore_hub_update_target(&update_id, update_target.as_ref());
                }
            })
            .map_err(|_| HubError::internal("could not start Hub restart request"))?;
        Ok(())
    }

    fn fail_hub_update(&self, update_id: &str, reason: &str) -> HubResult<FleetUpdateRun> {
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        tx.execute(
            "UPDATE fleet_update_runs SET status = 'failed', failure_reason = ?2, completed_at = ?3, state_revision = state_revision + 1 WHERE update_id = ?1 AND status IN ('planned', 'hub-updating')",
            params![update_id, reason, now],
        )
        .map_err(HubError::internal)?;
        tx.execute(
            "UPDATE machines SET desired_version = (SELECT previous_desired_version FROM fleet_update_nodes n WHERE n.update_id = ?1 AND n.machine_id = machines.machine_id), state_revision = state_revision + 1 WHERE machine_id IN (SELECT machine_id FROM fleet_update_nodes WHERE update_id = ?1) AND archived_at IS NULL",
            params![update_id],
        )
        .map_err(HubError::internal)?;
        tx.commit().map_err(HubError::internal)?;
        self.fleet_update(update_id)
    }

    #[allow(dead_code)]
    pub(crate) fn record_hub_health(
        &self,
        update_id: &str,
        request: FleetHubHealthRequest,
    ) -> HubResult<FleetUpdateRun> {
        self.validate_evidence(update_id, &request.evidence)?;
        let update = self.fleet_update(update_id)?;
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let conn = self.db.connection().map_err(HubError::internal)?;
        let hub_success = request.healthy
            && request.restarted
            && request.health_checked
            && request.hub_version == update.version;
        let status = if hub_success {
            "collectors-queued"
        } else {
            "failed"
        };
        let failure = if hub_success {
            None
        } else {
            Some(
                request
                    .failure_reason
                    .as_deref()
                    .unwrap_or("Hub health check failed"),
            )
        };
        let changed = conn
            .execute(
                r#"UPDATE fleet_update_runs SET status = ?2, hub_updated_at = ?3,
                    hub_health_at = CASE WHEN ?2 = 'collectors-queued' THEN ?3 ELSE NULL END,
                    failure_reason = ?4, attempts = attempts + 1, state_revision = state_revision + 1
                    WHERE update_id = ?1 AND status = 'hub-updating' AND state_revision = ?5"#,
                params![update_id, status, now, failure, request.expected_state_revision],
            )
            .map_err(HubError::internal)?;
        if changed == 0 {
            return Err(HubError::conflict(
                "hub-health-order",
                "Hub health must follow its snapshot and use the latest update revision",
            ));
        }
        self.fleet_update(update_id)
    }

    pub(crate) fn start_collector_update(
        &self,
        update_id: &str,
        machine_id: &str,
    ) -> HubResult<MachineActionResponse> {
        let update_id = validate_identifier(update_id, "update_id")?;
        let machine_id = validate_identifier(machine_id, "machine_id")?;
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let update: Option<(String, String)> = tx
            .query_row(
                "SELECT status, version FROM fleet_update_runs WHERE update_id = ?1",
                params![update_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some((update_status, version)) = update else {
            return Err(HubError::not_found(
                "update-not-found",
                "fleet update was not found",
            ));
        };
        if update_status != "collectors-queued" {
            return Err(HubError::conflict(
                "collector-update-order",
                "Collectors can update only after the Hub health gate succeeds",
            ));
        }
        let node: Option<(String, i64)> = tx
            .query_row(
                "SELECT status, state_revision FROM fleet_update_nodes WHERE update_id = ?1 AND machine_id = ?2",
                params![update_id, machine_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some((node_status, node_revision)) = node else {
            return Err(HubError::not_found(
                "update-machine-not-found",
                "Machine is not in this update",
            ));
        };
        let machine: Option<FleetMachineStateRow> = tx
            .query_row(
                "SELECT revoked_at, archived_at, collector_protocol_version, state_revision FROM machines WHERE machine_id = ?1",
                params![machine_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some((revoked_at, archived_at, protocol, machine_revision)) = machine else {
            return Err(HubError::not_found(
                "machine-not-found",
                "Machine was not found",
            ));
        };
        if revoked_at.is_some() || archived_at.is_some() {
            return Err(HubError::conflict(
                "machine-archived",
                "archived Machines cannot receive updates",
            ));
        }
        if !ProtocolCompatibility::for_version(protocol).is_supported() {
            return Err(HubError::conflict(
                "incompatible-collector-protocol",
                "Collector is outside the current/previous compatibility window",
            ));
        }
        let command_id = fleet_update_command_id(&update_id, &machine_id);
        let command = OwnerCommand::ApprovedUpdate {
            command_id: command_id.clone(),
            update_id: update_id.clone(),
            version,
            sha256: tx
                .query_row(
                    "SELECT artifact_sha256 FROM fleet_update_runs WHERE update_id = ?1",
                    params![update_id],
                    |row| row.get(0),
                )
                .map_err(HubError::internal)?,
            expected_state_revision: node_revision + if node_status == "queued" { 1 } else { 0 },
        };
        let command_json = serde_json::to_string(&command).map_err(HubError::internal)?;
        if command_json.len() > 4096 {
            return Err(HubError::unprocessable(
                "collector-command-too-large",
                "Collector update command is outside the typed protocol bound",
            ));
        }
        let existing_command: Option<String> = tx
            .query_row(
                "SELECT command_json FROM collector_commands WHERE command_id = ?1",
                params![command_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(HubError::internal)?;
        if let Some(existing_command) = existing_command {
            if existing_command != command_json {
                return Err(HubError::conflict(
                    "collector-command-conflict",
                    "deterministic update command is already bound to different data",
                ));
            }
            if node_status == "updating" {
                tx.commit().map_err(HubError::internal)?;
                return Ok(MachineActionResponse {
                    machine_id,
                    command_id,
                    action: "update".to_string(),
                    state_revision: machine_revision,
                });
            }
        } else if node_status != "queued" {
            return Err(HubError::conflict(
                "collector-update-state",
                "Collector update is already running or complete",
            ));
        }
        if node_status == "queued" {
            tx.execute(
                "INSERT INTO collector_commands(command_id, machine_id, command_json, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![command_id, machine_id, command_json, now],
            )
            .map_err(HubError::internal)?;
            let node_changed = tx
                .execute(
                    "UPDATE fleet_update_nodes SET status = 'updating', snapshot_at = ?3, update_started_at = ?3, attempts = attempts + 1, state_revision = state_revision + 1 WHERE update_id = ?1 AND machine_id = ?2 AND status = 'queued' AND state_revision = ?4",
                    params![update_id, machine_id, now, node_revision],
                )
                .map_err(HubError::internal)?;
            if node_changed == 0 {
                return Err(HubError::conflict(
                    "collector-update-state",
                    "Collector update was changed concurrently",
                ));
            }
            let machine_changed = tx
                .execute(
                    "UPDATE machines SET state_revision = state_revision + 1 WHERE machine_id = ?1 AND state_revision = ?2 AND archived_at IS NULL AND revoked_at IS NULL",
                    params![machine_id, machine_revision],
                )
                .map_err(HubError::internal)?;
            if machine_changed == 0 {
                return Err(HubError::conflict(
                    "machine-state-conflict",
                    "Machine changed while starting its update",
                ));
            }
            tx.execute(
                "UPDATE fleet_update_runs SET attempts = attempts + 1, state_revision = state_revision + 1 WHERE update_id = ?1",
                params![update_id],
            )
            .map_err(HubError::internal)?;
        }
        tx.commit().map_err(HubError::internal)?;
        self.notify_machine_command(&machine_id);
        Ok(MachineActionResponse {
            machine_id,
            command_id,
            action: "update".to_string(),
            state_revision: if node_status == "queued" {
                machine_revision + 1
            } else {
                machine_revision
            },
        })
    }

    #[allow(dead_code)]
    pub(crate) fn complete_collector_update(
        &self,
        update_id: &str,
        machine_id: &str,
        request: FleetUpdateNodeCompletion,
    ) -> HubResult<FleetUpdateRun> {
        let update = self.fleet_update(update_id)?;
        let node = update
            .nodes
            .iter()
            .find(|node| node.machine_id == machine_id)
            .ok_or_else(|| {
                HubError::not_found("update-machine-not-found", "Machine is not in this update")
            })?;
        if node.state_revision != request.expected_state_revision || node.status != "updating" {
            return Err(HubError::conflict(
                "collector-update-state",
                "Collector update state changed; reload before recording health",
            ));
        }
        self.validate_evidence(update_id, &request.signed_evidence)?;
        let protocol = ProtocolCompatibility::for_version(Some(request.protocol_version));
        let successful = request.restarted
            && request.health_checked
            && request.collector_version == update.version
            && request.signed_evidence.version == update.version
            && protocol.is_supported();
        let now = now_utc();
        let status = if successful {
            "succeeded"
        } else {
            "rolled-back"
        };
        let failure_reason =
            if successful {
                None
            } else {
                Some(request.failure_reason.as_deref().unwrap_or(
                    "restart, version, health, protocol, or signed evidence check failed",
                ))
            };
        let evidence_json =
            serde_json::to_string(&request.signed_evidence).map_err(HubError::internal)?;
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let changed = tx
            .execute(
                r#"UPDATE fleet_update_nodes SET status = ?3, restarted_at = CASE WHEN ?4 THEN ?5 ELSE restarted_at END,
                    health_checked_at = CASE WHEN ?6 THEN ?5 ELSE health_checked_at END,
                    rolled_back_at = CASE WHEN ?3 = 'rolled-back' THEN ?5 ELSE rolled_back_at END,
                    failure_reason = ?7, collector_protocol_version = ?8,
                    evidence_json = ?9, attempts = attempts + 1, state_revision = state_revision + 1
                    WHERE update_id = ?1 AND machine_id = ?2 AND status = 'updating' AND state_revision = ?10"#,
                params![
                    update_id,
                    machine_id,
                    status,
                    request.restarted,
                    now,
                    request.health_checked,
                    failure_reason,
                    request.protocol_version,
                    evidence_json,
                    request.expected_state_revision,
                ],
            )
            .map_err(HubError::internal)?;
        if changed == 0 {
            return Err(HubError::conflict(
                "collector-update-state",
                "Collector update was changed concurrently",
            ));
        }
        if successful {
            let machine_changed = tx
                .execute(
                    "UPDATE machines SET collector_version = ?2, collector_protocol_version = ?3, desired_version = NULL, state_revision = state_revision + 1 WHERE machine_id = ?1 AND archived_at IS NULL",
                    params![machine_id, request.collector_version, request.protocol_version],
                )
                .map_err(HubError::internal)?;
            if machine_changed == 0 {
                return Err(HubError::conflict(
                    "machine-state-conflict",
                    "Machine was archived while completing its update",
                ));
            }
        } else {
            tx.execute(
                "UPDATE machines SET desired_version = (SELECT previous_desired_version FROM fleet_update_nodes WHERE update_id = ?1 AND machine_id = ?2), state_revision = state_revision + 1 WHERE machine_id = ?2 AND archived_at IS NULL",
                params![update_id, machine_id],
            )
            .map_err(HubError::internal)?;
        }
        tx.commit().map_err(HubError::internal)?;
        drop(_guard);
        self.finish_update_if_terminal(update_id)
    }

    pub(crate) fn finish_update_if_terminal(&self, update_id: &str) -> HubResult<FleetUpdateRun> {
        let update = self.fleet_update(update_id)?;
        if !update.nodes.is_empty()
            && update
                .nodes
                .iter()
                .all(|node| matches!(node.status.as_str(), "succeeded" | "rolled-back"))
        {
            let status = if update.nodes.iter().all(|node| node.status == "succeeded") {
                "completed"
            } else {
                "completed-with-failures"
            };
            let now = now_utc();
            let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
            let conn = self.db.connection().map_err(HubError::internal)?;
            conn.execute(
                "UPDATE fleet_update_runs SET status = ?2, completed_at = ?3, state_revision = state_revision + 1 WHERE update_id = ?1 AND status = 'collectors-queued'",
                params![update_id, status, now],
            )
            .map_err(HubError::internal)?;
        }
        self.fleet_update(update_id)
    }

    #[allow(dead_code)]
    fn validate_evidence(&self, update_id: &str, evidence: &FleetUpdateEvidence) -> HubResult<()> {
        let update = self.fleet_update(update_id)?;
        if evidence.version != update.version
            || !evidence
                .artifact_sha256
                .eq_ignore_ascii_case(&update.artifact_sha256)
            || evidence.publisher_key_id != update.publisher_key_id
            || evidence.publisher_fingerprint != update.publisher_fingerprint
            || !evidence
                .manifest_sha256
                .eq_ignore_ascii_case(&update.manifest_sha256)
            || !evidence.publisher_verified
        {
            return Err(HubError::unprocessable(
                "signed-update-evidence-mismatch",
                "update evidence does not match the signed plan",
            ));
        }
        Ok(())
    }
}

fn hub_binary_backup_path(target: &Path, update_id: &str) -> PathBuf {
    let parent = target.parent().unwrap_or_else(|| std::path::Path::new("."));
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("dirtydash-hub");
    parent.join(format!(".{name}.previous-{update_id}"))
}

fn hub_binary_missing_marker(target: &Path, update_id: &str) -> PathBuf {
    let parent = target.parent().unwrap_or_else(|| std::path::Path::new("."));
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("dirtydash-hub");
    parent.join(format!(".{name}.previous-missing-{update_id}"))
}

pub(crate) fn read_bounded_executable_update_artifact(
    path: &Path,
    expected_sha256: &str,
) -> HubResult<Vec<u8>> {
    let expected = expected_sha256.trim().to_ascii_lowercase();
    if !digest_is_safe(&expected) {
        return Err(HubError::unprocessable(
            "fleet-artifact-mismatch",
            "the configured artifact digest is invalid",
        ));
    }
    let metadata =
        optional_safe_file_metadata(path, "signed update artifact")?.ok_or_else(|| {
            HubError::not_found(
                "fleet-artifact-unavailable",
                "the signed update artifact is unavailable",
            )
        })?;
    if metadata.len() > MAX_FLEET_UPDATE_ARTIFACT_BYTES {
        return Err(HubError::unprocessable(
            "fleet-artifact-too-large",
            "the signed update artifact exceeds the bounded size limit",
        ));
    }
    require_executable_file(&metadata, "signed update artifact")?;
    let bytes = fs::read(path).map_err(|_| {
        HubError::not_found(
            "fleet-artifact-unavailable",
            "the signed update artifact is unavailable",
        )
    })?;
    if bytes.len() as u64 > MAX_FLEET_UPDATE_ARTIFACT_BYTES {
        return Err(HubError::unprocessable(
            "fleet-artifact-too-large",
            "the signed update artifact exceeds the bounded size limit",
        ));
    }
    if hex::encode(Sha256::digest(&bytes)) != expected {
        return Err(HubError::unprocessable(
            "fleet-artifact-mismatch",
            "the configured artifact does not match the durable update digest",
        ));
    }
    Ok(bytes)
}

fn optional_path_metadata(path: &Path) -> std::io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn optional_safe_file_metadata(path: &Path, label: &str) -> HubResult<Option<fs::Metadata>> {
    let Some(metadata) = optional_path_metadata(path).map_err(HubError::internal)? else {
        return Ok(None);
    };
    if metadata.file_type().is_symlink() {
        return Err(HubError::unprocessable(
            "fleet-runtime-invalid",
            format!("{label} must not be a symlink"),
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(HubError::unprocessable(
            "fleet-runtime-invalid",
            format!("{label} must be a regular file"),
        ));
    }
    Ok(Some(metadata))
}

fn require_executable_file(metadata: &fs::Metadata, label: &str) -> HubResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(HubError::unprocessable(
                "fleet-artifact-not-executable",
                format!("{label} must be executable"),
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (metadata, label);
    }
    Ok(())
}

fn create_private_marker_durable(path: &Path) -> HubResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(HubError::internal)?;
    }
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(HubError::internal)?;
    set_private_file(path).map_err(HubError::internal)?;
    file.sync_all().map_err(HubError::internal)?;
    drop(file);
    if let Some(parent) = path.parent() {
        sync_parent_directory(parent).map_err(HubError::internal)?;
    }
    Ok(())
}

fn copy_file_durable(source: &Path, destination: &Path, executable: bool) -> HubResult<()> {
    let parent = destination.parent().ok_or_else(|| {
        HubError::unprocessable("fleet-runtime-invalid", "rollback snapshot has no parent")
    })?;
    fs::create_dir_all(parent).map_err(HubError::internal)?;
    let temporary = parent.join(format!(".snapshot-{}.tmp", random_token(8)));
    let mut input = fs::File::open(source).map_err(HubError::internal)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(HubError::internal)?;
    let copy_result = (|| -> std::io::Result<()> {
        std::io::copy(&mut input, &mut output)?;
        if executable {
            set_executable_mode(&temporary).map_err(std::io::Error::other)?;
        } else {
            set_private_file(&temporary).map_err(std::io::Error::other)?;
        }
        output.sync_all()?;
        Ok(())
    })();
    if let Err(error) = copy_result {
        let _ = fs::remove_file(&temporary);
        return Err(HubError::internal(error));
    }
    drop(output);
    fs::rename(&temporary, destination).map_err(HubError::internal)?;
    sync_parent_directory(parent).map_err(HubError::internal)?;
    Ok(())
}

fn sync_file(path: &std::path::Path) -> anyhow::Result<()> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn set_private_directory(path: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn set_private_file(path: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn set_executable_mode(path: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn sync_parent_directory(parent: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[derive(Debug)]
struct MachineRow {
    machine_id: String,
    display_name: String,
    enrolled_at: String,
    revoked_at: Option<String>,
    last_seen_at: Option<String>,
    archived_at: Option<String>,
    desired_version: Option<String>,
    collector_version: Option<String>,
    collector_protocol_version: Option<u32>,
    last_sync_at: Option<String>,
    diagnostics_json: Option<String>,
    diagnostics_status: Option<String>,
    diagnostics_at: Option<String>,
    state_revision: i64,
    credentials_active: u64,
    credentials_total: u64,
    pending_action: Option<String>,
    usage_event_count: u64,
}

fn machine_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MachineRow> {
    Ok(MachineRow {
        machine_id: row.get(0)?,
        display_name: row.get(1)?,
        enrolled_at: row.get(2)?,
        revoked_at: row.get(3)?,
        last_seen_at: row.get(4)?,
        archived_at: row.get(5)?,
        desired_version: row.get(6)?,
        collector_version: row.get(7)?,
        collector_protocol_version: row.get(8)?,
        last_sync_at: row.get(9)?,
        diagnostics_json: row.get(10)?,
        diagnostics_status: row.get(11)?,
        diagnostics_at: row.get(12)?,
        state_revision: row.get(13)?,
        credentials_active: row.get::<_, i64>(14)? as u64,
        credentials_total: row.get::<_, i64>(15)? as u64,
        pending_action: row.get(16)?,
        usage_event_count: row.get::<_, i64>(17)? as u64,
    })
}

impl MachineRow {
    fn into_record(self, now: DateTime<Utc>) -> MachineRecord {
        let archived = self.archived_at.is_some() || self.revoked_at.is_some();
        let diagnostics = self.diagnostics_json.as_deref().and_then(parse_diagnostics);
        let diagnostics_action_required = self
            .diagnostics_status
            .as_deref()
            .is_some_and(|status| matches!(status, "error" | "degraded" | "action-required"))
            || diagnostics.as_ref().is_some_and(|diagnostic| {
                diagnostic.watcher_degraded
                    || diagnostic.credential_rotation_pending
                    || diagnostic.terminal_outbox > 0
            });
        let protocol = ProtocolCompatibility::for_version(self.collector_protocol_version);
        let pending = self.pending_action.is_some();
        let status = derive_machine_health(
            MachineHealthInput {
                archived,
                last_seen_at: self.last_seen_at.as_deref(),
                last_sync_at: self.last_sync_at.as_deref(),
                pending_action: pending,
                diagnostics_action_required,
                credentials_active: self.credentials_active,
                protocol,
                desired_version: self.desired_version.as_deref(),
                current_version: self.collector_version.as_deref(),
            },
            now,
        );
        let status_reason = match status {
            MachineHealth::Archived => "archived; Collector credentials revoked".to_string(),
            MachineHealth::ActionRequired if !protocol.is_supported() => {
                "Collector protocol is outside the current/previous compatibility window"
                    .to_string()
            }
            MachineHealth::ActionRequired if self.credentials_active == 0 => {
                "no active Collector credential; repair or re-enroll the Machine".to_string()
            }
            MachineHealth::ActionRequired => self
                .diagnostics_status
                .clone()
                .unwrap_or_else(|| "Collector diagnostics require attention".to_string()),
            MachineHealth::UpdateAvailable => format!(
                "Collector update {} is staged; current version is {}",
                self.desired_version.as_deref().unwrap_or("unknown"),
                self.collector_version.as_deref().unwrap_or("unknown")
            ),
            MachineHealth::Syncing => {
                "an owner action is waiting for Collector acknowledgement".to_string()
            }
            MachineHealth::Stale => {
                "last Collector observation is older than five minutes".to_string()
            }
            MachineHealth::Offline => "no recent Collector observation".to_string(),
            MachineHealth::Online => "last sync and credential are healthy".to_string(),
        };
        MachineRecord {
            machine_id: self.machine_id,
            display_name: self.display_name,
            lifecycle: if archived {
                MachineLifecycle::Archived
            } else {
                MachineLifecycle::Active
            },
            status,
            status_reason,
            enrolled_at: self.enrolled_at,
            archived_at: self.archived_at,
            last_seen_at: self.last_seen_at,
            last_sync_at: self.last_sync_at,
            collector_version: self.collector_version,
            desired_version: self.desired_version,
            collector_protocol_version: self.collector_protocol_version,
            protocol_compatibility: protocol,
            diagnostics_status: self.diagnostics_status,
            diagnostics_at: self.diagnostics_at,
            diagnostics,
            credentials_active: self.credentials_active,
            credentials_total: self.credentials_total,
            pending_action: self.pending_action.and_then(|value| {
                serde_json::from_str::<OwnerCommand>(&value)
                    .ok()
                    .map(|command| command.command_id().to_string())
            }),
            usage_event_count: self.usage_event_count,
            state_revision: self.state_revision,
        }
    }
}

fn parse_diagnostics(value: &str) -> Option<MachineDiagnostics> {
    let json: serde_json::Value = serde_json::from_str(value).ok()?;
    let object = json
        .get("Diagnostics")
        .or_else(|| json.get("diagnostics"))
        .unwrap_or(&json);
    Some(MachineDiagnostics {
        watcher_degraded: object
            .pointer("/watcher/degraded")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        credential_rotation_pending: object
            .get("credential_rotation_pending")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        terminal_outbox: object
            .get("terminal_outbox")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        pending_outbox: object
            .get("pending_outbox")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        last_reconciliation_at: object
            .get("last_reconciliation_at")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        last_error: object
            .pointer("/watcher/last_error")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
    })
}

fn validate_update_request(request: &FleetUpdateRequest) -> HubResult<()> {
    if !version_is_safe(&request.version)
        || !digest_is_safe(&request.artifact_sha256)
        || !digest_is_safe(
            request
                .publisher_fingerprint
                .strip_prefix("sha256:")
                .unwrap_or(""),
        )
        || !digest_is_safe(&request.manifest_sha256)
        || request.publisher_key_id.trim().is_empty()
    {
        return Err(HubError::unprocessable(
            "invalid-signed-update",
            "signed update evidence must contain a safe version and SHA-256 digests",
        ));
    }
    Ok(())
}

fn update_run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FleetUpdateRun> {
    Ok(FleetUpdateRun {
        update_id: row.get(0)?,
        version: row.get(1)?,
        artifact_sha256: row.get(2)?,
        publisher_key_id: row.get(3)?,
        publisher_fingerprint: row.get(4)?,
        manifest_sha256: row.get(5)?,
        status: row.get(6)?,
        created_at: row.get(7)?,
        started_at: row.get(8)?,
        hub_snapshot_at: row.get(9)?,
        hub_restart_requested_at: row.get(10)?,
        hub_updated_at: row.get(11)?,
        hub_health_at: row.get(12)?,
        completed_at: row.get(13)?,
        failure_reason: row.get(14)?,
        attempts: row.get(15)?,
        state_revision: row.get(16)?,
        nodes: Vec::new(),
    })
}

fn update_node_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FleetUpdateNode> {
    let evidence_json: Option<String> = row.get(13)?;
    Ok(FleetUpdateNode {
        update_id: row.get(0)?,
        machine_id: row.get(1)?,
        status: row.get(2)?,
        previous_version: row.get(3)?,
        previous_desired_version: row.get(4)?,
        previous_runtime_generation: row.get(5)?,
        snapshot_at: row.get(6)?,
        update_started_at: row.get(7)?,
        restarted_at: row.get(8)?,
        health_checked_at: row.get(9)?,
        rolled_back_at: row.get(10)?,
        failure_reason: row.get(11)?,
        collector_protocol_version: row.get(12)?,
        receipt: evidence_json.and_then(|value| serde_json::from_str(&value).ok()),
        attempts: row.get(14)?,
        state_revision: row.get(15)?,
    })
}

/// A typed executor used by callers that can run the Phase 4 DeploymentRunner
/// for the Hub and its existing Collector command transport for each node.
/// It encodes the ordering and failure-isolation contract without accepting
/// arbitrary SSH strings or digest-only success claims.
pub trait FleetUpdateExecutor {
    fn snapshot_hub(&mut self, evidence: &FleetUpdateEvidence) -> AnyResult<()>;
    fn update_hub(&mut self, evidence: &FleetUpdateEvidence) -> AnyResult<()>;
    fn health_check_hub(&mut self, evidence: &FleetUpdateEvidence) -> AnyResult<()>;
    fn snapshot_collector(
        &mut self,
        machine_id: &str,
        previous_version: Option<&str>,
    ) -> AnyResult<()>;
    fn update_collector(
        &mut self,
        machine_id: &str,
        evidence: &FleetUpdateEvidence,
    ) -> AnyResult<()>;
    fn restart_and_health_check_collector(
        &mut self,
        machine_id: &str,
        expected_version: &str,
        evidence: &FleetUpdateEvidence,
    ) -> AnyResult<()>;
    fn rollback_collector(
        &mut self,
        machine_id: &str,
        previous_version: Option<&str>,
    ) -> AnyResult<()>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetUpdateRequestNode {
    pub machine_id: String,
    pub previous_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetUpdateReport {
    pub hub_updated: bool,
    pub hub_healthy: bool,
    pub collectors: Vec<FleetUpdateNodeResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetUpdateNodeResult {
    pub machine_id: String,
    pub status: String,
    pub previous_version: Option<String>,
    pub failure_reason: Option<String>,
}

pub struct FleetUpdateCoordinator<E> {
    executor: E,
}

impl<E> FleetUpdateCoordinator<E> {
    pub fn new(executor: E) -> Self {
        Self { executor }
    }

    pub fn executor(&self) -> &E {
        &self.executor
    }

    pub fn executor_mut(&mut self) -> &mut E {
        &mut self.executor
    }
}

impl<E: FleetUpdateExecutor> FleetUpdateCoordinator<E> {
    pub fn execute(
        &mut self,
        artifact: &crate::deployment::VerifiedArtifact,
        target_version: &str,
        nodes: &[FleetUpdateRequestNode],
    ) -> AnyResult<FleetUpdateReport> {
        let evidence = FleetUpdateEvidence::from_verified_artifact(artifact);
        self.execute_evidence(&evidence, target_version, nodes)
    }

    /// Execute an already server-verified plan. Production callers obtain
    /// `FleetUpdateEvidence` only from a configured publisher policy; this
    /// method is also the seam used by deterministic executor tests.
    pub fn execute_verified_evidence(
        &mut self,
        evidence: &FleetUpdateEvidence,
        target_version: &str,
        nodes: &[FleetUpdateRequestNode],
    ) -> AnyResult<FleetUpdateReport> {
        self.execute_evidence(evidence, target_version, nodes)
    }

    fn execute_evidence(
        &mut self,
        evidence: &FleetUpdateEvidence,
        target_version: &str,
        nodes: &[FleetUpdateRequestNode],
    ) -> AnyResult<FleetUpdateReport> {
        if !evidence.publisher_verified || evidence.version != target_version {
            bail!("fleet update requires signed evidence for the requested version");
        }
        self.executor.snapshot_hub(evidence)?;
        self.executor.update_hub(evidence)?;
        self.executor.health_check_hub(evidence)?;
        let mut results = Vec::with_capacity(nodes.len());
        for node in nodes {
            let result = self.execute_collector(evidence, target_version, node);
            results.push(result);
        }
        Ok(FleetUpdateReport {
            hub_updated: true,
            hub_healthy: true,
            collectors: results,
        })
    }

    fn execute_collector(
        &mut self,
        evidence: &FleetUpdateEvidence,
        target_version: &str,
        node: &FleetUpdateRequestNode,
    ) -> FleetUpdateNodeResult {
        let failed = |reason: String| FleetUpdateNodeResult {
            machine_id: node.machine_id.clone(),
            status: "rolled-back".to_string(),
            previous_version: node.previous_version.clone(),
            failure_reason: Some(reason),
        };
        if let Err(error) = self
            .executor
            .snapshot_collector(&node.machine_id, node.previous_version.as_deref())
        {
            return failed(format!("Collector snapshot failed: {error}"));
        }
        if let Err(error) = self.executor.update_collector(&node.machine_id, evidence) {
            let rollback = self
                .executor
                .rollback_collector(&node.machine_id, node.previous_version.as_deref());
            return failed(match rollback {
                Ok(()) => format!("Collector update failed: {error}"),
                Err(rollback_error) => {
                    format!("Collector update failed: {error}; rollback failed: {rollback_error}")
                }
            });
        }
        if let Err(error) = self.executor.restart_and_health_check_collector(
            &node.machine_id,
            target_version,
            evidence,
        ) {
            let rollback = self
                .executor
                .rollback_collector(&node.machine_id, node.previous_version.as_deref());
            return failed(match rollback {
                Ok(()) => format!("Collector restart/health failed: {error}"),
                Err(rollback_error) => format!(
                    "Collector restart/health failed: {error}; rollback failed: {rollback_error}"
                ),
            });
        }
        FleetUpdateNodeResult {
            machine_id: node.machine_id.clone(),
            status: "succeeded".to_string(),
            previous_version: node.previous_version.clone(),
            failure_reason: None,
        }
    }
}
