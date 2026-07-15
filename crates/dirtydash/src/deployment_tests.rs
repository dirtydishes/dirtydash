use super::*;
use ed25519_dalek::{Signer, SigningKey};
use std::collections::VecDeque;
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
    results: VecDeque<Result<RemoteResult>>,
}

impl RemoteExecutor for FakeExecutor {
    fn detect(&mut self) -> Result<RemoteFacts> {
        self.facts.clone().context("missing fake facts")
    }

    fn run(&mut self, action: RemoteAction) -> Result<RemoteResult> {
        let is_tailscale = matches!(action, RemoteAction::ConfigureTailscale { .. });
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
        results: VecDeque::from([
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
        .any(|action| matches!(action, RemoteAction::Cleanup { .. })));
    let _ = dir;
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
