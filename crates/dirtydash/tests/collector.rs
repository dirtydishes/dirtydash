use std::collections::HashSet;
use std::fs;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dirtydash::collector::{
    ApprovedUpdate, Collector, CollectorOptions, CollectorTransport, CommandOutcome, RetryClass,
    RetryPolicy, TransportError, OWNER_COMMAND_LONG_POLL,
};
use dirtydash::config::SourceRoot;
use dirtydash::db::Database;
use dirtydash::hub::{IngestBatchRequest, IngestBatchResponse, OwnerCommand};
use dirtydash::importers::{self, DetectedSource, SourceKind};
use rusqlite::params;
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

fn fixture_path(agent: &str, file: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(agent)
        .join(file)
}

fn make_collector() -> (
    tempfile::TempDir,
    Collector,
    Vec<(SourceKind, std::path::PathBuf)>,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let dir = tempdir().unwrap();
    let mut roots = Vec::new();
    let fixtures = [
        (SourceKind::ClaudeCode, "claude-code", "session.jsonl"),
        (SourceKind::Codex, "codex", "session.jsonl"),
        (SourceKind::OpenCode, "opencode", "message.json"),
        (SourceKind::PiAgent, "pi", "session.jsonl"),
        (SourceKind::HermesAgent, "hermes-agent", "session.jsonl"),
    ];
    for (kind, fixture_dir, file) in fixtures {
        let root = dir.path().join(kind.as_str());
        fs::create_dir_all(&root).unwrap();
        fs::copy(fixture_path(fixture_dir, file), root.join(file)).unwrap();
        roots.push((kind, root));
    }
    let options = CollectorOptions {
        source_roots: roots
            .iter()
            .map(|(kind, path)| SourceRoot {
                kind: kind.as_str().to_string(),
                path: path.clone(),
            })
            .collect(),
        machine_id: Some("machine-fixtures".to_string()),
        credential_token: Some("ddcol_fixture.secret".to_string()),
        retry_policy: RetryPolicy {
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(8),
        },
        approved_updates: vec![ApprovedUpdate {
            version: "0.1.2".to_string(),
            sha256: "a".repeat(64),
        }],
        ..CollectorOptions::default()
    };
    let usage_path = dir.path().join("usage.sqlite3");
    let collector_path = dir.path().join("collector.sqlite3");
    let usage = Database::open(&usage_path).unwrap();
    let collector_db = Database::open(&collector_path).unwrap();
    let collector = Collector::with_databases(usage, collector_db, options).unwrap();
    (dir, collector, roots, usage_path, collector_path)
}

#[derive(Default)]
struct CredentialFallbackTransport {
    seen_credentials: Vec<String>,
}

impl CollectorTransport for CredentialFallbackTransport {
    fn send_batch(
        &mut self,
        credential_token: &str,
        request: &IngestBatchRequest,
    ) -> Result<IngestBatchResponse, TransportError> {
        self.seen_credentials.push(credential_token.to_string());
        if credential_token.starts_with("ddcol_rotation-fallback.") {
            return Err(TransportError::new(
                RetryClass::Unauthorized,
                "pending credential not active yet",
            ));
        }
        Ok(IngestBatchResponse {
            batch_id: request.batch_id.clone(),
            inserted_events: request.events.len() as u64,
            updated_events: 0,
            skipped_events: 0,
            idempotent_replay: false,
            committed_at: "2026-07-15T00:00:00Z".to_string(),
        })
    }

    fn poll_owner_command(
        &mut self,
        _credential_token: &str,
        _machine_id: &str,
        _wait: Duration,
    ) -> Result<Option<OwnerCommand>, TransportError> {
        Ok(None)
    }
}

#[derive(Default)]
struct FakeTransport {
    responses: Vec<Result<IngestBatchResponse, TransportError>>,
    command: Option<OwnerCommand>,
    sent: Vec<IngestBatchRequest>,
    seen_event_ids: HashSet<String>,
    unique_events_arrived: usize,
    poll_wait: Option<Duration>,
    acknowledgements: usize,
}

impl CollectorTransport for FakeTransport {
    fn send_batch(
        &mut self,
        _credential_token: &str,
        request: &IngestBatchRequest,
    ) -> Result<IngestBatchResponse, TransportError> {
        self.sent.push(request.clone());
        self.unique_events_arrived += request
            .events
            .iter()
            .filter(|event| {
                self.seen_event_ids.insert(format!(
                    "{}:{}",
                    event.agent, event.collector_event_fingerprint
                ))
            })
            .count();
        self.responses.pop().unwrap_or_else(|| {
            Ok(IngestBatchResponse {
                batch_id: request.batch_id.clone(),
                inserted_events: request.events.len() as u64,
                updated_events: 0,
                skipped_events: 0,
                idempotent_replay: false,
                committed_at: "2026-07-15T00:00:00Z".to_string(),
            })
        })
    }

