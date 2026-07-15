use super::*;
use ed25519_dalek::{Signer, SigningKey};
use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

fn signed_fixture() -> (SignedArtifactManifest, Vec<u8>, [u8; 32]) {
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let bytes = b"dirtydash-linux-artifact".to_vec();
    let manifest = ArtifactManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        release: "0.1.2-test".to_string(),
        artifacts: vec![ArtifactDescriptor {
            platform: TargetPlatform {
                os: ArtifactOs::Linux,
                arch: ArtifactArch::X86_64,
            },
            file: "dirtydash-linux-x86_64".to_string(),
            sha256: hex::encode(Sha256::digest(&bytes)),
            size: bytes.len() as u64,
        }],
    };
    let mut unsigned = SignedArtifactManifest {
        key_id: PublisherKey::fingerprint(&key.verifying_key().to_bytes()).unwrap(),
        manifest,
        signature: String::new(),
    };
    let payload = unsigned.signing_bytes().unwrap();
    unsigned.signature = hex::encode(key.sign(&payload).to_bytes());
    (unsigned, bytes, key.verifying_key().to_bytes())
}

#[test]
fn platform_aliases_select_linux_and_macos_arm64_deterministically() {
    assert_eq!(
        TargetPlatform::from_uname("Linux", "x86_64").unwrap(),
        TargetPlatform {
            os: ArtifactOs::Linux,
            arch: ArtifactArch::X86_64
        }
    );
    assert_eq!(
        TargetPlatform::from_uname("Darwin", "arm64").unwrap(),
        TargetPlatform {
            os: ArtifactOs::Macos,
            arch: ArtifactArch::Arm64
        }
    );
    assert!(TargetPlatform::from_uname("FreeBSD", "amd64").is_err());
}

#[test]
fn publisher_anchor_rejects_replaced_manifest_key_and_publisher_assertions() {
    let (signed, _bytes, public_key) = signed_fixture();
    let fingerprint = PublisherKey::fingerprint(&public_key).unwrap();
    let publisher =
        PublisherKey::new(signed.key_id.clone(), fingerprint.clone(), &public_key).unwrap();
    assert!(signed.verify_with_publisher(&publisher).is_ok());

    let replacement_key = SigningKey::from_bytes(&[9_u8; 32]);
    let mut replacement = signed.clone();
    replacement.key_id =
        PublisherKey::fingerprint(&replacement_key.verifying_key().to_bytes()).unwrap();
    replacement.signature = hex::encode(
        replacement_key
            .sign(&replacement.signing_bytes().unwrap())
            .to_bytes(),
    );
    assert!(replacement.verify_with_publisher(&publisher).is_err());
    assert!(PublisherKey::new(
        "replacement",
        "sha256:deadbeef",
        &replacement_key.verifying_key().to_bytes()
    )
    .is_err());
    assert!(PublisherKey::new(
        signed.key_id.clone(),
        fingerprint,
        &replacement_key.verifying_key().to_bytes()
    )
    .is_err());
}

#[test]
fn all_linux_macos_x86_and_arm_targets_select_without_ambiguity() {
    let bytes = b"same-fixture".to_vec();
    let digest = hex::encode(Sha256::digest(&bytes));
    let platforms = [
        TargetPlatform {
            os: ArtifactOs::Linux,
            arch: ArtifactArch::X86_64,
        },
        TargetPlatform {
            os: ArtifactOs::Linux,
            arch: ArtifactArch::Arm64,
        },
        TargetPlatform {
            os: ArtifactOs::Macos,
            arch: ArtifactArch::X86_64,
        },
        TargetPlatform {
            os: ArtifactOs::Macos,
            arch: ArtifactArch::Arm64,
        },
    ];
    let verified = VerifiedArtifactManifest {
        key_id: "fixture".to_string(),
        publisher_fingerprint: "fixture-fingerprint".to_string(),
        manifest: ArtifactManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            release: "0.1.2".to_string(),
            artifacts: platforms
                .iter()
                .enumerate()
                .map(|(index, platform)| ArtifactDescriptor {
                    platform: *platform,
                    file: format!("dirtydash-{index}"),
                    sha256: digest.clone(),
                    size: bytes.len() as u64,
                })
                .collect(),
        },
        manifest_sha256: "fixture".to_string(),
    };
    for platform in platforms {
        assert!(verified.verify_artifact(platform, bytes.clone()).is_ok());
    }
}

