//! Durable Hub-side SSH enrollment state machine.
//!
//! The durable interface is deliberately smaller than the workflow.  It
//! persists a sanitized draft and accepts short-lived [`EnrollmentSecrets`]
//! only at host trust, authentication, and execution calls.  Passwords,
//! passphrases, sudo input, key bytes, command output, and installer bytes do
//! not implement `Serialize`, `Debug`, or `Display` and never enter a draft.
//!
//! The five durable states are:
//!
//! 1. target draft;
//! 2. host trust/auth;
//! 3. probe and plan;
//! 4. immutable-plan review;
//! 5. execute/verify/receipt.
//!
//! Unknown host keys require explicit confirmation.  A changed key is a hard
//! block, not a prompt to overwrite managed known-hosts.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::{Zeroize, Zeroizing};

use crate::deployment::{
    DeploymentPlan, DeploymentReceipt, DeploymentRequest, DeploymentRunner, PublisherTrustPolicy,
    RemoteFacts, SshLiveSecrets, SshRemoteExecutor, TargetPlatform, VerifiedArtifact,
};
use crate::listener::ListenerPlan;
use crate::ssh::{canonical_known_hosts_line, host_key_fingerprint, CanonicalSshTarget};

pub const ENROLLMENT_STATE_VERSION: u32 = 1;

/// Focused enrollment seams: sanitized model, durable store, workflow, and
/// production SSH adapter. Re-exports keep each seam discoverable without
/// exposing secret-bearing constructors or arbitrary shell execution.
pub mod model {
    pub use super::{
        AuthMethod, ConnectionSpec, EnrollmentBlocker, EnrollmentDraft,
        EnrollmentExecutionSubstate, EnrollmentReceipt, EnrollmentState, HostKeyObservation,
        HostKeyStatus, PersistedAuthMethod, SanitizedFacts,
    };
}

pub mod store {
    pub use super::{EnrollmentStore, KnownHostStore};
}

pub mod workflow {
    pub use super::{EnrollmentBackend, EnrollmentExecution, EnrollmentWorkflow, HostTrustOutcome};
}

