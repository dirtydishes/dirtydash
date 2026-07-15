use std::fs;
use std::io::BufRead;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use dirtydash::deployment::{
    ArtifactArch, ArtifactDescriptor, ArtifactManifest, ArtifactOs, PublisherKey,
    SignedArtifactManifest, TargetPlatform, MANIFEST_SCHEMA_VERSION,
};
use ed25519_dalek::{Signer, SigningKey};
use predicates::str::contains;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

fn dirtydash_cmd() -> assert_cmd::Command {
    let mut command = assert_cmd::Command::cargo_bin("dirtydash").unwrap();
    command
        .env("CLAUDE_CONFIG_DIR", "/tmp/dirtydash-test-no-claude")
        .env("CODEX_HOME", "/tmp/dirtydash-test-no-codex")
        .env("OPENCODE_DATA_DIR", "/tmp/dirtydash-test-no-opencode")
        .env("PI_AGENT_DIR", "/tmp/dirtydash-test-no-pi")
        .env("HERMES_HOME", "/tmp/dirtydash-test-no-hermes");
    command
}

fn std_dirtydash_cmd() -> std::process::Command {
    let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin("dirtydash"));
    apply_clean_source_env(&mut command);
    command
}

fn apply_clean_source_env(command: &mut std::process::Command) {
    command
        .env("CLAUDE_CONFIG_DIR", "/tmp/dirtydash-test-no-claude")
        .env("CODEX_HOME", "/tmp/dirtydash-test-no-codex")
        .env("OPENCODE_DATA_DIR", "/tmp/dirtydash-test-no-opencode")
        .env("PI_AGENT_DIR", "/tmp/dirtydash-test-no-pi")
        .env("HERMES_HOME", "/tmp/dirtydash-test-no-hermes");
}

#[test]
fn scan_import_doctor_and_pricing_commands_work_from_binary() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("claude/projects/project-a");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("session.jsonl"),
        r#"{"sessionId":"cli-1","cwd":"/repo/cli","timestamp":"2026-06-06T12:00:00Z","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":1000,"output_tokens":250,"cache_read_input_tokens":100}}}"#,
    )
    .unwrap();

    let db = dir.path().join("dirtydash.sqlite3");
    let config = dir.path().join("config.toml");
    let source_root = format!(
        "claude-code={}",
        dir.path().join("claude/projects").display()
    );

    dirtydash_cmd()
        .args([
            "--db",
            db.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "--source-root",
            &source_root,
            "scan",
            "--json",
        ])
        .assert()
        .success()
        .stdout(contains("claude-code"));

    dirtydash_cmd()
        .args([
            "--db",
            db.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "--source-root",
            &source_root,
            "import",
            "--json",
        ])
        .assert()
        .success()
        .stdout(contains("\"inserted_events\": 1"));

    dirtydash_cmd()
        .args([
            "--db",
            db.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "doctor",
            "--json",
        ])
        .assert()
        .success()
        .stdout(contains("\"event_count\": 1"));

    dirtydash_cmd()
        .args([
            "--db",
            db.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "pricing",
            "list",
            "--json",
        ])
        .assert()
        .success()
        .stdout(contains("claude-sonnet-4-6"));
}

#[test]
fn collector_reconcile_and_diagnostics_use_separate_state_database() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("claude/projects/project-a");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("session.jsonl"),
        r#"{"sessionId":"collector-cli-1","cwd":"/private/project","timestamp":"2026-06-06T12:00:00Z","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":1000,"output_tokens":250}}}"#,
    )
    .unwrap();
    let db = dir.path().join("dirtydash.sqlite3");
    let config = dir.path().join("config.toml");
    let source_root = format!(
        "claude-code={}",
        dir.path().join("claude/projects").display()
    );

    dirtydash_cmd()
        .args([
            "--db",
            db.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "--source-root",
            &source_root,
            "collector",
            "reconcile",
            "--json",
        ])
        .assert()
        .success()
        .stdout(contains("\"events_queued\": 1"));

    dirtydash_cmd()
        .args([
            "--db",
            db.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "collector",
            "diagnostics",
            "--json",
        ])
        .assert()
        .success()
        .stdout(contains("\"pending_outbox\": 1"));

    let collector_db = dir.path().join("dirtydash-collector.sqlite3");
    assert!(collector_db.exists());
    let tables = rusqlite::Connection::open(collector_db)
        .unwrap()
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'usage_events'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(tables.is_empty());
}

