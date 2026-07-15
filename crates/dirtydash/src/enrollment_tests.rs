use super::*;
use crate::config::{RemoteConfig, SourceRoot};
use crate::deployment::{
    ArtifactArch, ArtifactDescriptor, ArtifactManifest, ArtifactOs, PublisherTrustPolicy,
    MANIFEST_SCHEMA_VERSION,
};
use ed25519_dalek::{Signer, SigningKey};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::Path;
use tempfile::tempdir;

#[cfg(unix)]
fn executable_script(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
fn enrollment_fake_ssh(path: &Path, fail_commands: bool, home: &Path) {
    let failure = if fail_commands {
        r#"case "$last" in
    *"deployment-rollback"*) exec /bin/sh -c "$last" ;;
    *"mkdir -p"*) printf 'password=PASSWORD_SENTINEL hostile mutation failure\n' >&2; exit 43 ;;
esac
"#
    } else {
        ""
    };
    executable_script(
        path,
        &format!(
            r#"#!/bin/sh
set -eu
socket=''
master=0
operation=''
last=''
while [ "$#" -gt 0 ]; do
    arg=$1
    shift
    case "$arg" in
        -S) socket=$1; shift ;;
        -O) operation=$1; shift ;;
        -M) master=1 ;;
        -o|-p|-i) shift ;;
        *) last=$arg ;;
    esac
done
if [ "$operation" = exit ]; then rm -f "$socket"; exit 0; fi
if [ "$master" = 1 ]; then
    printf 'user@host password: ' >&2
    IFS= read -r password
    [ "$password" = PASSWORD_SENTINEL ] || {{ printf 'password=%s hostile\n' "$password" >&2; exit 21; }}
    : > "$socket"
    exit 0
fi
case "$last" in
    *"printf 'os="*) printf 'os=Linux\narch=x86_64\nuser=deploy\nuid=1000\nhome={home}\n' ; exit 0 ;;
    *"printf dirtydash-authenticated"*) printf 'dirtydash-authenticated\n' ; exit 0 ;;
esac
{failure}
exec /bin/sh -c "$last"
"#,
            home = home.display(),
            failure = failure,
        ),
    );
}

#[cfg(unix)]
fn enrollment_fake_keyscan(path: &Path) {
    executable_script(path, "#!/bin/sh\nprintf 'example ssh-ed25519 AQID\\n'\n");
}

#[derive(Default)]
struct ScriptedBackend {
    observation: Option<HostKeyObservation>,
    auth_error: Option<String>,
    facts: Option<RemoteFacts>,
    plan: Option<DeploymentPlan>,
    execute_error: Option<String>,
    cleanup_error: Option<String>,
    seen_secrets: Vec<String>,
    execute_calls: usize,
    cleanup_calls: usize,
}

impl EnrollmentBackend for ScriptedBackend {
    fn observe_host_key(
        &mut self,
        _connection: &ConnectionSpec,
        _auth_method: &AuthMethod,
        _known_hosts: &KnownHostStore,
        secrets: &EnrollmentSecrets,
    ) -> Result<HostKeyObservation> {
        self.capture(secrets);
        self.observation
            .clone()
            .context("missing scripted host key")
    }

    fn authenticate(
        &mut self,
        _connection: &ConnectionSpec,
        _auth_method: &AuthMethod,
        _known_hosts: &KnownHostStore,
        secrets: &EnrollmentSecrets,
    ) -> Result<()> {
        self.capture(secrets);
        if let Some(error) = &self.auth_error {
            bail!(
                "hostile stderr password={} {error}",
                secrets
                    .password
                    .as_ref()
                    .map(|value| value.expose())
                    .unwrap_or("")
            );
        }
        Ok(())
    }

    fn probe_and_plan(
        &mut self,
        _connection: &ConnectionSpec,
        _auth_method: &AuthMethod,
        _known_hosts: &KnownHostStore,
        secrets: &EnrollmentSecrets,
    ) -> Result<(RemoteFacts, DeploymentPlan)> {
        self.capture(secrets);
        Ok((
            self.facts.clone().context("missing facts")?,
            self.plan.clone().context("missing plan")?,
        ))
    }