pub mod ssh_backend {
    pub use super::SshEnrollmentBackend;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnrollmentState {
    TargetDraft,
    HostTrustAuth,
    ProbeAndPlan,
    ImmutablePlanReview,
    ExecuteVerifyReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnrollmentBlocker {
    None,
    UnknownHostKey,
    ChangedHostKey,
    AuthenticationFailed,
    SudoFailed,
    PlanInvalidated,
    CleanupRequired,
    ManualRecoveryRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EnrollmentExecutionSubstate {
    #[default]
    NotStarted,
    Preparing,
    Quiescing,
    Installing,
    Activating,
    Restarting,
    HealthChecking,
    ConfiguringListener,
    VerifyingReceipt,
    CleanupRequired,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionSpec {
    SshAlias {
        alias: String,
    },
    Manual {
        user: String,
        host: String,
        port: u16,
    },
}

impl ConnectionSpec {
    pub fn alias(alias: impl Into<String>) -> Result<Self> {
        let alias = alias.into();
        validate_connection_part(&alias, "SSH alias")?;
        Ok(Self::SshAlias { alias })
    }

    pub fn manual(user: impl Into<String>, host: impl Into<String>, port: u16) -> Result<Self> {
        let user = user.into();
        let host = host.into();
        validate_connection_part(&user, "SSH user")?;
        validate_connection_part(&host, "SSH host")?;
        if port == 0 {
            bail!("SSH port must be non-zero");
        }
        Ok(Self::Manual { user, host, port })
    }

    pub fn display_target(&self) -> String {
        match self {
            Self::SshAlias { alias } => alias.clone(),
            Self::Manual { user, host, .. } => format!("{user}@{host}"),
        }
    }

    pub fn display_endpoint(&self) -> String {
        match self {
            Self::SshAlias { alias } => alias.clone(),
            Self::Manual { user, host, port } => format!("{user}@{host}:{port}"),
        }
    }

    pub fn canonical_target(&self) -> Result<CanonicalSshTarget> {
        match self {
            Self::SshAlias { alias } => CanonicalSshTarget::resolve(alias.clone()),
            Self::Manual { user, host, port } => {
                CanonicalSshTarget::resolve(format!("{user}@{host}:{port}"))
            }
        }
    }

    fn host_key_name(&self) -> String {
        match self {
            Self::SshAlias { alias } => alias.clone(),
            Self::Manual { host, port, .. } => format!("[{host}]:{port}"),
        }
    }
}

fn validate_connection_part(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > 255
        || value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(
                    character,
                    '\'' | '"' | '`' | ';' | '&' | '|' | '$' | '<' | '>'
                )
        })
    {
        bail!("{field} is not a safe SSH connection identifier");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMethod {
    KeyPath { path: PathBuf },
    Password,
}

impl AuthMethod {
    pub fn password() -> Self {
        Self::Password
    }

    pub fn key_path(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if path.as_os_str().is_empty()
            || path
                .to_string_lossy()
                .chars()
                .any(|character| character.is_control())
        {
            bail!("SSH key path is invalid");
        }
        Ok(Self::KeyPath { path })
    }

    pub fn persisted_reference(&self) -> PersistedAuthMethod {
        match self {
            Self::KeyPath { path } => PersistedAuthMethod::KeyPath { path: path.clone() },
            Self::Password => PersistedAuthMethod::Password,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PersistedAuthMethod {
    KeyPath { path: PathBuf },
    Password,
}

/// A secret with no formatting or serialization implementation.  The value
/// is zeroized when dropped.  It may be borrowed by an executor only for the
/// duration of one operation.
pub struct SecretString {
    value: Vec<u8>,
}

impl SecretString {
    pub fn new(value: impl AsRef<[u8]>) -> Self {
        Self {
            value: value.as_ref().to_vec(),
        }
    }

    pub fn expose(&self) -> &str {
        std::str::from_utf8(&self.value).unwrap_or("")
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

pub struct EnrollmentSecrets {
    pub password: Option<SecretString>,
    pub key_passphrase: Option<SecretString>,
    pub sudo_password: Option<SecretString>,
}

impl EnrollmentSecrets {
    pub fn none() -> Self {
        Self {
            password: None,
            key_passphrase: None,
            sudo_password: None,
        }
    }

    pub fn password(value: impl AsRef<[u8]>) -> Self {
        Self {
            password: Some(SecretString::new(value)),
            ..Self::none()
        }
    }

    pub fn with_sudo_password(mut self, value: impl AsRef<[u8]>) -> Self {
        self.sudo_password = Some(SecretString::new(value));
        self
    }

    pub fn with_key_passphrase(mut self, value: impl AsRef<[u8]>) -> Self {
        self.key_passphrase = Some(SecretString::new(value));
        self
    }
}

impl fmt::Debug for EnrollmentSecrets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EnrollmentSecrets([REDACTED])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostKeyObservation {
    pub fingerprint: String,
    /// The complete OpenSSH known_hosts key line.  It is public key material,
    /// not a private credential, and is retained only in managed known-hosts.
    pub known_hosts_line: String,
}

impl HostKeyObservation {
    pub fn new(
        fingerprint: impl Into<String>,
        known_hosts_line: impl Into<String>,
    ) -> Result<Self> {
        let fingerprint = fingerprint.into();
        let known_hosts_line = known_hosts_line.into();
        validate_fingerprint(&fingerprint)?;
        if known_hosts_line
            .chars()
            .any(|character| character.is_control())
        {
            bail!("host key line contains control characters");
        }
        Ok(Self {
            fingerprint,
            known_hosts_line,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostKeyStatus {
    Unknown,
    Matching,
    Changed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ManagedHostKey {
    fingerprint: String,
    known_hosts_line: String,
}

/// Dirtydash owns this file rather than modifying the user's global
/// `known_hosts`.  The OpenSSH file is accompanied by a small sidecar index so
/// changed-key checks do not depend on invoking a parser during a wizard.
#[derive(Debug, Clone)]
pub struct KnownHostStore {
    known_hosts_path: PathBuf,
    index_path: PathBuf,
}

impl KnownHostStore {
    pub fn new(known_hosts_path: impl Into<PathBuf>) -> Self {
        let known_hosts_path = known_hosts_path.into();
        let index_path = known_hosts_path.with_extension("fingerprints.json");
        Self {
            known_hosts_path,
            index_path,
        }
    }

    pub fn path(&self) -> &Path {
        &self.known_hosts_path
    }

    pub fn status(&self, host: &str, fingerprint: &str) -> Result<HostKeyStatus> {
        validate_host_key_name(host)?;
        validate_fingerprint(fingerprint)?;
        let records = self.records()?;
        Ok(match records.get(host) {
            None => HostKeyStatus::Unknown,
            Some(record) if record.fingerprint == fingerprint => HostKeyStatus::Matching,
            Some(_) => HostKeyStatus::Changed,
        })
    }

    pub fn confirm_unknown(&self, host: &str, observation: &HostKeyObservation) -> Result<()> {
        validate_host_key_name(host)?;
        match self.status(host, &observation.fingerprint)? {
            HostKeyStatus::Matching => return Ok(()),
            HostKeyStatus::Changed => {
                bail!("remote host key changed; refusing to overwrite managed known-hosts")
            }
            HostKeyStatus::Unknown => {}
        }
        let mut records = self.records()?;
        records.insert(
            host.to_string(),
            ManagedHostKey {
                fingerprint: observation.fingerprint.clone(),
                known_hosts_line: observation.known_hosts_line.clone(),
            },
        );
        let mut known_hosts = String::new();
        for (name, record) in &records {
            if !record.known_hosts_line.is_empty() {
                known_hosts.push_str(&record.known_hosts_line);
            } else {
                known_hosts.push_str(&format!("# dirtydash {name} {}\n", record.fingerprint));
            }
            if !known_hosts.ends_with('\n') {
                known_hosts.push('\n');
            }
        }
        atomic_write(&self.known_hosts_path, known_hosts.as_bytes(), 0o600)?;
        let bytes = serde_json::to_vec_pretty(&records)?;
        atomic_write(&self.index_path, &bytes, 0o600)?;
        Ok(())
    }

    fn records(&self) -> Result<BTreeMap<String, ManagedHostKey>> {
        if !self.index_path.exists() {
            return Ok(BTreeMap::new());
        }
        let bytes = fs::read(&self.index_path)?;
        serde_json::from_slice(&bytes).context("parsing Dirtydash known-host fingerprint index")
    }
}

fn validate_host_key_name(value: &str) -> Result<()> {
    validate_connection_part(value, "known-host key")
}

fn validate_fingerprint(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 200
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        bail!("host key fingerprint is invalid");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizedFacts {
    pub platform: TargetPlatform,
    pub user: String,
    pub uid: u32,
}

impl From<&RemoteFacts> for SanitizedFacts {
    fn from(facts: &RemoteFacts) -> Self {
        Self {
            platform: facts.platform,
            user: facts.user.clone(),
            uid: facts.uid,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentReceipt {
    pub plan_hash: String,
    pub release: String,
    pub artifact_sha256: String,
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub collector_credential_id: Option<String>,
    #[serde(default)]
    pub collector_hub_url: Option<String>,
    pub artifact_size: u64,
    pub publisher_key_id: String,
    pub hub_health_verified: bool,
    pub collector_health_verified: bool,
    pub backfill_queued: bool,
    pub status: String,
}

impl From<DeploymentReceipt> for EnrollmentReceipt {
    fn from(receipt: DeploymentReceipt) -> Self {
        Self {
            plan_hash: receipt.plan_hash,
            release: receipt.release,
            artifact_sha256: receipt.artifact_sha256,
            machine_id: None,
            collector_credential_id: None,
            collector_hub_url: None,
            artifact_size: receipt.artifact_size,
            publisher_key_id: receipt.publisher_key_id,
            hub_health_verified: receipt.hub_health_verified,
            collector_health_verified: receipt.collector_service_verified,
            backfill_queued: receipt.database_seeded,
            status: receipt.status,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentDraft {
    pub state_version: u32,
    pub id: String,
    pub connection: ConnectionSpec,
    /// Non-secret binding for the one Hub-side credential row reserved for
    /// this enrollment. The token itself is never persisted here.
    #[serde(default)]
    pub collector_credential_id: Option<String>,
    #[serde(default)]
    pub collector_credential_state: Option<String>,
    pub auth_method: PersistedAuthMethod,
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    pub state: EnrollmentState,
    pub blocker: EnrollmentBlocker,
    pub host_fingerprint: Option<String>,
    pub facts: Option<SanitizedFacts>,
    pub plan_hash: Option<String>,
    pub reviewed_plan_hash: Option<String>,
    #[serde(default)]
    pub plan: Option<DeploymentPlan>,
    #[serde(default)]
    pub execution_substate: EnrollmentExecutionSubstate,
    pub receipt: Option<EnrollmentReceipt>,
    pub last_error: Option<String>,
    pub attempts: u32,
    pub cleanup_complete: bool,
    pub updated_at: String,
}

impl EnrollmentDraft {
    pub fn new(
        id: impl Into<String>,
        connection: ConnectionSpec,
        auth_method: AuthMethod,
    ) -> Result<Self> {
        let id = id.into();
        validate_id(&id)?;
        Ok(Self {
            state_version: ENROLLMENT_STATE_VERSION,
            id,
            connection,
            collector_credential_id: None,
            collector_credential_state: None,
            auth_method: auth_method.persisted_reference(),
            machine_id: None,
            display_name: None,
            state: EnrollmentState::TargetDraft,
            blocker: EnrollmentBlocker::None,
            host_fingerprint: None,
            facts: None,
            plan_hash: None,
            reviewed_plan_hash: None,
            plan: None,
            execution_substate: EnrollmentExecutionSubstate::NotStarted,
            receipt: None,
            last_error: None,
            attempts: 0,
            cleanup_complete: true,
            updated_at: Utc::now().to_rfc3339(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct EnrollmentStore {
    root: PathBuf,
}

impl EnrollmentStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn save(&self, draft: &EnrollmentDraft) -> Result<()> {
        validate_id(&draft.id)?;
        fs::create_dir_all(&self.root)?;
        let bytes = serde_json::to_vec_pretty(draft)?;
        atomic_write(&self.path_for(&draft.id), &bytes, 0o600)
    }

    pub fn load(&self, id: &str) -> Result<EnrollmentDraft> {
        validate_id(id)?;
        let bytes = fs::read(self.path_for(id))
            .with_context(|| format!("reading enrollment draft {id}"))?;
        let draft: EnrollmentDraft =
            serde_json::from_slice(&bytes).context("parsing enrollment draft")?;
        if draft.state_version != ENROLLMENT_STATE_VERSION {
            bail!("unsupported enrollment draft state version");
        }
        Ok(draft)
    }

    pub fn list(&self) -> Result<Vec<EnrollmentDraft>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut drafts: Vec<EnrollmentDraft> = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            if let Ok(bytes) = fs::read(&path) {
                if let Ok(draft) = serde_json::from_slice(&bytes) {
                    drafts.push(draft);
                }
            }
        }
        drafts.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(drafts)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        validate_id(id)?;
        match fs::remove_file(self.path_for(id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 100
        || !id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        bail!("enrollment ID is invalid");
    }
    Ok(())
}

pub struct EnrollmentExecution<'a> {
    pub draft: &'a EnrollmentDraft,
    pub plan: &'a DeploymentPlan,
    pub plan_hash: &'a str,
    pub artifact: &'a VerifiedArtifact,
    pub database_seed: Option<&'a [u8]>,
    pub listener: &'a ListenerPlan,
    pub secrets: &'a EnrollmentSecrets,
    /// Request-scoped Hub-issued credential material. It is passed only to the
    /// binary-safe remote secret-store stdin seam and is never part of a draft.
    pub collector_credential_token: Option<&'a [u8]>,
    pub collector_machine_id: Option<&'a str>,
    pub collector_hub_url: Option<&'a str>,
}

pub trait EnrollmentBackend {
    /// Return the managed known-host key name for the canonical target.  The
    /// default keeps deterministic in-memory backends independent of ssh -G;
    /// the production backend overrides it with HostKeyAlias/port facts.
    fn host_key_name(&self, connection: &ConnectionSpec) -> Result<String> {
        Ok(connection.host_key_name())
    }

    fn observe_host_key(
        &mut self,
        connection: &ConnectionSpec,
        auth_method: &AuthMethod,
        known_hosts: &KnownHostStore,
        secrets: &EnrollmentSecrets,
    ) -> Result<HostKeyObservation>;

    fn authenticate(
        &mut self,
        connection: &ConnectionSpec,
        auth_method: &AuthMethod,
        known_hosts: &KnownHostStore,
        secrets: &EnrollmentSecrets,
    ) -> Result<()>;

    fn probe_and_plan(
        &mut self,
        connection: &ConnectionSpec,
        auth_method: &AuthMethod,
        known_hosts: &KnownHostStore,
        secrets: &EnrollmentSecrets,
    ) -> Result<(RemoteFacts, DeploymentPlan)>;

    fn execute(&mut self, request: EnrollmentExecution<'_>) -> Result<EnrollmentReceipt>;

    fn cleanup(&mut self, draft: &EnrollmentDraft, secrets: &EnrollmentSecrets) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostTrustOutcome {
    pub status: HostKeyStatus,
    pub fingerprint: String,
    pub confirmed: bool,
}

pub struct EnrollmentWorkflow<B> {
    store: EnrollmentStore,
    known_hosts: KnownHostStore,
    backend: B,
    publisher_policy: PublisherTrustPolicy,
}

impl<B> EnrollmentWorkflow<B> {
    pub fn new(
        store: EnrollmentStore,
        known_hosts: KnownHostStore,
        backend: B,
        publisher_policy: PublisherTrustPolicy,
    ) -> Self {
        Self {
            store,
            known_hosts,
            backend,
            publisher_policy,
        }
    }

    pub fn store(&self) -> &EnrollmentStore {
        &self.store
    }

    pub fn known_hosts(&self) -> &KnownHostStore {
        &self.known_hosts
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn start(&self, draft: EnrollmentDraft) -> Result<()> {
        if draft.state != EnrollmentState::TargetDraft {
            bail!("enrollment draft is not in target-draft state");
        }
        self.store.save(&draft)
    }
}

impl<B: EnrollmentBackend> EnrollmentWorkflow<B> {
    pub fn trust_and_auth(
        &mut self,
        id: &str,
        auth_method: &AuthMethod,
        secrets: &EnrollmentSecrets,
        confirm_fingerprint: Option<&str>,
    ) -> Result<HostTrustOutcome> {
        let mut draft = self.store.load(id)?;
        if !matches!(
            draft.state,
            EnrollmentState::TargetDraft | EnrollmentState::HostTrustAuth
        ) {
            bail!("enrollment draft is not at the host trust/auth step");
        }
        if draft.auth_method != auth_method.persisted_reference() {
            bail!("enrollment authentication method changed; start a new draft");
        }
        let observation = self
            .backend
            .observe_host_key(&draft.connection, auth_method, &self.known_hosts, secrets)
            .map_err(|error| {
                self.record_failure(
                    &mut draft,
                    EnrollmentBlocker::AuthenticationFailed,
                    error,
                    secrets,
                )
            })?;
        let host = self.backend.host_key_name(&draft.connection)?;
        let status = self
            .known_hosts
            .status(&host, &observation.fingerprint)
            .map_err(|error| {
                self.record_failure(
                    &mut draft,
                    EnrollmentBlocker::AuthenticationFailed,
                    error,
                    secrets,
                )
            })?;
        if status == HostKeyStatus::Changed {
            draft.blocker = EnrollmentBlocker::ChangedHostKey;
            draft.host_fingerprint = Some(observation.fingerprint.clone());
            draft.last_error =
                Some("managed host key changed; explicit re-enrollment is required".to_string());
            draft.cleanup_complete = false;
            draft.updated_at = Utc::now().to_rfc3339();
            self.store.save(&draft)?;
            bail!("managed host key changed; refusing connection")
        }
        let confirmed = match status {
            HostKeyStatus::Matching => true,
            HostKeyStatus::Unknown => confirm_fingerprint == Some(observation.fingerprint.as_str()),
            HostKeyStatus::Changed => false,
        };
        if !confirmed {
            draft.blocker = EnrollmentBlocker::UnknownHostKey;
            draft.host_fingerprint = Some(observation.fingerprint.clone());
            draft.last_error =
                Some("unknown host key requires explicit fingerprint confirmation".to_string());
            draft.updated_at = Utc::now().to_rfc3339();
            self.store.save(&draft)?;
            return Ok(HostTrustOutcome {
                status,
                fingerprint: observation.fingerprint,
                confirmed: false,
            });
        }
        if status == HostKeyStatus::Unknown {
            self.known_hosts.confirm_unknown(&host, &observation)?;
        }
        self.backend
            .authenticate(&draft.connection, auth_method, &self.known_hosts, secrets)
            .map_err(|error| {
                let blocker = if error.to_string().to_ascii_lowercase().contains("sudo") {
                    EnrollmentBlocker::SudoFailed
                } else {
                    EnrollmentBlocker::AuthenticationFailed
                };
                self.record_failure(&mut draft, blocker, error, secrets)
            })?;
        draft.state = EnrollmentState::HostTrustAuth;
        draft.blocker = EnrollmentBlocker::None;
        draft.host_fingerprint = Some(observation.fingerprint.clone());
        draft.last_error = None;
        draft.cleanup_complete = true;
        draft.updated_at = Utc::now().to_rfc3339();
        self.store.save(&draft)?;
        Ok(HostTrustOutcome {
            status,
            fingerprint: observation.fingerprint,
            confirmed: true,
        })
    }

    pub fn probe_and_plan(
        &mut self,
        id: &str,
        auth_method: &AuthMethod,
        secrets: &EnrollmentSecrets,
    ) -> Result<DeploymentPlan> {
        self.probe_and_plan_with_seed(id, auth_method, secrets, None)
    }

    pub fn probe_and_plan_with_seed(
        &mut self,
        id: &str,
        auth_method: &AuthMethod,
        secrets: &EnrollmentSecrets,
        database_seed: Option<&[u8]>,
    ) -> Result<DeploymentPlan> {
        let mut draft = self.store.load(id)?;
        if draft.state != EnrollmentState::HostTrustAuth {
            bail!("enrollment draft is not at the probe/plan step");
        }
        let (facts, mut plan) = self
            .backend
            .probe_and_plan(&draft.connection, auth_method, &self.known_hosts, secrets)
            .map_err(|error| {
                self.record_failure(
                    &mut draft,
                    EnrollmentBlocker::AuthenticationFailed,
                    error,
                    secrets,
                )
            })?;
        if let Some(seed) = database_seed {
            plan.seed_intent.requested = true;
            plan.seed_intent.digest = Some(hex::encode(Sha256::digest(seed)));
            plan.seed_intent.size = Some(seed.len() as u64);
            plan.backfill_intent.requested = true;
            plan.backfill_intent.source = Some("sqlite-seed".to_string());
            plan.database_seed = true;
            plan.refresh_hash()?;
        }
        plan.verify_hash()?;
        draft.facts = Some(SanitizedFacts::from(&facts));
        draft.plan = Some(plan.clone());
        draft.plan_hash = Some(plan.plan_hash.clone());
        draft.reviewed_plan_hash = None;
        draft.execution_substate = EnrollmentExecutionSubstate::NotStarted;
        draft.state = EnrollmentState::ProbeAndPlan;
        draft.blocker = EnrollmentBlocker::None;
        draft.last_error = None;
        draft.updated_at = Utc::now().to_rfc3339();
        self.store.save(&draft)?;
        Ok(plan)
    }

    pub fn review(&mut self, id: &str, immutable_plan: &DeploymentPlan) -> Result<()> {
        let mut draft = self.store.load(id)?;
        if draft.state != EnrollmentState::ProbeAndPlan {
            bail!("enrollment draft is not at immutable-plan review");
        }
        if immutable_plan.verify_hash().is_err()
            || draft.plan.as_ref() != Some(immutable_plan)
            || draft.plan_hash.as_deref() != Some(immutable_plan.plan_hash.as_str())
        {
            draft.blocker = EnrollmentBlocker::PlanInvalidated;
            draft.last_error =
                Some("deployment plan changed; probe and review must be repeated".to_string());
            draft.updated_at = Utc::now().to_rfc3339();
            self.store.save(&draft)?;
            bail!("deployment plan changed after probe")
        }
        let evidence = immutable_plan
            .artifact_evidence()
            .context("artifact evidence is required; use review_with_artifact")?;
        if !self.publisher_policy.accepts_evidence(evidence) {
            bail!("deployment plan publisher evidence is not anchored to the enrollment policy");
        }
        draft.reviewed_plan_hash = Some(immutable_plan.plan_hash.clone());
        draft.plan = Some(immutable_plan.clone());
        draft.execution_substate = EnrollmentExecutionSubstate::NotStarted;
        draft.state = EnrollmentState::ImmutablePlanReview;
        draft.blocker = EnrollmentBlocker::None;
        draft.last_error = None;
        draft.updated_at = Utc::now().to_rfc3339();
        self.store.save(&draft)
    }

    /// Finalize review with the exact verified artifact and optional seed
    /// bytes. Their digest/size/key ID and intent become part of the reviewed
    /// plan hash before execution is allowed.
    pub fn review_with_artifact(
        &mut self,
        id: &str,
        immutable_plan: &DeploymentPlan,
        artifact: &VerifiedArtifact,
        database_seed: Option<&[u8]>,
    ) -> Result<DeploymentPlan> {
        let mut draft = self.store.load(id)?;
        if !self.publisher_policy.accepts_evidence(&artifact.evidence()) {
            draft.blocker = EnrollmentBlocker::PlanInvalidated;
            draft.last_error =
                Some("artifact is not anchored to the enrollment publisher policy".to_string());
            draft.updated_at = Utc::now().to_rfc3339();
            self.store.save(&draft)?;
            bail!("artifact is not anchored to the enrollment publisher policy");
        }
        if draft.state != EnrollmentState::ProbeAndPlan
            || immutable_plan.verify_hash().is_err()
            || draft.plan.as_ref() != Some(immutable_plan)
            || draft.plan_hash.as_deref() != Some(immutable_plan.plan_hash.as_str())
        {
            draft.blocker = EnrollmentBlocker::PlanInvalidated;
            draft.last_error = Some("deployment plan changed before artifact review".to_string());
            draft.updated_at = Utc::now().to_rfc3339();
            self.store.save(&draft)?;
            bail!("deployment plan changed before artifact review");
        }
        let mut reviewed = immutable_plan.clone();
        reviewed.artifact = Some(artifact.evidence());
        reviewed.release = artifact.manifest().manifest().release.clone();
        if reviewed.release != immutable_plan.release {
            if let Some(facts) = reviewed.target_facts.as_ref() {
                reviewed.paths = Some(crate::deployment::DeploymentPaths::for_facts(
                    facts,
                    &reviewed.release,
                )?);
                reviewed.rollback = crate::deployment::RollbackData {
                    previous_release: facts.current_release.clone(),
                    database_path: reviewed.paths.as_ref().map(|paths| paths.hub_db.clone()),
                    database_backup: reviewed
                        .paths
                        .as_ref()
                        .map(|paths| paths.hub_db_backup.clone()),
                    database_wal_backup: reviewed
                        .paths
                        .as_ref()
                        .map(|paths| format!("{}-wal.previous", paths.hub_db_backup)),
                    database_shm_backup: reviewed
                        .paths
                        .as_ref()
                        .map(|paths| format!("{}-shm.previous", paths.hub_db_backup)),
                    previous_config: reviewed
                        .paths
                        .as_ref()
                        .map(|paths| paths.config_file.clone()),
                    previous_services: reviewed
                        .paths
                        .as_ref()
                        .map(|paths| vec![paths.service_dir.clone()])
                        .unwrap_or_default(),
                    previous_listener_state: reviewed.listener.tailscale_state,
                    rollback_snapshot_dir: reviewed
                        .paths
                        .as_ref()
                        .map(|paths| format!("{}/deployment-rollback", paths.state_dir)),
                    activation_platform: Some(facts.platform.service_platform()),
                };
            }
        }
        reviewed.seed_intent = crate::deployment::SeedIntent {
            requested: database_seed.is_some(),
            digest: database_seed.map(|seed| hex::encode(Sha256::digest(seed))),
            size: database_seed.map(|seed| seed.len() as u64),
        };
        reviewed.backfill_intent = crate::deployment::BackfillIntent {
            requested: database_seed.is_some(),
            source: database_seed.map(|_| "sqlite-seed".to_string()),
        };
        reviewed.database_seed = database_seed.is_some();
        reviewed.refresh_hash()?;
        draft.plan = Some(reviewed.clone());
        draft.plan_hash = Some(reviewed.plan_hash.clone());
        draft.reviewed_plan_hash = Some(reviewed.plan_hash.clone());
        draft.state = EnrollmentState::ImmutablePlanReview;
        draft.blocker = EnrollmentBlocker::None;
        draft.execution_substate = EnrollmentExecutionSubstate::NotStarted;
        draft.last_error = None;
        draft.updated_at = Utc::now().to_rfc3339();
        self.store.save(&draft)?;
        Ok(reviewed)
    }

    pub fn execute(
        &mut self,
        id: &str,
        immutable_plan: &DeploymentPlan,
        artifact: &VerifiedArtifact,
        database_seed: Option<&[u8]>,
        listener: &ListenerPlan,
        secrets: &EnrollmentSecrets,
    ) -> Result<EnrollmentReceipt> {
        self.execute_with_collector(
            id,
            immutable_plan,
            artifact,
            database_seed,
            listener,
            secrets,
            None,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn execute_with_collector(
        &mut self,
        id: &str,
        immutable_plan: &DeploymentPlan,
        artifact: &VerifiedArtifact,
        database_seed: Option<&[u8]>,
        listener: &ListenerPlan,
        secrets: &EnrollmentSecrets,
        collector_credential_token: Option<&[u8]>,
        collector_machine_id: Option<&str>,
        collector_hub_url: Option<&str>,
    ) -> Result<EnrollmentReceipt> {
        let mut draft = self.store.load(id)?;
        if draft.blocker == EnrollmentBlocker::ManualRecoveryRequired {
            bail!("manual recovery is required before enrollment can be retried");
        }
        if !matches!(
            draft.state,
            EnrollmentState::ImmutablePlanReview | EnrollmentState::ExecuteVerifyReceipt
        ) || (draft.state == EnrollmentState::ExecuteVerifyReceipt && !draft.cleanup_complete)
        {
            bail!(
                "enrollment draft is not ready for execution; cleanup must complete before retry"
            );
        }
        if immutable_plan.verify_hash().is_err()
            || draft.plan.as_ref() != Some(immutable_plan)
            || draft.reviewed_plan_hash.as_deref() != Some(immutable_plan.plan_hash.as_str())
            || draft.plan_hash.as_deref() != Some(immutable_plan.plan_hash.as_str())
        {
            draft.blocker = EnrollmentBlocker::PlanInvalidated;
            draft.last_error =
                Some("reviewed deployment plan no longer matches; refusing execution".to_string());
            draft.updated_at = Utc::now().to_rfc3339();
            self.store.save(&draft)?;
            bail!("reviewed deployment plan is stale")
        }
        if let Err(error) = validate_execution_intent(
            &self.publisher_policy,
            immutable_plan,
            artifact,
            database_seed,
        ) {
            let message = redact_error(&error.to_string(), secrets);
            draft.blocker = EnrollmentBlocker::PlanInvalidated;
            draft.state = EnrollmentState::ImmutablePlanReview;
            draft.execution_substate = EnrollmentExecutionSubstate::NotStarted;
            draft.cleanup_complete = true;
            draft.last_error = Some(message.clone());
            draft.updated_at = Utc::now().to_rfc3339();
            self.store.save(&draft)?;
            return Err(anyhow::anyhow!(message));
        }
        draft.attempts = draft.attempts.saturating_add(1);
        draft.state = EnrollmentState::ExecuteVerifyReceipt;
        draft.blocker = EnrollmentBlocker::None;
        draft.execution_substate = EnrollmentExecutionSubstate::Preparing;
        draft.cleanup_complete = false;
        draft.updated_at = Utc::now().to_rfc3339();
        self.store.save(&draft)?;
        match self.backend.execute(EnrollmentExecution {
            draft: &draft,
            plan: immutable_plan,
            plan_hash: &immutable_plan.plan_hash,
            artifact,
            database_seed,
            listener,
            secrets,
            collector_credential_token,
            collector_machine_id,
            collector_hub_url,
        }) {
            Ok(mut receipt) => {
                if receipt.plan_hash != immutable_plan.plan_hash {
                    return self.fail_execute(
                        &mut draft,
                        EnrollmentBlocker::PlanInvalidated,
                        "installer receipt did not match the reviewed plan",
                        secrets,
                    );
                }
                if let Some(machine_id) = collector_machine_id {
                    let Some(credential_id) = draft.collector_credential_id.clone() else {
                        return self.fail_execute(
                            &mut draft,
                            EnrollmentBlocker::PlanInvalidated,
                            "Collector credential binding is missing from the enrollment draft",
                            secrets,
                        );
                    };
                    receipt.machine_id = Some(machine_id.to_string());
                    receipt.collector_credential_id = Some(credential_id);
                    receipt.collector_hub_url = collector_hub_url.map(ToOwned::to_owned);
                    draft.collector_credential_state =
                        Some("installed-pending-confirmation".to_string());
                }
                draft.receipt = Some(receipt.clone());
                draft.last_error = None;
                draft.blocker = EnrollmentBlocker::None;
                draft.execution_substate = EnrollmentExecutionSubstate::Completed;
                draft.cleanup_complete = true;
                draft.updated_at = Utc::now().to_rfc3339();
                self.store.save(&draft)?;
                Ok(receipt)
            }
            Err(error) => {
                let blocker = if error
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("manual recovery")
                {
                    EnrollmentBlocker::ManualRecoveryRequired
                } else {
                    EnrollmentBlocker::CleanupRequired
                };
                self.fail_execute(
                    &mut draft,
                    blocker,
                    &redact_error(&error.to_string(), secrets),
                    secrets,
                )
            }
        }
    }

    fn fail_execute(
        &mut self,
        draft: &mut EnrollmentDraft,
        blocker: EnrollmentBlocker,
        message: &str,
        secrets: &EnrollmentSecrets,
    ) -> Result<EnrollmentReceipt> {
        let cleanup_result = self.backend.cleanup(draft, secrets);
        draft.blocker = if cleanup_result.is_ok() {
            blocker.clone()
        } else if blocker == EnrollmentBlocker::ManualRecoveryRequired {
            EnrollmentBlocker::ManualRecoveryRequired
        } else {
            EnrollmentBlocker::CleanupRequired
        };
        draft.cleanup_complete = cleanup_result.is_ok();
        draft.execution_substate = EnrollmentExecutionSubstate::CleanupRequired;
        draft.last_error = Some(redact_error(message, secrets));
        draft.updated_at = Utc::now().to_rfc3339();
        self.store.save(draft)?;
        cleanup_result.context("enrollment execution failed and cleanup failed")?;
        if blocker == EnrollmentBlocker::ManualRecoveryRequired {
            bail!("enrollment execution failed; manual recovery is required")
        }
        bail!("enrollment execution failed; retry remains available at the execute step")
    }

    pub fn retry_cleanup(&mut self, id: &str, secrets: &EnrollmentSecrets) -> Result<()> {
        let mut draft = self.store.load(id)?;
        if draft.blocker == EnrollmentBlocker::ManualRecoveryRequired {
            bail!("manual recovery is required; automatic cleanup is disabled");
        }
        self.backend.cleanup(&draft, secrets).map_err(|error| {
            self.record_failure(
                &mut draft,
                EnrollmentBlocker::CleanupRequired,
                error,
                secrets,
            )
        })?;
        draft.cleanup_complete = true;
        draft.blocker = EnrollmentBlocker::None;
        draft.execution_substate = EnrollmentExecutionSubstate::NotStarted;
        draft.state = EnrollmentState::ImmutablePlanReview;
        draft.last_error = None;
        draft.updated_at = Utc::now().to_rfc3339();
        self.store.save(&draft)
    }

    fn record_failure(
        &self,
        draft: &mut EnrollmentDraft,
        blocker: EnrollmentBlocker,
        error: anyhow::Error,
        secrets: &EnrollmentSecrets,
    ) -> anyhow::Error {
        let message = redact_error(&error.to_string(), secrets);
        draft.blocker = blocker;
        draft.last_error = Some(message.clone());
        draft.updated_at = Utc::now().to_rfc3339();
        let _ = self.store.save(draft);
        anyhow::anyhow!(message)
    }
}

fn validate_execution_intent(
    publisher_policy: &PublisherTrustPolicy,
    plan: &DeploymentPlan,
    artifact: &VerifiedArtifact,
    database_seed: Option<&[u8]>,
) -> Result<()> {
    let seed_requested = database_seed.is_some();
    if plan.seed_intent.requested != seed_requested
        || plan.backfill_intent.requested != seed_requested
    {
        bail!("seed/backfill intent does not match the reviewed deployment plan");
    }
    if let Some(seed) = database_seed {
        let digest = hex::encode(Sha256::digest(seed));
        if plan.seed_intent.digest.as_deref() != Some(digest.as_str())
            || plan.seed_intent.size != Some(seed.len() as u64)
        {
            bail!("database seed changed after plan review");
        }
    }
    let evidence = plan
        .artifact_evidence()
        .context("reviewed plan has no verified artifact evidence")?;
    if !publisher_policy.accepts_evidence(evidence) {
        bail!("reviewed plan is not anchored to the enrollment publisher policy");
    }
    let actual = artifact.evidence();
    if evidence != &actual {
        bail!("artifact evidence changed after plan review");
    }
    Ok(())
}

fn redact_error(value: &str, secrets: &EnrollmentSecrets) -> String {
    let mut result = value.to_string();
    for secret in [
        secrets.password.as_ref(),
        secrets.key_passphrase.as_ref(),
        secrets.sudo_password.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        let value = secret.expose();
        if !value.is_empty() {
            result = result.replace(value, "[REDACTED]");
        }
    }
    for marker in [
        "password=",
        "passphrase=",
        "sudo_password=",
        "secret=",
        "token=",
    ] {
        if let Some(index) = result.to_ascii_lowercase().find(marker) {
            let end = result[index..]
                .find(|character: char| {
                    character.is_whitespace() || character == ',' || character == ';'
                })
                .map(|offset| index + offset)
                .unwrap_or(result.len());
            result.replace_range(index..end, &format!("{marker}[REDACTED]"));
        }
    }
    if result.len() > 400 {
        result.truncate(400);
        result.push('…');
    }
    result
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().context("managed file has no parent")?;
    fs::create_dir_all(parent)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = parent.join(format!(
        ".{}.tmp-{}-{nonce}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("dirtydash"),
        std::process::id()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
    }
    fs::rename(temp, path)?;
    Ok(())
}

/// A production SSH adapter for the trust, probe, and install workflow. Every
/// operation remains typed and uses one canonical target; password, key-
/// passphrase, and sudo bytes travel only through the deployment module's
/// prompt-aware live PTY, never through `sshpass`, argv, environment, temp
/// files, or persisted state.
#[derive(Debug, Clone)]
pub struct SshEnrollmentBackend {
    target: CanonicalSshTarget,
    known_hosts_path: PathBuf,
    publisher_policy: PublisherTrustPolicy,
    ssh_program: PathBuf,
    keyscan_program: PathBuf,
}

#[derive(Debug, Clone, Copy)]
enum SshOperation {
    Authenticate,
    Probe,
    Cleanup,
}

impl SshOperation {
    fn command(self) -> &'static str {
        match self {
            Self::Authenticate => "printf dirtydash-authenticated",
            Self::Probe => "set -eu; printf 'os=%s\\n' \"$(uname -s)\"; printf 'arch=%s\\n' \"$(uname -m)\"; printf 'user=%s\\n' \"$(id -un)\"; printf 'uid=%s\\n' \"$(id -u)\"; printf 'home=%s\\n' \"$HOME\"",
            Self::Cleanup => "true",
        }
    }
}

impl SshEnrollmentBackend {
    pub fn new(
        target: impl Into<String>,
        known_hosts_path: impl Into<PathBuf>,
        publisher_policy: PublisherTrustPolicy,
    ) -> Result<Self> {
        Ok(Self {
            target: CanonicalSshTarget::resolve(target.into())?,
            known_hosts_path: known_hosts_path.into(),
            publisher_policy,
            ssh_program: PathBuf::from("ssh"),
            keyscan_program: PathBuf::from("ssh-keyscan"),
        })
    }

    #[cfg(test)]
    pub(crate) fn from_canonical_target_for_test(
        target: CanonicalSshTarget,
        known_hosts_path: impl Into<PathBuf>,
        publisher_policy: PublisherTrustPolicy,
        ssh_program: impl Into<PathBuf>,
        keyscan_program: impl Into<PathBuf>,
    ) -> Self {
        Self {
            target,
            known_hosts_path: known_hosts_path.into(),
            publisher_policy,
            ssh_program: ssh_program.into(),
            keyscan_program: keyscan_program.into(),
        }
    }

    fn live_secrets(&self, secrets: &EnrollmentSecrets) -> Result<SshLiveSecrets> {
        SshLiveSecrets::new(
            secrets
                .password
                .as_ref()
                .map(|value| value.expose().as_bytes()),
            secrets
                .key_passphrase
                .as_ref()
                .map(|value| value.expose().as_bytes()),
            secrets
                .sudo_password
                .as_ref()
                .map(|value| value.expose().as_bytes()),
        )
    }

    fn executor(
        &self,
        auth_method: &AuthMethod,
        secrets: &EnrollmentSecrets,
    ) -> Result<SshRemoteExecutor> {
        let mut executor = SshRemoteExecutor::from_canonical_target(
            self.target.clone(),
            self.known_hosts_path.clone(),
        )?
        .with_ssh_program(self.ssh_program.clone());
        if let AuthMethod::KeyPath { path } = auth_method {
            executor = executor.with_key_path(path.clone())?;
        }
        Ok(executor.with_live_secrets(self.live_secrets(secrets)?))
    }

    fn run_operation(
        &mut self,
        _connection: &ConnectionSpec,
        auth_method: &AuthMethod,
        operation: SshOperation,
        secrets: &EnrollmentSecrets,
        sudo: bool,
    ) -> Result<String> {
        let mut executor = self.executor(auth_method, secrets)?;
        let command = if sudo {
            // This token is deliberately fixed so only the sudo prompt, not
            // arbitrary remote output, can release the sudo secret.
            "sudo -S -p 'DIRTYDASH_SUDO_PROMPT' sh -c 'printf ok'"
        } else {
            operation.command()
        };
        Ok(executor.live_command(command, sudo)?.stdout)
    }
}

impl EnrollmentBackend for SshEnrollmentBackend {
    fn host_key_name(&self, _connection: &ConnectionSpec) -> Result<String> {
        Ok(self.target.host_key_name())
    }

    fn observe_host_key(
        &mut self,
        _connection: &ConnectionSpec,
        _auth_method: &AuthMethod,
        _known_hosts: &KnownHostStore,
        _secrets: &EnrollmentSecrets,
    ) -> Result<HostKeyObservation> {
        // `ssh-keyscan` is intentionally only an observation.  It does not
        // alter known-hosts and the workflow requires a separate confirmation.
        let output = Command::new(&self.keyscan_program)
            .args(self.target.keyscan_args())
            .output()
            .context("observing remote SSH host key")?;
        if !output.status.success() || output.stdout.is_empty() {
            bail!("SSH host-key observation failed");
        }
        let line = String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if line.is_empty() {
            bail!("SSH host-key observation returned no key");
        }
        let line = canonical_known_hosts_line(&self.target, &line)?;
        let fingerprint = host_key_fingerprint(&line)?;
        HostKeyObservation::new(fingerprint, line)
    }

    fn authenticate(
        &mut self,
        connection: &ConnectionSpec,
        auth_method: &AuthMethod,
        _known_hosts: &KnownHostStore,
        secrets: &EnrollmentSecrets,
    ) -> Result<()> {
        let sudo = secrets.sudo_password.is_some();
        self.run_operation(
            connection,
            auth_method,
            SshOperation::Authenticate,
            secrets,
            sudo,
        )
        .map(|_| ())
    }

    fn probe_and_plan(
        &mut self,
        connection: &ConnectionSpec,
        auth_method: &AuthMethod,
        known_hosts: &KnownHostStore,
        secrets: &EnrollmentSecrets,
    ) -> Result<(RemoteFacts, DeploymentPlan)> {
        let output =
            self.run_operation(connection, auth_method, SshOperation::Probe, secrets, false)?;
        let facts = RemoteFacts::parse_probe(&output)?;
        let listener = ListenerPlan::default();
        let plan = DeploymentPlan::for_facts_with_details(
            self.target.destination(),
            env!("CARGO_PKG_VERSION"),
            &facts,
            listener,
            crate::deployment::DeploymentPlanDetails {
                artifact: None,
                database_seed: false,
                seed_bytes: None,
                ssh_target: Some(self.target.clone()),
            },
        )?;
        let _ = known_hosts;
        Ok((facts, plan))
    }

    fn execute(&mut self, request: EnrollmentExecution<'_>) -> Result<EnrollmentReceipt> {
        let EnrollmentExecution {
            draft,
            plan,
            plan_hash,
            artifact,
            database_seed,
            listener,
            secrets,
            collector_credential_token,
            collector_machine_id,
            collector_hub_url,
        } = request;
        let auth = auth_from_persisted(&draft.auth_method)?;
        if plan.plan_hash != plan_hash {
            bail!("enrollment plan hash changed before execution");
        }
        // Password enrollment uses the same prompt-aware live stdin seam as
        // trust/probe.  The deployment runner therefore never creates a
        // second unauthenticated SSH connection or asks for a password in an
        // argv/env/temp-file fallback.
        let executor = self.executor(&auth, secrets)?;
        let request = DeploymentRequest {
            target: self.target.destination(),
            release: plan.release.clone(),
            listener: listener.clone(),
            database_seed: database_seed.map(ToOwned::to_owned),
            approved_plan_hash: Some(plan_hash.to_string()),
            collector_credential_token: collector_credential_token
                .map(|token| Zeroizing::new(token.to_owned())),
            collector_machine_id: collector_machine_id.map(ToOwned::to_owned),
            collector_hub_url: collector_hub_url.map(ToOwned::to_owned),
        };
        let mut runner = DeploymentRunner::new(executor, self.publisher_policy.clone())
            .with_reviewed_plan(plan.clone());
        let receipt = runner.apply(&request, artifact)?;
        let mut enrollment_receipt = EnrollmentReceipt::from(receipt);
        enrollment_receipt.artifact_sha256 = artifact.descriptor().sha256.clone();
        Ok(enrollment_receipt)
    }

    fn cleanup(&mut self, draft: &EnrollmentDraft, secrets: &EnrollmentSecrets) -> Result<()> {
        let auth = auth_from_persisted(&draft.auth_method)?;
        self.run_operation(
            &draft.connection,
            &auth,
            SshOperation::Cleanup,
            secrets,
            false,
        )
        .map(|_| ())
    }
}

fn auth_from_persisted(auth: &PersistedAuthMethod) -> Result<AuthMethod> {
    match auth {
        PersistedAuthMethod::KeyPath { path } => AuthMethod::key_path(path.clone()),
        PersistedAuthMethod::Password => Ok(AuthMethod::Password),
    }
}

/// Legacy pull remotes are converted only into drafts.  This pure conversion
/// never receives an executor, opens SSH, or mutates a known-host file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyMachineDraft {
    pub id: String,
    pub display_name: String,
    pub connection: ConnectionSpec,
    pub source_roots_json: String,
    pub enrolled: bool,
    pub conversion_note: String,
}

pub fn convert_legacy_remote_drafts(
    remotes: &[crate::config::RemoteConfig],
) -> Result<Vec<LegacyMachineDraft>> {
    let mut drafts = Vec::new();
    for remote in remotes {
        validate_id(&remote.name)?;
        let connection = if remote.ssh_target.contains('@') || remote.ssh_target.contains(':') {
            let (user, host) = remote
                .ssh_target
                .split_once('@')
                .map(|(user, host)| (user.to_string(), host.to_string()))
                .unwrap_or_else(|| ("".to_string(), remote.ssh_target.clone()));
            if !user.is_empty() && !host.is_empty() {
                ConnectionSpec::manual(user, host, 22)?
            } else {
                ConnectionSpec::alias(remote.ssh_target.clone())?
            }
        } else {
            ConnectionSpec::alias(remote.ssh_target.clone())?
        };
        drafts.push(LegacyMachineDraft {
            id: format!("legacy-{}", remote.name),
            display_name: remote.name.clone(),
            connection,
            source_roots_json: serde_json::to_string(&remote.source_roots)?,
            enrolled: false,
            conversion_note: "legacy remote converted to an un-enrolled Machine draft; explicit review and enrollment are required".to_string(),
        });
    }
    Ok(drafts)
}

/// Persist the explicit legacy conversion as target-draft enrollment records.
/// This function only writes sanitized drafts; it deliberately has no backend
/// or executor parameter, so conversion cannot accidentally enroll a host.
pub fn persist_legacy_remote_drafts(
    remotes: &[crate::config::RemoteConfig],
    store: &EnrollmentStore,
) -> Result<Vec<LegacyMachineDraft>> {
    let drafts = convert_legacy_remote_drafts(remotes)?;
    for draft in &drafts {
        store.save(&EnrollmentDraft::new(
            draft.id.clone(),
            draft.connection.clone(),
            AuthMethod::Password,
        )?)?;
    }
    Ok(drafts)
}

#[cfg(test)]
#[path = "enrollment_tests.rs"]
mod tests;