#[test]
fn signature_and_checksum_are_both_required() {
    let (signed, bytes, public_key) = signed_fixture();
    let verified = signed.verify(&public_key).unwrap();
    let artifact = verified
        .verify_artifact(
            TargetPlatform {
                os: ArtifactOs::Linux,
                arch: ArtifactArch::X86_64,
            },
            bytes.clone(),
        )
        .unwrap();
    assert_eq!(artifact.bytes, bytes);
    let mut changed = bytes;
    changed.push(0);
    assert!(verified
        .verify_artifact(artifact.descriptor.platform, changed)
        .is_err());
    let mut unsigned = signed;
    unsigned.signature = "00".repeat(64);
    assert!(unsigned.verify(&public_key).is_err());
}

#[derive(Default)]
struct FakeExecutor {
    facts: Option<RemoteFacts>,
    actions: Vec<RemoteAction>,
    uploads: Vec<(String, Vec<u8>, u32)>,
    rollback_commands: Vec<String>,
    results: VecDeque<Result<RemoteResult>>,
}

impl RemoteExecutor for FakeExecutor {
    fn detect(&mut self) -> Result<RemoteFacts> {
        self.facts.clone().context("missing fake facts")
    }

    fn run(&mut self, action: RemoteAction) -> Result<RemoteResult> {
        let is_tailscale = matches!(&action, RemoteAction::ConfigureTailscale { .. });
        if matches!(&action, RemoteAction::Rollback { .. }) {
            self.rollback_commands.push(action_command(&action)?);
        }
        self.actions.push(action);
        if is_tailscale {
            return Ok(RemoteResult::consent_required("consent required"));
        }
        self.results
            .pop_front()
            .unwrap_or_else(|| Ok(RemoteResult::success("ok")))
    }

    fn upload(&mut self, destination: &str, bytes: &[u8], mode: u32) -> Result<RemoteResult> {
        self.uploads
            .push((destination.to_string(), bytes.to_vec(), mode));
        Ok(RemoteResult::success("uploaded"))
    }
}

fn facts() -> RemoteFacts {
    RemoteFacts {
        platform: TargetPlatform {
            os: ArtifactOs::Linux,
            arch: ArtifactArch::X86_64,
        },
        user: "delta".to_string(),
        uid: 1000,
        home: "/home/delta".to_string(),
        current_release: Some("/home/delta/.local/share/dirtydash/releases/old".to_string()),
    }
}

#[test]
fn plan_is_inspectable_and_contains_no_secret_fields() {
    let plan = DeploymentPlan::skeleton("di", "0.1.2", ListenerPlan::default(), true).unwrap();
    let json = plan.to_json().unwrap();
    assert!(json.contains("verify-signed-artifact"));
    assert!(json.contains("optional-database-seed"));
    assert!(!json.contains("password"));
    assert!(!json.contains("token"));
    assert!(!json.contains("PASSWORD_SENTINEL"));
}