    fn poll_owner_command(
        &mut self,
        _credential_token: &str,
        _machine_id: &str,
        wait: Duration,
    ) -> Result<Option<OwnerCommand>, TransportError> {
        self.poll_wait = Some(wait);
        Ok(self.command.clone())
    }

    fn acknowledge_owner_command(
        &mut self,
        _credential_token: &str,
        _machine_id: &str,
        _command_id: &str,
        _result: &CommandOutcome,
    ) -> Result<(), TransportError> {
        self.acknowledgements += 1;
        Ok(())
    }
}

struct RotationTransport {
    command: OwnerCommand,
    activations: Vec<(String, String, String)>,
    proofs: Vec<(String, String)>,
    acknowledgements: Vec<CommandOutcome>,
    fail_first_proof: bool,
}

impl CollectorTransport for RotationTransport {
    fn send_batch(
        &mut self,
        _credential_token: &str,
        request: &IngestBatchRequest,
    ) -> Result<IngestBatchResponse, TransportError> {
        Ok(IngestBatchResponse {
            batch_id: request.batch_id.clone(),
            inserted_events: request.events.len() as u64,
            updated_events: 0,
            skipped_events: 0,
            idempotent_replay: false,
            committed_at: "2026-07-15T00:00:00Z".to_string(),
        })
    }

    fn poll_owner_command(
        &mut self,
        _credential_token: &str,
        _machine_id: &str,
        _wait: Duration,
    ) -> Result<Option<OwnerCommand>, TransportError> {
        Ok(Some(self.command.clone()))
    }

    fn acknowledge_owner_command(
        &mut self,
        _credential_token: &str,
        _machine_id: &str,
        _command_id: &str,
        result: &CommandOutcome,
    ) -> Result<(), TransportError> {
        self.acknowledgements.push(result.clone());
        Ok(())
    }

    fn activate_collector_credential_rotation(
        &mut self,
        credential_token: &str,
        _machine_id: &str,
        rotation_id: &str,
        replacement_secret: &str,
    ) -> Result<(), TransportError> {
        self.activations.push((
            credential_token.to_string(),
            rotation_id.to_string(),
            replacement_secret.to_string(),
        ));
        Ok(())
    }

    fn prove_collector_credential_rotation(
        &mut self,
        replacement_token: &str,
        _machine_id: &str,
        rotation_id: &str,
    ) -> Result<(), TransportError> {
        self.proofs
            .push((replacement_token.to_string(), rotation_id.to_string()));
        if self.fail_first_proof {
            self.fail_first_proof = false;
            return Err(TransportError::offline("rotation proof response lost"));
        }
        Ok(())
    }
}