    fn execute(&mut self, request: EnrollmentExecution<'_>) -> Result<EnrollmentReceipt> {
        let EnrollmentExecution {
            plan_hash,
            artifact,
            database_seed,
            secrets,
            ..
        } = request;
        self.capture(secrets);
        self.execute_calls += 1;
        if let Some(error) = &self.execute_error {
            bail!(
                "hostile installer stderr password={} {error}",
                secrets
                    .password
                    .as_ref()
                    .map(|value| value.expose())
                    .unwrap_or("")
            );
        }
        Ok(EnrollmentReceipt {
            plan_hash: plan_hash.to_string(),
            release: artifact.manifest().manifest().release.clone(),
            artifact_sha256: artifact.descriptor().sha256.clone(),
            machine_id: None,
            collector_credential_id: None,
            collector_hub_url: None,
            artifact_size: artifact.descriptor().size,
            publisher_key_id: artifact.manifest().key_id().to_string(),
            hub_health_verified: true,
            collector_health_verified: true,
            backfill_queued: database_seed.is_some(),
            status: "complete".to_string(),
        })
    }

    fn cleanup(&mut self, _draft: &EnrollmentDraft, secrets: &EnrollmentSecrets) -> Result<()> {
        self.capture(secrets);
        self.cleanup_calls += 1;
        if let Some(error) = &self.cleanup_error {
            bail!(
                "cleanup stderr password={} {error}",
                secrets
                    .password
                    .as_ref()
                    .map(|value| value.expose())
                    .unwrap_or("")
            );
        }
        Ok(())
    }
}

impl ScriptedBackend {
    fn capture(&mut self, secrets: &EnrollmentSecrets) {
        if let Some(password) = &secrets.password {
            self.seen_secrets.push(password.expose().to_string());
        }
        if let Some(sudo) = &secrets.sudo_password {
            self.seen_secrets.push(sudo.expose().to_string());
        }
    }
}

fn fixture_policy() -> PublisherTrustPolicy {
    let key = SigningKey::from_bytes(&[8_u8; 32]);
    PublisherTrustPolicy::new(
        PublisherTrustPolicy::fingerprint(&key.verifying_key().to_bytes()).unwrap(),
        PublisherTrustPolicy::fingerprint(&key.verifying_key().to_bytes()).unwrap(),
        &key.verifying_key().to_bytes(),
    )
    .unwrap()
}

fn fixture_artifact() -> VerifiedArtifact {
    let key = SigningKey::from_bytes(&[8_u8; 32]);
    let bytes = b"fixture".to_vec();
    let manifest = ArtifactManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        release: "0.1.1".to_string(),
        artifacts: vec![ArtifactDescriptor {
            platform: TargetPlatform {
                os: ArtifactOs::Linux,
                arch: ArtifactArch::X86_64,
            },
            file: "dirtydash".to_string(),
            sha256: hex::encode(Sha256::digest(&bytes)),
            size: bytes.len() as u64,
        }],
    };
    let mut signed = crate::deployment::SignedArtifactManifest {
        key_id: PublisherTrustPolicy::fingerprint(&key.verifying_key().to_bytes()).unwrap(),
        manifest,
        signature: String::new(),
    };
    signed.signature = hex::encode(key.sign(&signed.signing_bytes().unwrap()).to_bytes());
    PublisherTrustPolicy::new(
        signed.key_id.clone(),
        PublisherTrustPolicy::fingerprint(&key.verifying_key().to_bytes()).unwrap(),
        &key.verifying_key().to_bytes(),
    )
    .unwrap()
    .verify(&signed)
    .unwrap()
    .verify_artifact(
        TargetPlatform {
            os: ArtifactOs::Linux,
            arch: ArtifactArch::X86_64,
        },
        bytes,
    )
    .unwrap()
}