#[test]
fn runner_rolls_back_and_cleans_up_after_restart_failure() {
    let (signed, bytes, public_key) = signed_fixture();
    let artifact = signed
        .verify(&public_key)
        .unwrap()
        .verify_artifact(facts().platform, bytes)
        .unwrap();
    let dir = tempdir().unwrap();
    let fake = FakeExecutor {
        facts: Some(facts()),
        // Fail after activation/restart/health mutation, not during the
        // snapshot itself, so the real rollback action must run.
        results: VecDeque::from([
            Ok(RemoteResult::success("ok")),
            Ok(RemoteResult::success("ok")),
            Ok(RemoteResult::success("ok")),
            Ok(RemoteResult::success("ok")),
            Ok(RemoteResult::success("ok")),
            Ok(RemoteResult::success("ok")),
            Ok(RemoteResult::success("ok")),
            Ok(RemoteResult::success("ok")),
            Err(anyhow::anyhow!("hostile stderr SECRET_SENTINEL")),
        ]),
        ..FakeExecutor::default()
    };
    let store = DeploymentStateStore::new(dir.path().join("deployment.json"));
    let mut runner = DeploymentRunner::new(fake).with_state_store(store);
    let mut request = DeploymentRequest::new("alias", "0.1.2-test", ListenerPlan::default());
    let plan = runner.probe(&request, Some(&artifact)).unwrap();
    request.approved_plan_hash = Some(plan.plan_hash);
    let error = runner.apply(&request, &artifact).unwrap_err().to_string();
    assert!(!error.contains("SECRET_SENTINEL"));
    let executor = runner.executor();
    assert!(executor
        .actions
        .iter()
        .any(|action| matches!(action, RemoteAction::Rollback { .. })));
    let rollback_command = executor.rollback_commands.first().unwrap();
    assert!(rollback_command.contains("/healthz"));
    assert!(rollback_command.contains("collector diagnostics --json"));
    assert!(rollback_command.contains("restart dirtydash-hub.service"));
    assert!(rollback_command.contains("restart dirtydash-collector.service"));
    assert!(executor
        .actions
        .iter()
        .any(|action| matches!(action, RemoteAction::Cleanup { .. })));
    let _ = dir;
}

#[test]
fn rollback_failure_is_an_explicit_manual_recovery_blocker() {
    let (signed, bytes, public_key) = signed_fixture();
    let artifact = signed
        .verify(&public_key)
        .unwrap()
        .verify_artifact(facts().platform, bytes)
        .unwrap();
    let dir = tempdir().unwrap();
    let store = DeploymentStateStore::new(dir.path().join("deployment.json"));
    let mut results = VecDeque::new();
    for _ in 0..8 {
        results.push_back(Ok(RemoteResult::success("ok")));
    }
    results.push_back(Err(anyhow::anyhow!("health failed")));
    results.push_back(Err(anyhow::anyhow!("rollback failed")));
    let fake = FakeExecutor {
        facts: Some(facts()),
        results,
        ..FakeExecutor::default()
    };
    let mut runner = DeploymentRunner::new(fake).with_state_store(store.clone());
    let request = DeploymentRequest::new("alias", "0.1.2-test", ListenerPlan::default());
    let plan = runner.probe(&request, Some(&artifact)).unwrap();
    let mut approved = request;
    approved.approved_plan_hash = Some(plan.plan_hash);
    let error = runner.apply(&approved, &artifact).unwrap_err().to_string();
    assert!(error.contains("manual recovery required"));
    assert_eq!(
        store.load().unwrap().unwrap().status,
        "manual-recovery-required"
    );
}

#[test]
fn consent_is_a_durable_resumable_receipt_not_a_secret_failure() {
    let (signed, bytes, public_key) = signed_fixture();
    let artifact = signed
        .verify(&public_key)
        .unwrap()
        .verify_artifact(facts().platform, bytes)
        .unwrap();
    let dir = tempdir().unwrap();
    let checkpoint = DeploymentStateStore::new(dir.path().join("deployment.json"));
    let fake = FakeExecutor {
        facts: Some(facts()),
        results: VecDeque::from([Ok(RemoteResult::success("ok"))]),
        ..FakeExecutor::default()
    };
    let mut runner = DeploymentRunner::new(fake).with_state_store(checkpoint.clone());
    let request = DeploymentRequest::new("manual", "0.1.2-test", ListenerPlan::default());
    let plan = runner.probe(&request, Some(&artifact)).unwrap();
    let mut approved_request = request;
    approved_request.approved_plan_hash = Some(plan.plan_hash);
    let receipt = runner.apply(&approved_request, &artifact).unwrap();
    assert_eq!(receipt.status, "consent-required");
    assert_eq!(
        checkpoint.load().unwrap().unwrap().status,
        "consent-required"
    );
}