#[test]
fn deploy_hub_plan_requires_a_durable_publisher_anchor() {
    let dir = tempdir().unwrap();
    let output = dirtydash_cmd()
        .args([
            "--db",
            dir.path().join("dirtydash.sqlite3").to_str().unwrap(),
            "--config",
            dir.path().join("config.toml").to_str().unwrap(),
            "deploy",
            "hub",
            "ssh-alias",
            "--plan",
            "--json",
            "--publisher-key-id",
            "attacker-key",
            "--publisher-fingerprint",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("durable configured publisher"));
}

#[test]
fn deploy_hub_shape_plan_is_inspectable_only_with_configured_anchor() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("config.toml"),
        "[hub]\nallowed_publisher_key_id = \"release-key\"\nallowed_publisher_fingerprint = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"\n",
    )
    .unwrap();
    let output = dirtydash_cmd()
        .args([
            "--db",
            dir.path().join("dirtydash.sqlite3").to_str().unwrap(),
            "--config",
            dir.path().join("config.toml").to_str().unwrap(),
            "deploy",
            "hub",
            "ssh-alias",
            "--plan",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("verify-signed-artifact"));
    assert!(text.contains("consent-required"));
    assert!(!text.contains("PASSWORD_SENTINEL"));
    assert!(!text.contains("SUDO_SENTINEL"));
}