fn workflow() -> (
    EnrollmentWorkflow<ScriptedBackend>,
    EnrollmentDraft,
    DeploymentPlan,
    tempfile::TempDir,
) {
    let dir = tempdir().unwrap();
    let connection = ConnectionSpec::alias("prod-alias").unwrap();
    let auth = AuthMethod::password();
    let facts = RemoteFacts {
        platform: TargetPlatform {
            os: ArtifactOs::Linux,
            arch: ArtifactArch::X86_64,
        },
        user: "delta".to_string(),
        uid: 1000,
        home: "/home/delta".to_string(),
        current_release: None,
    };
    let plan = DeploymentPlan::for_facts(
        "prod-alias",
        "0.1.1",
        &facts,
        ListenerPlan::default(),
        false,
    )
    .unwrap();
    let draft = EnrollmentDraft::new("machine-1", connection, auth).unwrap();
    let backend = ScriptedBackend {
        observation: Some(
            HostKeyObservation::new("sha256:known", "prod-alias ssh-ed25519 AAAA").unwrap(),
        ),
        facts: Some(facts.clone()),
        plan: Some(plan.clone()),
        ..ScriptedBackend::default()
    };
    let store = EnrollmentStore::new(dir.path().join("enrollments"));
    let known = KnownHostStore::new(dir.path().join("known_hosts"));
    let workflow = EnrollmentWorkflow::new(store, known, backend, fixture_policy());
    (workflow, draft, plan, dir)
}

#[test]
fn alias_and_manual_connections_are_fixed_and_never_shell_fragments() {
    assert_eq!(
        ConnectionSpec::alias("prod").unwrap().display_target(),
        "prod"
    );
    assert_eq!(
        ConnectionSpec::manual("alice", "host.example", 2222)
            .unwrap()
            .display_target(),
        "alice@host.example"
    );
    assert!(ConnectionSpec::alias("prod; touch pwned").is_err());
    let key = AuthMethod::key_path("/home/alice/.ssh/id_ed25519").unwrap();
    assert!(matches!(key, AuthMethod::KeyPath { .. }));
}

#[test]
fn unknown_key_requires_confirmation_then_matching_key_is_accepted() {
    let (mut workflow, draft, plan, _dir) = workflow();
    workflow.start(draft).unwrap();
    let secrets =
        EnrollmentSecrets::password("PASSWORD_SENTINEL").with_sudo_password("SUDO_SENTINEL");
    let first = workflow
        .trust_and_auth("machine-1", &AuthMethod::Password, &secrets, None)
        .unwrap();
    assert_eq!(first.status, HostKeyStatus::Unknown);
    assert!(!first.confirmed);
    let second = workflow
        .trust_and_auth(
            "machine-1",
            &AuthMethod::Password,
            &secrets,
            Some("sha256:known"),
        )
        .unwrap();
    assert!(second.confirmed);
    assert_eq!(
        workflow.store.load("machine-1").unwrap().state,
        EnrollmentState::HostTrustAuth
    );
    let planned = workflow
        .probe_and_plan("machine-1", &AuthMethod::Password, &secrets)
        .unwrap();
    assert_eq!(planned.plan_hash, plan.plan_hash);
}

#[test]
fn changed_key_is_blocked_without_overwriting_known_hosts() {
    let (mut workflow, draft, _plan, _dir) = workflow();
    workflow.start(draft).unwrap();
    let secrets = EnrollmentSecrets::none();
    workflow
        .trust_and_auth(
            "machine-1",
            &AuthMethod::Password,
            &secrets,
            Some("sha256:known"),
        )
        .unwrap();
    workflow.backend_mut().observation =
        Some(HostKeyObservation::new("sha256:changed", "prod-alias ssh-ed25519 BBBB").unwrap());
    assert!(workflow
        .trust_and_auth(
            "machine-1",
            &AuthMethod::Password,
            &secrets,
            Some("sha256:changed")
        )
        .is_err());
    assert_eq!(
        workflow.store.load("machine-1").unwrap().blocker,
        EnrollmentBlocker::ChangedHostKey
    );
    assert_eq!(
        workflow
            .known_hosts
            .status("prod-alias", "sha256:known")
            .unwrap(),
        HostKeyStatus::Matching
    );
}