#[test]
fn concrete_plan_persists_review_evidence_and_rollback_intent() {
    let (signed, bytes, public_key) = signed_fixture();
    let artifact = signed
        .verify(&public_key)
        .unwrap()
        .verify_artifact(facts().platform, bytes.clone())
        .unwrap();
    let canonical = CanonicalSshTarget::from_ssh_config(
            "delta@example:2222",
            "hostname remote.example\nport 2222\nuser delta\nhostkeyalias remote-managed\nproxyjump bastion\nproxycommand none\n",
        )
        .unwrap();
    let plan = DeploymentPlan::for_facts_with_details(
        "delta@example:2222",
        "0.1.2-test",
        &facts(),
        ListenerPlan::default(),
        DeploymentPlanDetails {
            artifact: Some(artifact.evidence()),
            database_seed: true,
            seed_bytes: Some(b"seed".to_vec()),
            ssh_target: Some(canonical),
        },
    )
    .unwrap();
    let json = plan.to_json().unwrap();
    assert!(json.contains(&artifact.descriptor().sha256));
    assert!(json.contains(&artifact.manifest().key_id));
    assert!(json.contains("remote-managed"));
    assert!(json.contains("target_facts"));
    assert!(json.contains("private-tailscale"));
    assert!(json.contains("sqlite-seed"));
    assert!(json.contains("rollback_snapshot_dir"));
    let dir = tempdir().unwrap();
    let store = DeploymentStateStore::new(dir.path().join("checkpoint.json"));
    store.save_plan(&plan).unwrap();
    assert_eq!(store.load_plan().unwrap().unwrap(), plan);
}

#[test]
fn apply_requires_an_explicit_approved_persisted_hash() {
    let (signed, bytes, public_key) = signed_fixture();
    let artifact = signed
        .verify(&public_key)
        .unwrap()
        .verify_artifact(facts().platform, bytes)
        .unwrap();
    let dir = tempdir().unwrap();
    let store = DeploymentStateStore::new(dir.path().join("checkpoint.json"));
    let fake = FakeExecutor {
        facts: Some(facts()),
        ..FakeExecutor::default()
    };
    let mut runner = DeploymentRunner::new(fake).with_state_store(store);
    let request = DeploymentRequest::new("alias", "0.1.2-test", ListenerPlan::default());
    let plan = runner.probe(&request, Some(&artifact)).unwrap();
    assert!(runner.apply(&request, &artifact).is_err());
    assert_eq!(plan.target, "alias");
    assert!(runner.executor().actions.is_empty());
}

#[test]
fn rollback_refuses_database_deletion_without_a_backup() {
    let result = action_command(&RemoteAction::Rollback {
        current: "/home/delta/current".to_string(),
        previous: None,
        database_path: Some("/home/delta/db.sqlite3".to_string()),
        database_backup: None,
        database_wal_backup: None,
        database_shm_backup: None,
        platform: ServicePlatform::Systemd,
        listener: None,
        snapshot_dir: None,
    });
    assert!(result.is_err());
}

#[test]
fn health_and_service_restart_verify_hub_and_collector_independently() {
    let restart = action_command(&RemoteAction::RestartServices {
        platform: ServicePlatform::Systemd,
    })
    .unwrap();
    let health = action_command(&RemoteAction::HealthCheck {
        port: 4599,
        platform: ServicePlatform::Systemd,
    })
    .unwrap();
    assert!(restart.contains("is-active --quiet dirtydash-hub.service"));
    assert!(restart.contains("is-active --quiet dirtydash-collector.service"));
    assert!(health.contains("/healthz"));
    assert!(health.contains("dirtydash-collector.service"));
}

#[test]
fn remote_probe_rejects_root_and_parses_platform() {
    let facts =
        RemoteFacts::parse_probe("os=Darwin\narch=arm64\nuser=alice\nuid=501\nhome=/Users/alice\n")
            .unwrap();
    assert_eq!(facts.platform.service_platform(), ServicePlatform::Launchd);
    assert!(
        RemoteFacts::parse_probe("os=Linux\narch=x86_64\nuser=root\nuid=0\nhome=/root\n").is_err()
    );
}

#[test]
fn ssh_actions_use_fixed_options_and_no_secret_arguments() {
    let command = action_command(&RemoteAction::HealthCheck {
        port: 4599,
        platform: ServicePlatform::Systemd,
    })
    .unwrap();
    assert!(command.contains("127.0.0.1:4599"));
    assert!(!command.contains("password"));
    assert!(!command.contains("secret"));
}