#[test]
fn five_real_agent_fixtures_parse_and_transport_only_redacted_metadata() {
    let (_dir, mut collector, roots, _usage_path, collector_path) = make_collector();
    let nested = json!({
        "model": "NESTED_MODEL_SECRET_SENTINEL",
        "provider": "NESTED_PROVIDER_SECRET_SENTINEL",
        "ssh": "NESTED_SSH_SECRET_SENTINEL",
        "sudo": "NESTED_SUDO_SECRET_SENTINEL",
        "authorization": "NESTED_AUTHORIZATION_SECRET_SENTINEL"
    });
    fs::write(
        roots[0].1.join("nested.jsonl"),
        json!({
            "sessionId": "nested-claude",
            "message": {"content": nested.clone()}
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        roots[1].1.join("nested.jsonl"),
        json!({
            "type": "event_msg",
            "payload": {"type": "turn_context", "metadata": nested.clone()}
        })
        .to_string(),
    )
    .unwrap();
    let mut opencode: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(roots[2].1.join("message.json")).unwrap())
            .unwrap();
    opencode["metadata"] = nested.clone();
    fs::write(
        roots[2].1.join("message.json"),
        serde_json::to_string(&opencode).unwrap(),
    )
    .unwrap();
    fs::write(
        roots[3].1.join("nested.jsonl"),
        json!({
            "type": "message",
            "id": "nested-pi",
            "provider": "safe-provider",
            "model": "safe-model",
            "message": {"content": nested.clone()}
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        roots[4].1.join("nested.jsonl"),
        json!({"type": "turn", "session_id": "nested-hermes", "content": nested}).to_string(),
    )
    .unwrap();

    let report = collector
        .reconcile_manual(at("2026-07-15T00:00:00Z"))
        .unwrap();
    assert!(report.source_count >= 5);
    assert_eq!(report.events_queued, 6);
    assert_eq!(report.parse_errors, 3);

    let payload = collector.diagnostics().unwrap();
    assert_eq!(payload.parser_versions.len(), 5);

    let collector_db = Database::open(collector_path).unwrap();
    let rows = collector_db
        .collector_outbox_ready("2026-07-15T00:00:01Z", 10)
        .unwrap();
    assert_eq!(rows.len(), 1);
    let json = &rows[0].payload_json;
    for sentinel in [
        "SENTINEL_RAW_PROMPT_SHOULD_NOT_TRAVEL",
        "SENTINEL_PI_RESPONSE_SHOULD_NOT_TRAVEL",
        "SENTINEL_HERMES_MESSAGE_SHOULD_NOT_TRAVEL",
        "/home/example/secret-project",
        "/Users/example/secret-project",
        "C:\\\\Users\\\\example",
        "\\\\server\\share\\private-project",
        "NESTED_MODEL_SECRET_SENTINEL",
        "NESTED_PROVIDER_SECRET_SENTINEL",
        "NESTED_SSH_SECRET_SENTINEL",
        "NESTED_SUDO_SECRET_SENTINEL",
        "NESTED_AUTHORIZATION_SECRET_SENTINEL",
    ] {
        assert!(!json.contains(sentinel), "payload leaked {sentinel}");
    }
    assert!(!json.contains("raw_path"));
    assert!(!json.contains("raw_span"));
    assert!(json.matches("\"metadata_only\":true").count() == 6);
    assert!(json.contains("project-"));
    let request: IngestBatchRequest = serde_json::from_str(json).unwrap();
    assert!(request.events.iter().any(|event| event.confidence == 0.98));
    assert!(request.events.iter().any(|event| event.confidence < 0.7));
    assert!(request.events.iter().all(|event| event.metadata_only));
    assert!(request
        .events
        .iter()
        .all(|event| event.parser_version.ends_with("-v1")));
}

#[test]
fn offline_restart_replay_and_hub_identity_are_at_least_once_without_duplicates() {
    let (dir, mut collector, _roots, _usage_path, _collector_path) = make_collector();
    collector
        .reconcile_manual(at("2026-07-15T00:00:00Z"))
        .unwrap();
    let mut offline = FakeTransport {
        responses: vec![Err(TransportError::offline("Hub is offline"))],
        ..FakeTransport::default()
    };
    let failed = collector
        .deliver_pending(&mut offline, at("2026-07-15T00:00:01Z"))
        .unwrap();
    assert_eq!(failed.failed, 1);
    assert_eq!(failed.pending, 1);
    assert_eq!(
        failed.next_retry_at.as_deref(),
        Some("2026-07-15T00:00:02+00:00")
    );

    let usage = Database::open(dir.path().join("usage.sqlite3")).unwrap();
    let collector_db = Database::open(dir.path().join("collector.sqlite3")).unwrap();
    let options = CollectorOptions {
        machine_id: Some("machine-fixtures".to_string()),
        credential_token: Some("ddcol_fixture.secret".to_string()),
        ..CollectorOptions::default()
    };
    let mut restarted = Collector::with_databases(usage, collector_db, options).unwrap();
    let mut online = FakeTransport::default();
    let delivered = restarted
        .deliver_pending(&mut online, at("2026-07-15T00:00:02Z"))
        .unwrap();
    assert_eq!(delivered.acknowledged, 1);
    assert_eq!(delivered.pending, 0);
    assert_eq!(online.unique_events_arrived, 6);

    // A second durable replay of the same request would be idempotent at the
    // Hub because identity is agent + collector fingerprint.
}

#[test]
fn parser_upgrade_reprocesses_manifest_but_reuses_fingerprint() {
    let (_dir, mut collector, roots, usage_path, collector_path) = make_collector();
    let first = collector
        .reconcile_startup(at("2026-07-15T00:00:00Z"))
        .unwrap();
    assert_eq!(first.files_reprocessed, 5);
    let usage = Database::open(usage_path).unwrap();
    let source = roots[0].1.join("session.jsonl");
    let detected = DetectedSource {
        kind: SourceKind::ClaudeCode,
        path: roots[0].1.clone(),
        confidence: "high".to_string(),
        file_count: 1,
        harness_names: vec!["Claude Code".to_string()],
    };
    let before = importers::parse_source_file_for_collector(
        &usage,
        &detected,
        &source,
        "machine-fixtures",
        "2026-07-15T00:00:00Z",
    )
    .unwrap()
    .events[0]
        .raw_event_hash
        .clone();
    let source_key = dirtydash::collector::redacted_identifier(
        collector.project_salt(),
        "source",
        &format!("{}|{}", detected.kind.as_str(), source.display()),
    );
    let mut manifest = Database::open(collector_path.clone())
        .unwrap()
        .collector_manifest(&source_key)
        .unwrap()
        .unwrap();
    manifest.parser_version = "old-parser-v0".to_string();
    Database::open(collector_path)
        .unwrap()
        .upsert_collector_manifest(&manifest)
        .unwrap();
    let second = collector
        .reconcile_startup(at("2026-07-15T00:15:00Z"))
        .unwrap();
    assert!(second.files_reprocessed >= 1);
    let after_parsed = importers::parse_source_file_for_collector(
        &usage,
        &detected,
        &source,
        "machine-fixtures",
        "2026-07-15T00:15:00Z",
    )
    .unwrap();
    assert_eq!(before, after_parsed.events[0].raw_event_hash);

    let relocated = roots[0].1.join("relocated-session.jsonl");
    let original_bytes = fs::read_to_string(&source).unwrap();
    // Move the unchanged record after an unrelated valid metadata record;
    // line position must not participate in canonical Collector identity.
    fs::write(
        &relocated,
        format!(
            "{}\n{}",
            r#"{"sessionId":"unrelated-metadata","type":"system"}"#, original_bytes
        ),
    )
    .unwrap();
    let relocated_source = DetectedSource {
        path: roots[0].1.clone(),
        ..detected.clone()
    };
    let relocated_parsed = importers::parse_source_file_for_collector(
        &usage,
        &relocated_source,
        &relocated,
        "machine-fixtures",
        "2026-07-15T00:15:00Z",
    )
    .unwrap();
    assert_eq!(
        importers::stable_event_fingerprint(&after_parsed.events[0]),
        importers::stable_event_fingerprint(&relocated_parsed.events[0])
    );
}

#[test]
fn unchanged_manifests_and_tombstones_are_coalesced() {
    let (_dir, mut collector, roots, _usage_path, collector_path) = make_collector();
    let first = collector
        .reconcile_startup(at("2026-07-15T00:00:00Z"))
        .unwrap();
    assert!(first.batch_id.is_some());
    let second = collector
        .reconcile_startup(at("2026-07-15T00:15:00Z"))
        .unwrap();
    assert!(second.batch_id.is_none());
    let store = Database::open(collector_path.clone()).unwrap();
    assert_eq!(store.collector_outbox_count().unwrap(), 1);

    fs::remove_file(roots[0].1.join("session.jsonl")).unwrap();
    let tombstone = collector
        .reconcile_startup(at("2026-07-15T00:30:00Z"))
        .unwrap();
    assert!(tombstone.batch_id.is_some());
    let repeated = collector
        .reconcile_startup(at("2026-07-15T00:45:00Z"))
        .unwrap();
    assert!(repeated.batch_id.is_none());
    assert_eq!(store.collector_outbox_count().unwrap(), 2);
}

#[test]
fn repeated_startup_manual_and_refresh_reconcile_without_outbox_growth_or_lost_updates() {
    let (_dir, mut collector, roots, _usage_path, collector_path) = make_collector();
    let store = Database::open(collector_path).unwrap();

    let first = collector
        .reconcile_startup(at("2026-07-15T00:00:00Z"))
        .unwrap();
    assert_eq!(first.events_queued, 6);
    assert_eq!(store.collector_outbox_count().unwrap(), 1);

    let repeated_startup = collector
        .reconcile_startup(at("2026-07-15T00:00:01Z"))
        .unwrap();
    assert_eq!(repeated_startup.events_queued, 0);
    assert_eq!(store.collector_outbox_count().unwrap(), 1);

    let manual_before_delivery = collector
        .reconcile_manual(at("2026-07-15T00:00:02Z"))
        .unwrap();
    assert_eq!(manual_before_delivery.events_queued, 0);
    assert_eq!(store.collector_outbox_count().unwrap(), 1);

    let mut transport = FakeTransport::default();
    let delivered = collector
        .deliver_pending(&mut transport, at("2026-07-15T00:00:03Z"))
        .unwrap();
    assert_eq!(delivered.acknowledged, 1);
    assert_eq!(store.collector_outbox_count().unwrap(), 0);
    assert!(store
        .collector_event_manifests()
        .unwrap()
        .iter()
        .all(|record| record.status == "delivered"));

    let manual_after_delivery = collector
        .reconcile_manual(at("2026-07-15T00:00:04Z"))
        .unwrap();
    assert_eq!(manual_after_delivery.events_queued, 0);
    assert_eq!(store.collector_outbox_count().unwrap(), 0);

    transport.command = Some(OwnerCommand::Refresh {
        command_id: "refresh-idempotent".to_string(),
    });
    let refreshed = collector
        .poll_owner_command(&mut transport, at("2026-07-15T00:00:05Z"))
        .unwrap()
        .unwrap();
    assert!(matches!(
        refreshed,
        CommandOutcome::Refreshed { batch_id: None }
    ));
    assert_eq!(store.collector_outbox_count().unwrap(), 0);
    let replayed_refresh = collector
        .poll_owner_command(&mut transport, at("2026-07-15T00:00:06Z"))
        .unwrap()
        .unwrap();
    assert!(matches!(
        replayed_refresh,
        CommandOutcome::Refreshed { batch_id: None }
    ));
    assert_eq!(store.collector_outbox_count().unwrap(), 0);

    let source = roots[0].1.join("session.jsonl");
    let original = fs::read_to_string(&source).unwrap();
    fs::write(
        &source,
        format!(
            "{}\n{}",
            original,
            r#"{"sessionId":"new-update","cwd":"/private/project","timestamp":"2026-07-15T00:00:07Z","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":101,"output_tokens":21}}}"#
        ),
    )
    .unwrap();
    let update = collector
        .reconcile_manual(at("2026-07-15T00:00:07Z"))
        .unwrap();
    assert_eq!(update.events_queued, 1);
    assert_eq!(store.collector_outbox_count().unwrap(), 1);
    let delivered_update = collector
        .deliver_pending(&mut transport, at("2026-07-15T00:00:08Z"))
        .unwrap();
    assert_eq!(delivered_update.acknowledged, 1);
    assert_eq!(transport.unique_events_arrived, 7);
    assert_eq!(store.collector_outbox_count().unwrap(), 0);
    assert!(store
        .collector_event_manifests()
        .unwrap()
        .iter()
        .all(|record| record.status == "delivered"));
}

#[test]
fn missing_source_timestamps_do_not_make_refresh_events_change() {
    let (_dir, mut collector, roots, _usage_path, collector_path) = make_collector();
    let source = roots[0].1.join("missing-timestamp.jsonl");
    fs::write(
        &source,
        r#"{"sessionId":"missing-timestamp","cwd":"/private/project","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":3,"output_tokens":2}}}"#,
    )
    .unwrap();
    let first = collector
        .reconcile_manual(at("2026-07-15T00:00:00Z"))
        .unwrap();
    assert!(first.events_queued >= 1);
    let mut transport = FakeTransport::default();
    assert_eq!(
        collector
            .deliver_pending(&mut transport, at("2026-07-15T00:00:01Z"))
            .unwrap()
            .acknowledged,
        1
    );
    let second = collector
        .reconcile_manual(at("2026-07-15T00:00:02Z"))
        .unwrap();
    assert_eq!(second.events_queued, 0);
    assert_eq!(
        Database::open(collector_path)
            .unwrap()
            .collector_outbox_count()
            .unwrap(),
        0
    );
}

#[test]
fn hermes_state_database_rows_are_parsed_as_metadata() {
    let dir = tempdir().unwrap();
    let hermes_root = dir.path().join("hermes");
    fs::create_dir_all(&hermes_root).unwrap();
    let state = hermes_root.join("state.db");
    let connection = rusqlite::Connection::open(&state).unwrap();
    connection
        .execute(
            "CREATE TABLE sessions (id TEXT, cwd TEXT, provider TEXT, model TEXT, input_tokens INTEGER, output_tokens INTEGER, cache_read_tokens INTEGER, cache_write_tokens INTEGER, reasoning_tokens INTEGER, cost REAL, created_at TEXT)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sessions VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                "hermes-db-session",
                "/private/hermes/project",
                "anthropic",
                "claude-sonnet-4-6",
                1000,
                200,
                300,
                10,
                20,
                0.0042_f64,
                "2026-07-15T00:00:00Z"
            ],
        )
        .unwrap();
    drop(connection);

    let usage_path = dir.path().join("usage.sqlite3");
    let collector_path = dir.path().join("collector.sqlite3");
    let collector = Collector::with_databases(
        Database::open(&usage_path).unwrap(),
        Database::open(&collector_path).unwrap(),
        CollectorOptions {
            source_roots: vec![SourceRoot {
                kind: "hermes".to_string(),
                path: hermes_root,
            }],
            machine_id: Some("machine-hermes-db".to_string()),
            credential_token: Some("ddcol_fixture.secret".to_string()),
            ..CollectorOptions::default()
        },
    )
    .unwrap();
    let mut collector = collector;
    let report = collector
        .reconcile_startup(at("2026-07-15T00:01:00Z"))
        .unwrap();
    assert_eq!(report.events_queued, 1);
    let row = Database::open(usage_path)
        .unwrap()
        .connection()
        .unwrap()
        .query_row(
            "SELECT source, session_id, prompt_tokens, cache_read_tokens, pricing_version FROM usage_events",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row.0, "hermes-agent");
    assert_eq!(row.1, "hermes-db-session");
    assert_eq!(row.2, 1000);
    assert_eq!(row.3, 300);
    assert_eq!(row.4, "reported-cost");
}

#[test]
fn restricted_updater_verifies_bytes_and_applies_atomically() {
    let dir = tempdir().unwrap();
    let artifact = b"approved-artifact-bytes";
    let digest = hex::encode(Sha256::digest(artifact));
    let target = dir.path().join("installed/dirtydash.bin");
    let collector = Collector::with_databases(
        Database::open(dir.path().join("usage.sqlite3")).unwrap(),
        Database::open(dir.path().join("collector.sqlite3")).unwrap(),
        CollectorOptions {
            machine_id: Some("machine-update".to_string()),
            credential_token: Some("ddcol_fixture.secret".to_string()),
            approved_updates: vec![ApprovedUpdate {
                version: "0.1.2".to_string(),
                sha256: digest.clone(),
            }],
            update_target: Some(target.clone()),
            ..CollectorOptions::default()
        },
    )
    .unwrap();
    collector
        .apply_approved_update_artifact("0.1.2", &digest, artifact)
        .unwrap();
    assert_eq!(fs::read(&target).unwrap(), artifact);
    assert!(collector
        .apply_approved_update_artifact("0.1.2", &"0".repeat(64), b"unapproved")
        .is_err());
    assert_eq!(fs::read(&target).unwrap(), artifact);
}

#[test]
fn watcher_debounce_fallback_commands_and_update_allowlist_are_visible() {
    let (_dir, mut collector, _roots, _usage_path, _collector_path) = make_collector();
    let now = at("2026-07-15T00:00:00Z");
    collector.notify_watcher_hint(now);
    collector.notify_watcher_hint(now + chrono::Duration::milliseconds(100));
    assert!(collector
        .reconcile_if_due(now + chrono::Duration::milliseconds(400))
        .unwrap()
        .is_none());
    assert!(collector
        .reconcile_if_due(now + chrono::Duration::milliseconds(700))
        .unwrap()
        .is_some());

    collector.watcher_failed(now, "/home/private/watcher failure");
    assert!(collector.watcher_status().degraded);
    let watcher_error = collector.watcher_status().last_error.unwrap();
    assert!(watcher_error.starts_with("diagnostic-"));
    assert!(!watcher_error.contains("home_private"));
    assert!(collector.reconciliation_due(now));

    let mut transport = FakeTransport {
        command: Some(OwnerCommand::Diagnostics {
            command_id: "diagnostics-1".to_string(),
        }),
        ..FakeTransport::default()
    };
    let outcome = collector
        .poll_owner_command(&mut transport, now)
        .unwrap()
        .unwrap();
    assert!(matches!(
        outcome,
        CommandOutcome::Diagnostics { diagnostics: _ }
    ));
    assert_eq!(transport.poll_wait, Some(OWNER_COMMAND_LONG_POLL));
    assert_eq!(transport.acknowledgements, 1);
    transport.command = Some(OwnerCommand::Diagnostics {
        command_id: "diagnostics-1".to_string(),
    });
    assert!(matches!(
        collector.poll_owner_command(&mut transport, now).unwrap(),
        Some(CommandOutcome::Diagnostics { diagnostics: _ })
    ));
    assert_eq!(transport.acknowledgements, 2);

    transport.command = Some(OwnerCommand::ApprovedUpdate {
        command_id: "update-1".to_string(),
        update_id: "update-test-1".to_string(),
        version: "0.1.2".to_string(),
        sha256: "a".repeat(64),
    });
    assert!(matches!(
        collector.poll_owner_command(&mut transport, now).unwrap(),
        Some(CommandOutcome::Rejected { .. })
    ));
    transport.command = Some(OwnerCommand::ApprovedUpdate {
        command_id: "update-2".to_string(),
        update_id: "update-test-2".to_string(),
        version: "0.1.2".to_string(),
        sha256: "b".repeat(64),
    });
    assert!(matches!(
        collector.poll_owner_command(&mut transport, now).unwrap(),
        Some(CommandOutcome::Rejected { .. })
    ));
}

#[test]
fn credential_rotation_generates_locally_proves_after_retry_and_commits_atomically() {
    let (_dir, mut collector, _roots, usage_path, collector_path) = make_collector();
    let command = OwnerCommand::RotateCredential {
        command_id: "rotate-local-secret".to_string(),
        rotation_id: "rotation-local-secret".to_string(),
    };
    let mut transport = RotationTransport {
        command,
        activations: Vec::new(),
        proofs: Vec::new(),
        acknowledgements: Vec::new(),
        fail_first_proof: true,
    };

    assert!(collector
        .poll_owner_command(&mut transport, at("2026-07-15T00:00:00Z"))
        .is_err());
    let store = Database::open(collector_path.clone()).unwrap();
    let after_crash = store.collector_identity().unwrap().unwrap();
    assert_eq!(
        after_crash.credential_token.as_deref(),
        Some("ddcol_fixture.secret")
    );
    assert!(after_crash
        .pending_credential_token
        .as_deref()
        .is_some_and(|token| token.starts_with("ddcol_rotation-local-secret.")));
    assert_eq!(
        after_crash.pending_credential_id.as_deref(),
        Some("rotation-local-secret")
    );
    assert!(transport.activations[0].0 == "ddcol_fixture.secret");
    assert!(!transport.activations[0].2.is_empty());
    assert!(transport.proofs[0]
        .0
        .starts_with("ddcol_rotation-local-secret."));

    drop(collector);
    let mut restarted = Collector::with_databases(
        Database::open(usage_path).unwrap(),
        Database::open(collector_path.clone()).unwrap(),
        CollectorOptions {
            machine_id: Some("machine-fixtures".to_string()),
            credential_token: Some("ddcol_fixture.secret".to_string()),
            ..CollectorOptions::default()
        },
    )
    .unwrap();
    let outcome = restarted
        .poll_owner_command(&mut transport, at("2026-07-15T00:02:00Z"))
        .unwrap()
        .unwrap();
    assert_eq!(outcome, CommandOutcome::CredentialRotationStaged);
    let committed = store.collector_identity().unwrap().unwrap();
    assert!(committed
        .credential_token
        .as_deref()
        .is_some_and(|token| token.starts_with("ddcol_rotation-local-secret.")));
    assert!(committed.pending_credential_token.is_none());
    assert!(committed.pending_credential_id.is_none());
    assert_eq!(transport.activations.len(), 2);
    assert_eq!(transport.proofs.len(), 2);
    assert_eq!(transport.acknowledgements.len(), 1);
    let acknowledgement = serde_json::to_string(&transport.acknowledgements[0]).unwrap();
    assert!(!acknowledgement.contains("ddcol_fixture.secret"));
    assert!(!acknowledgement.contains("rotation-local-secret."));

    // A completed receipt is safe to acknowledge again after a response loss;
    // it does not regenerate or stage another secret.
    let replayed = restarted
        .poll_owner_command(&mut transport, at("2026-07-15T00:03:00Z"))
        .unwrap()
        .unwrap();
    assert_eq!(replayed, CommandOutcome::CredentialRotationStaged);
    assert_eq!(transport.activations.len(), 2);
    assert_eq!(transport.proofs.len(), 2);
    assert_eq!(transport.acknowledgements.len(), 2);
}

#[test]
fn pending_credential_auth_failure_falls_back_without_retiring_old_token() {
    let (_dir, mut collector, _roots, _usage_path, collector_path) = make_collector();
    collector
        .reconcile_startup(at("2026-07-15T00:00:00Z"))
        .unwrap();
    collector
        .handle_owner_command(
            OwnerCommand::RotateCredential {
                command_id: "rotate-fallback".to_string(),
                rotation_id: "rotation-fallback".to_string(),
            },
            at("2026-07-15T00:00:00Z"),
        )
        .unwrap();
    let mut transport = CredentialFallbackTransport::default();
    let report = collector
        .deliver_pending(&mut transport, at("2026-07-15T00:00:01Z"))
        .unwrap();
    assert_eq!(report.acknowledged, 1);
    assert_eq!(transport.seen_credentials.len(), 2);
    assert!(transport.seen_credentials[0].starts_with("ddcol_rotation-fallback."));
    assert_eq!(transport.seen_credentials[1], "ddcol_fixture.secret");
    let identity = Database::open(collector_path)
        .unwrap()
        .collector_identity()
        .unwrap()
        .unwrap();
    assert_eq!(
        identity.credential_token.as_deref(),
        Some("ddcol_fixture.secret")
    );
    assert!(identity.pending_credential_token.is_some());
    assert_eq!(
        identity.pending_credential_id.as_deref(),
        Some("rotation-fallback")
    );
}

#[test]
fn terminal_outbox_is_excluded_and_requires_manual_recovery() {
    let (_dir, mut collector, _roots, _usage_path, collector_path) = make_collector();
    collector
        .reconcile_startup(at("2026-07-15T00:00:00Z"))
        .unwrap();
    let mut unauthorized = FakeTransport {
        responses: vec![Err(TransportError::new(
            RetryClass::Unauthorized,
            "expired credential",
        ))],
        ..FakeTransport::default()
    };
    let report = collector
        .deliver_pending(&mut unauthorized, at("2026-07-15T00:00:01Z"))
        .unwrap();
    assert_eq!(report.pending, 0);
    assert_eq!(report.terminal, 1);
    let repeated = collector
        .reconcile_manual(at("2026-07-15T00:00:02Z"))
        .unwrap();
    assert_eq!(repeated.batch_id, None);
    let store = Database::open(collector_path).unwrap();
    assert!(store
        .collector_outbox_ready("2026-07-15T00:00:02Z", 10)
        .unwrap()
        .is_empty());
    assert!(!collector
        .recover_outbox_batch("batch-does-not-exist", at("2026-07-15T00:00:02Z"))
        .unwrap());
    let batch_id = store
        .collector_outbox_records(true)
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .batch_id;
    assert!(collector
        .recover_outbox_batch(&batch_id, at("2026-07-15T00:00:03Z"))
        .unwrap());
    assert_eq!(store.collector_outbox_count().unwrap(), 1);
}

#[test]
fn started_command_is_reclaimed_and_resumed_after_lease_expiry() {
    let (_dir, mut collector, _roots, _usage_path, collector_path) = make_collector();
    let store = Database::open(collector_path).unwrap();
    assert!(store
        .begin_collector_command_owned("reclaim-1", "2026-07-15T00:00:00Z", "crashed-owner",)
        .unwrap());
    let mut transport = FakeTransport {
        command: Some(OwnerCommand::Diagnostics {
            command_id: "reclaim-1".to_string(),
        }),
        ..FakeTransport::default()
    };
    let outcome = collector
        .poll_owner_command(&mut transport, at("2026-07-15T00:02:00Z"))
        .unwrap()
        .unwrap();
    assert!(matches!(
        outcome,
        CommandOutcome::Diagnostics { diagnostics: _ }
    ));
    assert_eq!(transport.acknowledgements, 1);
}

#[test]
fn retry_policy_is_bounded_and_single_instance_lock_is_durable() {
    let (_dir, collector, _roots, _usage_path, _collector_path) = make_collector();
    let policy = RetryPolicy {
        base_delay: Duration::from_secs(2),
        max_delay: Duration::from_secs(8),
    };
    assert_eq!(policy.delay_for(1), Duration::from_secs(2));
    assert_eq!(policy.delay_for(2), Duration::from_secs(4));
    assert_eq!(policy.delay_for(3), Duration::from_secs(8));
    assert_eq!(policy.delay_for(20), Duration::from_secs(8));
    assert!(!RetryClass::Unauthorized.is_retryable());

    let first = collector
        .acquire_instance(at("2026-07-15T00:00:00Z"))
        .unwrap();
    assert!(collector
        .acquire_instance(at("2026-07-15T00:00:01Z"))
        .is_err());
    drop(first);
    assert!(collector
        .acquire_instance(at("2026-07-15T00:00:01Z"))
        .is_ok());
}