#[test]
fn five_states_survive_restart_and_execute_records_receipt_backfill() {
    let (mut workflow, draft, _plan, dir) = workflow();
    workflow.start(draft).unwrap();
    let secrets = EnrollmentSecrets::password("PASSWORD_SENTINEL");
    workflow
        .trust_and_auth(
            "machine-1",
            &AuthMethod::Password,
            &secrets,
            Some("sha256:known"),
        )
        .unwrap();
    workflow
        .probe_and_plan_with_seed("machine-1", &AuthMethod::Password, &secrets, Some(b"seed"))
        .unwrap();
    let probed_plan = workflow.store.load("machine-1").unwrap().plan.unwrap();
    let reviewed_plan = workflow
        .review_with_artifact(
            "machine-1",
            &probed_plan,
            &fixture_artifact(),
            Some(b"seed"),
        )
        .unwrap();
    let store = workflow.store.clone();
    let known = workflow.known_hosts.clone();
    let backend = std::mem::take(&mut workflow.backend);
    let mut restarted = EnrollmentWorkflow::new(store, known, backend, fixture_policy());
    let receipt = restarted
        .execute(
            "machine-1",
            &reviewed_plan,
            &fixture_artifact(),
            Some(b"seed"),
            &ListenerPlan::default(),
            &secrets,
        )
        .unwrap();
    assert!(receipt.backfill_queued);
    assert_eq!(
        restarted.store.load("machine-1").unwrap().state,
        EnrollmentState::ExecuteVerifyReceipt
    );
    let raw = fs::read_dir(dir.path().join("enrollments"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let json = fs::read_to_string(raw).unwrap();
    assert!(!json.contains("PASSWORD_SENTINEL"));
    assert!(!json.contains("SUDO_SENTINEL"));
}

#[test]
fn failed_install_redacts_hostile_stderr_and_retry_cleanup_is_explicit() {
    let (mut workflow, draft, plan, _dir) = workflow();
    workflow.start(draft).unwrap();
    let secrets = EnrollmentSecrets::password("PASSWORD_SENTINEL");
    workflow
        .trust_and_auth(
            "machine-1",
            &AuthMethod::Password,
            &secrets,
            Some("sha256:known"),
        )
        .unwrap();
    workflow
        .probe_and_plan("machine-1", &AuthMethod::Password, &secrets)
        .unwrap();
    let reviewed_plan = workflow
        .review_with_artifact("machine-1", &plan, &fixture_artifact(), None)
        .unwrap();
    workflow.backend_mut().execute_error = Some("PASSWORD_SENTINEL".to_string());
    assert!(workflow
        .execute(
            "machine-1",
            &reviewed_plan,
            &fixture_artifact(),
            None,
            &ListenerPlan::default(),
            &secrets
        )
        .is_err());
    let draft = workflow.store.load("machine-1").unwrap();
    assert_eq!(draft.blocker, EnrollmentBlocker::CleanupRequired);
    assert!(!draft.last_error.unwrap().contains("PASSWORD_SENTINEL"));
    workflow.retry_cleanup("machine-1", &secrets).unwrap();
    assert!(workflow.store.load("machine-1").unwrap().cleanup_complete);
    workflow.backend_mut().execute_error = None;
    let receipt = workflow
        .execute(
            "machine-1",
            &reviewed_plan,
            &fixture_artifact(),
            None,
            &ListenerPlan::default(),
            &secrets,
        )
        .unwrap();
    assert_eq!(receipt.plan_hash, reviewed_plan.plan_hash);
    assert_eq!(workflow.store.load("machine-1").unwrap().attempts, 2);
}

#[test]
fn rollback_failure_is_persisted_as_manual_recovery_blocker() {
    let (mut workflow, draft, plan, _dir) = workflow();
    workflow.start(draft).unwrap();
    let secrets = EnrollmentSecrets::password("PASSWORD_SENTINEL");
    workflow
        .trust_and_auth(
            "machine-1",
            &AuthMethod::Password,
            &secrets,
            Some("sha256:known"),
        )
        .unwrap();
    workflow
        .probe_and_plan("machine-1", &AuthMethod::Password, &secrets)
        .unwrap();
    let reviewed_plan = workflow
        .review_with_artifact("machine-1", &plan, &fixture_artifact(), None)
        .unwrap();
    workflow.backend_mut().execute_error = Some("manual recovery required".to_string());
    assert!(workflow
        .execute(
            "machine-1",
            &reviewed_plan,
            &fixture_artifact(),
            None,
            &ListenerPlan::default(),
            &secrets,
        )
        .is_err());
    assert_eq!(
        workflow.store.load("machine-1").unwrap().blocker,
        EnrollmentBlocker::ManualRecoveryRequired
    );
}

#[test]
fn sudo_failure_stays_on_auth_step_and_redacts_password() {
    let (mut workflow, draft, _plan, _dir) = workflow();
    workflow.start(draft).unwrap();
    workflow.backend_mut().auth_error = Some("sudo failure".to_string());
    let secrets =
        EnrollmentSecrets::password("PASSWORD_SENTINEL").with_sudo_password("SUDO_SENTINEL");
    assert!(workflow
        .trust_and_auth(
            "machine-1",
            &AuthMethod::Password,
            &secrets,
            Some("sha256:known")
        )
        .is_err());
    let draft = workflow.store.load("machine-1").unwrap();
    assert_eq!(draft.blocker, EnrollmentBlocker::SudoFailed);
    assert!(!draft.last_error.unwrap().contains("PASSWORD_SENTINEL"));
    assert!(!workflow.backend().seen_secrets.is_empty());
}

#[test]
fn stale_plan_is_rejected() {
    let (mut workflow, draft, plan, _dir) = workflow();
    workflow.start(draft).unwrap();
    let secrets = EnrollmentSecrets::none();
    workflow
        .trust_and_auth(
            "machine-1",
            &AuthMethod::Password,
            &secrets,
            Some("sha256:known"),
        )
        .unwrap();
    workflow
        .probe_and_plan("machine-1", &AuthMethod::Password, &secrets)
        .unwrap();
    let mut changed = plan.clone();
    changed.release = "changed".to_string();
    changed.refresh_hash().unwrap();
    assert!(workflow.review("machine-1", &changed).is_err());
}

#[test]
fn legacy_conversion_never_enrolls_or_calls_ssh() {
    let remotes = vec![RemoteConfig {
        name: "old-box".to_string(),
        ssh_target: "alice@example.com".to_string(),
        source_roots: vec![SourceRoot {
            kind: "codex".to_string(),
            path: PathBuf::from("~/.codex"),
        }],
    }];
    let drafts = convert_legacy_remote_drafts(&remotes).unwrap();
    assert_eq!(drafts.len(), 1);
    assert!(!drafts[0].enrolled);
    assert!(drafts[0].conversion_note.contains("explicit"));
}

#[cfg(unix)]
#[test]
fn production_ssh_enrollment_auth_probe_failure_and_snapshot_use_real_backend() {
    let dir = tempdir().unwrap();
    let ssh = dir.path().join("fake-ssh");
    let keyscan = dir.path().join("fake-keyscan");
    let remote_home = dir.path().join("remote-home");
    enrollment_fake_ssh(&ssh, true, &remote_home);
    enrollment_fake_keyscan(&keyscan);
    let known_path = dir.path().join("known_hosts");
    let canonical = CanonicalSshTarget::from_ssh_config(
        "deploy@example",
        "hostname example\nport 22\nuser deploy\nproxycommand none\n",
    )
    .unwrap();
    let policy = fixture_policy();
    let mut backend = SshEnrollmentBackend::from_canonical_target_for_test(
        canonical.clone(),
        &known_path,
        policy.clone(),
        &ssh,
        &keyscan,
    );
    let known = KnownHostStore::new(&known_path);
    let secrets = EnrollmentSecrets::password("PASSWORD_SENTINEL");
    let observation = backend
        .observe_host_key(
            &ConnectionSpec::manual("deploy", "example", 22).unwrap(),
            &AuthMethod::Password,
            &known,
            &secrets,
        )
        .unwrap();
    known
        .confirm_unknown(&canonical.host_key_name(), &observation)
        .unwrap();
    backend
        .authenticate(
            &ConnectionSpec::manual("deploy", "example", 22).unwrap(),
            &AuthMethod::Password,
            &known,
            &secrets,
        )
        .unwrap();
    let (facts, mut plan) = backend
        .probe_and_plan(
            &ConnectionSpec::manual("deploy", "example", 22).unwrap(),
            &AuthMethod::Password,
            &known,
            &secrets,
        )
        .unwrap();
    assert_eq!(facts.uid, 1000);
    assert_eq!(
        facts.platform,
        TargetPlatform {
            os: ArtifactOs::Linux,
            arch: ArtifactArch::X86_64
        }
    );
    let artifact = fixture_artifact();
    plan.artifact = Some(artifact.evidence());
    plan.refresh_hash().unwrap();
    let draft = EnrollmentDraft::new(
        "production-ssh",
        ConnectionSpec::manual("deploy", "example", 22).unwrap(),
        AuthMethod::Password,
    )
    .unwrap();
    let listener = ListenerPlan::default();
    let request = EnrollmentExecution {
        draft: &draft,
        plan: &plan,
        plan_hash: &plan.plan_hash,
        artifact: &artifact,
        database_seed: None,
        listener: &listener,
        secrets: &secrets,
        collector_credential_token: None,
        collector_machine_id: None,
        collector_hub_url: None,
    };
    let error = backend.execute(request).unwrap_err().to_string();
    assert!(!error.contains("PASSWORD_SENTINEL"));
    assert!(error.contains("manual recovery") || error.contains("rolled back"));
    let snapshot = remote_home.join(".local/state/dirtydash/deployment-rollback");
    // With absent systemd units, rollback now succeeds and cleanup removes the
    // snapshot; retain and inspect it when another rollback step remains unavailable.
    if snapshot.exists() {
        assert!(snapshot.join("database").exists() || snapshot.join(".missing-database").exists());
        assert_eq!(
            fs::read_to_string(snapshot.join("dirtydash-hub.service.loaded"))
                .unwrap()
                .trim(),
            "unloaded"
        );
        assert_eq!(
            fs::read_to_string(snapshot.join("dirtydash-collector.service.active"))
                .unwrap()
                .trim(),
            "inactive"
        );
        let _ = fs::remove_dir_all(snapshot);
    }

    let failing_ssh = dir.path().join("failing-ssh");
    enrollment_fake_ssh(&failing_ssh, false, &remote_home);
    executable_script(
        &failing_ssh,
        r#"#!/bin/sh
set -eu
socket=''
master=0
operation=''
last=''
while [ "$#" -gt 0 ]; do
  arg=$1; shift
  case "$arg" in
    -S) socket=$1; shift;;
    -O) operation=$1; shift;;
    -M) master=1;;
    -o|-p|-i) shift;;
    *) last=$arg;;
  esac