#[test]
fn live_stdin_writes_classified_passwords_after_fixed_prompts() {
    let secrets =
        SshLiveSecrets::new(Some(b"PASSWORD_SENTINEL"), None, Some(b"SUDO_SENTINEL")).unwrap();
    let script = r#"
        printf 'user@host password: ' >&2
        IFS= read -r password
        printf 'DIRTYDASH_SUDO_PROMPT' >&2
        IFS= read -r sudo
        [ "$password" = PASSWORD_SENTINEL ] || exit 20
        [ "$sudo" = SUDO_SENTINEL ] || exit 21
        dd bs=1 count=19 2>/dev/null
    "#;
    let args = vec!["-c".to_string(), script.to_string()];
    let result = run_live_process(
        Path::new("/bin/sh"),
        &args,
        &secrets,
        true,
        true,
        Some(b"classified-artifact"),
    )
    .unwrap();
    assert!(result.stdout.contains("classified-artifact"));
    assert!(!result.stdout.contains("SENTINEL"));
    assert!(!result.stderr.contains("SENTINEL"));
    assert!(!format!("{secrets:?}").contains("SENTINEL"));
}

#[test]
fn live_stdin_failure_redacts_remote_secret_echoes() {
    let secrets = SshLiveSecrets::new(Some(b"PASSWORD_SENTINEL"), None, None).unwrap();
    let script = r#"
        printf 'password: ' >&2
        IFS= read -r password
        printf 'password=%s hostile failure\n' "$password" >&2
        exit 1
    "#;
    let args = vec!["-c".to_string(), script.to_string()];
    let error = run_live_process(Path::new("/bin/sh"), &args, &secrets, true, false, None)
        .unwrap_err()
        .to_string();
    assert!(!error.contains("PASSWORD_SENTINEL"));
    assert!(error.contains("[REDACTED]"));
}

#[test]
fn sqlite_header_validation_and_remote_fallback_are_byte_level() {
    let mut valid = b"SQLite format 3\0".to_vec();
    valid.extend_from_slice(b"payload");
    assert!(validate_sqlite_header(&valid).is_ok());
    assert!(validate_sqlite_header(b"not sqlite").is_err());

    let command = action_command(&RemoteAction::InstallDatabaseSeed {
        seed_path: "/tmp/seed".to_string(),
        database_path: "/tmp/db.sqlite3".to_string(),
        backup_path: "/tmp/db.sqlite3.previous".to_string(),
        wal_backup_path: "/tmp/db.sqlite3.previous-wal".to_string(),
        shm_backup_path: "/tmp/db.sqlite3.previous-shm".to_string(),
    })
    .unwrap();
    assert!(command.contains("python3 -"));
    assert!(command.contains("od -An -v -t x1"));
    assert!(command.contains(SQLITE_HEADER_HEX));
    assert!(!command.contains("\\000"));
    assert!(!command.contains("$(dd if='\\''/tmp/seed\\'' bs=1 count=16"));
    assert!(command.contains("db.sqlite3"));
    assert!(command.contains("previous-wal"));
}

