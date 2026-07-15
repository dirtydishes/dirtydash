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

#[test]
fn five_real_agent_fixtures_parse_and_transport_only_redacted_metadata() {
    let (_dir, mut collector, _roots, _usage_path, collector_path) = make_collector();
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
    fs::copy(&source, &relocated).unwrap();
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
    assert!(matches!(outcome, CommandOutcome::Diagnostics(_)));
    assert_eq!(transport.poll_wait, Some(OWNER_COMMAND_LONG_POLL));
    assert_eq!(transport.acknowledgements, 1);
    transport.command = Some(OwnerCommand::Diagnostics {
        command_id: "diagnostics-1".to_string(),
    });
    assert!(matches!(
        collector.poll_owner_command(&mut transport, now).unwrap(),
        Some(CommandOutcome::Diagnostics(_))
    ));
    assert_eq!(transport.acknowledgements, 2);

    transport.command = Some(OwnerCommand::ApprovedUpdate {
        command_id: "update-1".to_string(),
        version: "0.1.2".to_string(),
        sha256: "a".repeat(64),
    });
    assert!(matches!(
        collector.poll_owner_command(&mut transport, now).unwrap(),
        Some(CommandOutcome::UpdateAccepted { .. })
    ));
    transport.command = Some(OwnerCommand::ApprovedUpdate {
        command_id: "update-2".to_string(),
        version: "0.1.2".to_string(),
        sha256: "b".repeat(64),
    });
    assert!(matches!(
        collector.poll_owner_command(&mut transport, now).unwrap(),
        Some(CommandOutcome::Rejected { .. })
    ));
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
