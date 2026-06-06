use std::fs;
use std::io::BufRead;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use predicates::str::contains;
use tempfile::tempdir;

fn dirtydash_cmd() -> assert_cmd::Command {
    let mut command = assert_cmd::Command::cargo_bin("dirtydash").unwrap();
    command
        .env("CLAUDE_CONFIG_DIR", "/tmp/dirtydash-test-no-claude")
        .env("CODEX_HOME", "/tmp/dirtydash-test-no-codex")
        .env("OPENCODE_DATA_DIR", "/tmp/dirtydash-test-no-opencode")
        .env("PI_AGENT_DIR", "/tmp/dirtydash-test-no-pi");
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
        .env("PI_AGENT_DIR", "/tmp/dirtydash-test-no-pi");
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