#[test]
fn no_sqlite3_install_accepts_valid_wal_backup_and_rejects_malformed_seed() {
    let dir = tempdir().unwrap();
    let seed = dir.path().join("seed.sqlite3");
    let database = dir.path().join("database.sqlite3");
    let backup = dir.path().join("database.sqlite3.previous");
    let wal = dir.path().join("database.sqlite3.previous-wal");
    let shm = dir.path().join("database.sqlite3.previous-shm");
    let action = RemoteAction::InstallDatabaseSeed {
        seed_path: seed.display().to_string(),
        database_path: database.display().to_string(),
        backup_path: backup.display().to_string(),
        wal_backup_path: wal.display().to_string(),
        shm_backup_path: shm.display().to_string(),
    };
    let command = action_command(&action).unwrap();
    let mut old = b"SQLite format 3\0old-database".to_vec();
    old.extend(std::iter::repeat_n(0_u8, 64));
    fs::write(&database, &old).unwrap();
    fs::write(database.with_extension("sqlite3-wal"), b"wal-bytes").unwrap();
    fs::write(database.with_extension("sqlite3-shm"), b"shm-bytes").unwrap();
    let mut new_seed = b"SQLite format 3\0new-database".to_vec();
    new_seed.extend(std::iter::repeat_n(1_u8, 64));
    fs::write(&seed, &new_seed).unwrap();
    let status = Command::new("sh")
        .args(["-c", &command])
        .env("PATH", "/usr/bin:/bin")
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(fs::read(&database).unwrap(), new_seed);
    assert_eq!(fs::read(&backup).unwrap(), old);
    assert_eq!(fs::read(&wal).unwrap(), b"wal-bytes");
    assert_eq!(fs::read(&shm).unwrap(), b"shm-bytes");

    let malformed_seed = dir.path().join("malformed.sqlite3");
    fs::write(&malformed_seed, b"not sqlite").unwrap();
    let malformed = action_command(&RemoteAction::InstallDatabaseSeed {
        seed_path: malformed_seed.display().to_string(),
        database_path: database.display().to_string(),
        backup_path: dir.path().join("malformed.previous").display().to_string(),
        wal_backup_path: dir
            .path()
            .join("malformed.previous-wal")
            .display()
            .to_string(),
        shm_backup_path: dir
            .path()
            .join("malformed.previous-shm")
            .display()
            .to_string(),
    })
    .unwrap();
    let malformed_status = Command::new("sh")
        .args(["-c", &malformed])
        .env("PATH", "/usr/bin:/bin")
        .status()
        .unwrap();
    assert!(!malformed_status.success());
    assert_eq!(fs::read(&database).unwrap(), new_seed);
    assert!(malformed_seed.exists());
}

#[test]
fn rollback_restores_snapshot_listener_and_checks_old_hub_and_collector() {
    let paths = DeploymentPaths::for_facts(&facts(), "0.1.2-test").unwrap();
    let snapshot = action_command(&RemoteAction::SnapshotRollbackState {
        paths,
        platform: ServicePlatform::Systemd,
        listener: ListenerPlan::default(),
    })
    .unwrap();
    assert!(snapshot.contains("tailscale serve status"));
    assert!(snapshot.contains("previous-current"));
    assert!(snapshot.contains("is-enabled"));
    assert!(snapshot.contains("is-active"));
    assert!(snapshot.contains("listener-state"));
    assert!(Command::new("sh")
        .args(["-n", "-c", &snapshot])
        .status()
        .unwrap()
        .success());
    let command = action_command(&RemoteAction::Rollback {
        current: "/home/delta/.local/share/dirtydash/current".to_string(),
        previous: Some("/home/delta/.local/share/dirtydash/releases/old".to_string()),
        database_path: Some(
            "/home/delta/.local/state/dirtydash/data/dirtydash.sqlite3".to_string(),
        ),
        database_backup: Some(
            "/home/delta/.local/state/dirtydash/data/dirtydash.sqlite3.previous".to_string(),
        ),
        database_wal_backup: Some(
            "/home/delta/.local/state/dirtydash/data/dirtydash.sqlite3.previous-wal".to_string(),
        ),
        database_shm_backup: Some(
            "/home/delta/.local/state/dirtydash/data/dirtydash.sqlite3.previous-shm".to_string(),
        ),
        platform: ServicePlatform::Systemd,
        listener: Some(ListenerPlan::default()),
        snapshot_dir: Some("/home/delta/.local/state/dirtydash/deployment-rollback".to_string()),
    })
    .unwrap();
    assert!(command.contains("listener-state"));
    assert!(command.contains("listener-backend"));
    assert!(command.contains("tailscale serve reset"));
    assert!(command.contains("systemctl --user restart dirtydash-hub.service"));
    assert!(command.contains("systemctl --user restart dirtydash-collector.service"));
    assert!(command.contains("/healthz"));
    assert!(command.contains("collector diagnostics --json"));
    assert!(Command::new("sh")
        .args(["-n", "-c", &command])
        .status()
        .unwrap()
        .success());
}