#[test]
fn deploy_hub_rejects_replaced_release_evidence_and_cli_trust_flags() {
    let dir = tempdir().unwrap();
    let trusted = SigningKey::from_bytes(&[41_u8; 32]);
    let replacement = SigningKey::from_bytes(&[42_u8; 32]);
    let (manifest, artifact_dir, public_key) = write_release_fixture(dir.path(), &trusted);
    let trusted_key_id = PublisherKey::fingerprint(&trusted.verifying_key().to_bytes()).unwrap();
    let trusted_fingerprint = trusted_key_id.clone();
    fs::write(
        dir.path().join("config.toml"),
        format!(
            "[hub]\nallowed_publisher_key_id = \"{trusted_key_id}\"\nallowed_publisher_fingerprint = \"{trusted_fingerprint}\"\n"
        ),
    )
    .unwrap();

    // A replacement manifest signed by another key is rejected before any
    // host-key observation or remote mutation.
    let (replacement_manifest, _, _) = write_release_fixture(dir.path(), &replacement);
    let output = dirtydash_cmd()
        .args([
            "--db",
            dir.path().join("dirtydash.sqlite3").to_str().unwrap(),
            "--config",
            dir.path().join("config.toml").to_str().unwrap(),
            "deploy",
            "hub",
            "unreachable",
            "--plan",
            "--manifest",
            replacement_manifest.to_str().unwrap(),
            "--artifact-dir",
            artifact_dir.to_str().unwrap(),
            "--public-key",
            public_key.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("key ID"));

    // Replacing only the public-key file cannot replace the durable anchor.
    fs::write(&public_key, replacement.verifying_key().to_bytes()).unwrap();
    let output = dirtydash_cmd()
        .args([
            "--db",
            dir.path().join("dirtydash.sqlite3").to_str().unwrap(),
            "--config",
            dir.path().join("config.toml").to_str().unwrap(),
            "deploy",
            "hub",
            "unreachable",
            "--plan",
            "--manifest",
            manifest.to_str().unwrap(),
            "--artifact-dir",
            artifact_dir.to_str().unwrap(),
            "--public-key",
            public_key.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("fingerprint"));

    // A single invocation cannot replace trust with assertion flags.
    fs::write(&public_key, trusted.verifying_key().to_bytes()).unwrap();
    let output = dirtydash_cmd()
        .args([
            "--db",
            dir.path().join("dirtydash.sqlite3").to_str().unwrap(),
            "--config",
            dir.path().join("config.toml").to_str().unwrap(),
            "deploy",
            "hub",
            "unreachable",
            "--plan",
            "--manifest",
            manifest.to_str().unwrap(),
            "--artifact-dir",
            artifact_dir.to_str().unwrap(),
            "--public-key",
            public_key.to_str().unwrap(),
            "--publisher-key-id",
            "replacement-key",
            "--publisher-fingerprint",
            &trusted_fingerprint,
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("trust anchor"));
}

fn write_release_fixture(
    root: &std::path::Path,
    key: &SigningKey,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let artifact_dir = root.join(format!("artifacts-{}", key.to_bytes()[0]));
    fs::create_dir_all(&artifact_dir).unwrap();
    let bytes = b"signed-cli-artifact";
    let descriptor = ArtifactDescriptor {
        platform: TargetPlatform {
            os: ArtifactOs::Linux,
            arch: ArtifactArch::X86_64,
        },
        file: "dirtydash-linux-x86_64".to_string(),
        sha256: hex::encode(Sha256::digest(bytes)),
        size: bytes.len() as u64,
    };
    fs::write(artifact_dir.join(&descriptor.file), bytes).unwrap();
    let mut signed = SignedArtifactManifest {
        key_id: PublisherKey::fingerprint(&key.verifying_key().to_bytes()).unwrap(),
        manifest: ArtifactManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            release: "0.1.1-cli".to_string(),
            artifacts: vec![descriptor],
        },
        signature: String::new(),
    };
    signed.signature = hex::encode(key.sign(&signed.signing_bytes().unwrap()).to_bytes());
    let manifest = root.join(format!("manifest-{}.json", key.to_bytes()[0]));
    let public_key = root.join(format!("public-{}.key", key.to_bytes()[0]));
    fs::write(&manifest, serde_json::to_vec(&signed).unwrap()).unwrap();
    fs::write(&public_key, key.verifying_key().to_bytes()).unwrap();
    (manifest, artifact_dir, public_key)
}

#[test]
fn serve_starts_and_prints_dashboard_url() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("dirtydash.sqlite3");
    let config = dir.path().join("config.toml");
    let mut child = std_dirtydash_cmd()
        .args([
            "--db",
            db.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "serve",
            "--port",
            "0",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = sender.send(line);
    });

    let stdout = receiver
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_default();
    child.kill().ok();
    let _ = child.wait();
    assert!(db.exists());
    assert!(stdout.contains("dirtydash dashboard: http://127.0.0.1:"));
}

#[test]
fn loop_upgrade_refreshes_dirtyloops_runtime_artifacts() {
    let dir = tempdir().unwrap();
    let dirtyloops_root = dir.path().join("dirtyloops");
    write_fake_dirtyloops_root(&dirtyloops_root);
    let loop_dir = dir.path().join("docs/implementation/example-stream");
    fs::create_dir_all(loop_dir.join("prompts")).unwrap();
    fs::write(
        loop_dir.join("IMPLEMENT.md"),
        "# Example Stream Implementation Loop\n\nWorkflow: `single-thread-subagent`\n\nCanonical tracker: Beads epic `example-epic`\n",
    )
    .unwrap();
    fs::write(
        loop_dir.join("loop-state.md"),
        "# Loop State\n\nCanonical tracker: Beads epic `example-epic`\n\nWorkflow: `single-thread-subagent`\n",
    )
    .unwrap();
    fs::write(
        loop_dir.join("prompts/run-loop.md"),
        "# Run Loop: Example Stream\n\nWorkflow: `single-thread-subagent`\n\nCanonical tracker: Beads epic `example-epic`\n\n## Start Prompt\n\nKeep this custom prompt.\n",
    )
    .unwrap();

    dirtydash_cmd()
        .args([
            "--db",
            dir.path().join("dirtydash.sqlite3").to_str().unwrap(),
            "--config",
            dir.path().join("config.toml").to_str().unwrap(),
            "loop",
            "upgrade",
            loop_dir.to_str().unwrap(),
            "--dirtyloops-root",
            dirtyloops_root.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(contains("updated: prompts/run-loop.md"))
        .stdout(contains("created: schemas/swarm-report.schema.json"));

    let upgraded_prompt = fs::read_to_string(loop_dir.join("prompts/run-loop.md")).unwrap();
    assert!(upgraded_prompt.contains("new single-thread addendum"));
    assert!(upgraded_prompt.contains("Keep this custom prompt."));
    let upgraded_schema =
        fs::read_to_string(loop_dir.join("schemas/swarm-report.schema.json")).unwrap();
    assert!(upgraded_schema.contains("\"fake\": true"));
}

#[test]
fn loop_upgrade_check_reports_drift_without_writing() {
    let dir = tempdir().unwrap();
    let dirtyloops_root = dir.path().join("dirtyloops");
    write_fake_dirtyloops_root(&dirtyloops_root);
    let loop_dir = dir.path().join("docs/implementation/example-stream");
    fs::create_dir_all(loop_dir.join("prompts")).unwrap();
    fs::write(
        loop_dir.join("IMPLEMENT.md"),
        "# Example Stream Implementation Loop\n\nWorkflow: `single-thread-subagent`\n\nCanonical tracker: Beads epic `example-epic`\n",
    )
    .unwrap();
    fs::write(
        loop_dir.join("loop-state.md"),
        "# Loop State\n\nCanonical tracker: Beads epic `example-epic`\n\nWorkflow: `single-thread-subagent`\n",
    )
    .unwrap();
    fs::write(loop_dir.join("prompts/run-loop.md"), "old prompt\n").unwrap();

    dirtydash_cmd()
        .args([
            "--db",
            dir.path().join("dirtydash.sqlite3").to_str().unwrap(),
            "--config",
            dir.path().join("config.toml").to_str().unwrap(),
            "loop",
            "upgrade",
            loop_dir.to_str().unwrap(),
            "--dirtyloops-root",
            dirtyloops_root.to_str().unwrap(),
            "--check",
        ])
        .assert()
        .failure()
        .stdout(contains("would update: prompts/run-loop.md"))
        .stderr(contains("loop upgrade required"));

    let unchanged_prompt = fs::read_to_string(loop_dir.join("prompts/run-loop.md")).unwrap();
    assert_eq!(unchanged_prompt, "old prompt\n");
}

#[test]
fn loop_upgrade_preserves_orchestrator_prompt_values() {
    let dir = tempdir().unwrap();
    let dirtyloops_root = dir.path().join("dirtyloops");
    write_fake_dirtyloops_root(&dirtyloops_root);
    let loop_dir = dir.path().join("docs/implementation/callback-stream");
    fs::create_dir_all(loop_dir.join("prompts")).unwrap();
    fs::write(
        loop_dir.join("IMPLEMENT.md"),
        "# Callback Stream Implementation Loop\n\nWorkflow: `orchestrator-callback`\n\nCanonical tracker: Beads epic `callback-epic`\n",
    )
    .unwrap();
    fs::write(
        loop_dir.join("loop-state.md"),
        "# Loop State\n\nCanonical tracker: Beads epic `callback-epic`\n\nWorkflow: `orchestrator-callback`\n",
    )
    .unwrap();
    fs::write(
        loop_dir.join("prompts/run-loop.md"),
        "# Run Loop: Callback Stream\n\nWorkflow: `orchestrator-callback`\n\nCanonical tracker: Beads epic `callback-epic`\n\n## Start Prompt\n\nRun the callback stream.\n",
    )
    .unwrap();
    fs::write(
        loop_dir.join("prompts/implementation-thread.md"),
        "# Implementation Thread Prompt\n\nYou are the implementation thread for Beads issue `callback-epic.2`.\n\nCallback target:\n\n`THREAD-123`\n\n- Phase doc: `docs/implementation/callback-stream/02-build.md`\n- Implementation index: `docs/implementation/callback-stream/IMPLEMENT.md`\n- Turn doc: `docs/implementation/callback-stream/turn-docs/02-build.md`\n- Branch policy: create `callback-stream/02-build`\n",
    )
    .unwrap();
    fs::write(
        loop_dir.join("prompts/review-thread.md"),
        "# Review Thread Prompt\n\nYou are the review thread for Beads issue `callback-epic.2`.\n\nCallback target:\n\n`THREAD-123`\n\n- Phase doc: `docs/implementation/callback-stream/02-build.md`\n- Turn doc: `docs/implementation/callback-stream/turn-docs/02-build.md`\n- PR: `#42`\n- Branch/commit: `callback-stream/02-build`\n- Required gates: `cargo test`\n",
    )
    .unwrap();

    dirtydash_cmd()
        .args([
            "--db",
            dir.path().join("dirtydash.sqlite3").to_str().unwrap(),
            "--config",
            dir.path().join("config.toml").to_str().unwrap(),
            "loop",
            "upgrade",
            loop_dir.to_str().unwrap(),
            "--dirtyloops-root",
            dirtyloops_root.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(contains("updated: prompts/implementation-thread.md"))
        .stdout(contains("prompts/review-thread.md"));

    let implementation_prompt =
        fs::read_to_string(loop_dir.join("prompts/implementation-thread.md")).unwrap();
    assert!(implementation_prompt.contains("callback-epic.2"));
    assert!(implementation_prompt.contains("THREAD-123"));
    assert!(implementation_prompt.contains("docs/implementation/callback-stream/02-build.md"));
    assert!(implementation_prompt.contains("create `callback-stream/02-build`"));

    let review_prompt = fs::read_to_string(loop_dir.join("prompts/review-thread.md")).unwrap();
    assert!(review_prompt.contains("callback-epic.2"));
    assert!(review_prompt.contains("THREAD-123"));
    assert!(review_prompt.contains("#42"));
    assert!(review_prompt.contains("cargo test"));
}

fn write_fake_dirtyloops_root(root: &std::path::Path) {
    fs::create_dir_all(root.join("templates/common")).unwrap();
    fs::create_dir_all(root.join("templates/workflows/single-thread-subagent")).unwrap();
    fs::create_dir_all(root.join("templates/workflows/orchestrator-callback")).unwrap();
    fs::create_dir_all(root.join("schemas")).unwrap();
    fs::write(root.join("SKILL.md"), "# dirtyloops\n").unwrap();
    fs::write(
        root.join("templates/common/run-loop.md.template"),
        "# Run Loop: {{STREAM_NAME}}\n\nWorkflow: `{{WORKFLOW}}`\n\nCanonical tracker: Beads epic `{{EPIC_ID}}`\n\n## Workflow Addendum\n\n{{WORKFLOW_ADDENDUM}}\n\n## Start Prompt\n\n{{CUSTOM_RUN_PROMPT}}\n",
    )
    .unwrap();
    fs::write(
        root.join("templates/workflows/single-thread-subagent/run-loop-addendum.md.template"),
        "new single-thread addendum\n",
    )
    .unwrap();
    fs::write(
        root.join("templates/workflows/orchestrator-callback/run-loop-addendum.md.template"),
        "new callback addendum\n",
    )
    .unwrap();
    fs::write(
        root.join(
            "templates/workflows/orchestrator-callback/implementation-thread-prompt.md.template",
        ),
        "# Implementation Thread Prompt\n\nYou are the implementation thread for Beads issue `{{PHASE_ISSUE_ID}}`.\n\nCallback target:\n\n`{{ORCHESTRATOR_THREAD_ID}}`\n\n- Phase doc: `{{PHASE_DOC}}`\n- Implementation index: `{{IMPLEMENT_MD}}`\n- Turn doc: `{{TURN_DOC}}`\n- Branch/worktree instructions: `{{BRANCH_WORKTREE_INSTRUCTIONS}}`\n",
    )
    .unwrap();
    fs::write(
        root.join("templates/workflows/orchestrator-callback/review-thread-prompt.md.template"),
        "# Review Thread Prompt\n\nYou are the review thread for Beads issue `{{PHASE_ISSUE_ID}}`.\n\nCallback target:\n\n`{{ORCHESTRATOR_THREAD_ID}}`\n\n- Phase doc: `{{PHASE_DOC}}`\n- Turn doc: `{{TURN_DOC}}`\n- PR: `{{PR_URL_OR_ID}}`\n- Branch/commit: `{{BRANCH_OR_COMMIT}}`\n- Required gates: `{{QUALITY_GATES}}`\n",
    )
    .unwrap();
    fs::write(
        root.join("schemas/swarm-report.schema.json"),
        "{\"fake\": true}\n",
    )
    .unwrap();
}