done
if [ "$operation" = exit ]; then rm -f "$socket"; exit 0; fi
if [ "$master" = 1 ]; then printf 'password: ' >&2; IFS= read -r password; : > "$socket"; exit 0; fi
printf 'password=PASSWORD_SENTINEL hostile auth failure\n' >&2
exit 44
"#,
    );
    let mut redacting_backend = SshEnrollmentBackend::from_canonical_target_for_test(
        canonical,
        &known_path,
        policy,
        &failing_ssh,
        &keyscan,
    );
    let error = redacting_backend
        .authenticate(
            &ConnectionSpec::manual("deploy", "example", 22).unwrap(),
            &AuthMethod::Password,
            &known,
            &secrets,
        )
        .unwrap_err()
        .to_string();
    assert!(!error.contains("PASSWORD_SENTINEL"));
}

#[test]
fn secret_types_have_redacted_debug_and_json_never_contains_sentinels() {
    let secrets =
        EnrollmentSecrets::password("PASSWORD_SENTINEL").with_sudo_password("SUDO_SENTINEL");
    assert!(!format!("{secrets:?}").contains("SENTINEL"));
    let draft = EnrollmentDraft::new(
        "safe",
        ConnectionSpec::alias("alias").unwrap(),
        AuthMethod::Password,
    )
    .unwrap();
    let json = serde_json::to_string(&draft).unwrap();
    assert!(!json.contains("SENTINEL"));
}
