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
use std::process::{Command, Stdio};
use zeroize::Zeroize;

use crate::deployment::{
    DeploymentPlan, DeploymentReceipt, DeploymentRequest, DeploymentRunner, RemoteFacts,
    SshRemoteExecutor, TargetPlatform, VerifiedArtifact,
};
use crate::listener::ListenerPlan;

pub const ENROLLMENT_STATE_VERSION: u32 = 1;

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
            Self::Manual { user, host, port } => format!("{user}@{host}:{port}"),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
            artifact_sha256: String::new(),
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
    pub auth_method: PersistedAuthMethod,
    pub state: EnrollmentState,
    pub blocker: EnrollmentBlocker,
    pub host_fingerprint: Option<String>,
    pub facts: Option<SanitizedFacts>,
    pub plan_hash: Option<String>,
    pub reviewed_plan_hash: Option<String>,
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
            auth_method: auth_method.persisted_reference(),
            state: EnrollmentState::TargetDraft,
            blocker: EnrollmentBlocker::None,
            host_fingerprint: None,
            facts: None,
            plan_hash: None,
            reviewed_plan_hash: None,
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
}

pub trait EnrollmentBackend {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostTrustOutcome {
    pub status: HostKeyStatus,
    pub fingerprint: String,
    pub confirmed: bool,
}

pub struct EnrollmentWorkflow<B> {
    store: EnrollmentStore,
    known_hosts: KnownHostStore,
    backend: B,
}

impl<B> EnrollmentWorkflow<B> {
    pub fn new(store: EnrollmentStore, known_hosts: KnownHostStore, backend: B) -> Self {
        Self {
            store,
            known_hosts,
            backend,
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
        let host = draft.connection.host_key_name();
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
        let mut draft = self.store.load(id)?;
        if draft.state != EnrollmentState::HostTrustAuth {
            bail!("enrollment draft is not at the probe/plan step");
        }
        let (facts, plan) = self
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
        draft.facts = Some(SanitizedFacts::from(&facts));
        draft.plan_hash = Some(plan.plan_hash.clone());
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
            || draft.plan_hash.as_deref() != Some(immutable_plan.plan_hash.as_str())
        {
            draft.blocker = EnrollmentBlocker::PlanInvalidated;
            draft.last_error =
                Some("deployment plan changed; probe and review must be repeated".to_string());
            draft.updated_at = Utc::now().to_rfc3339();
            self.store.save(&draft)?;
            bail!("deployment plan changed after probe")
        }
        draft.reviewed_plan_hash = Some(immutable_plan.plan_hash.clone());
        draft.state = EnrollmentState::ImmutablePlanReview;
        draft.blocker = EnrollmentBlocker::None;
        draft.last_error = None;
        draft.updated_at = Utc::now().to_rfc3339();
        self.store.save(&draft)
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
        let mut draft = self.store.load(id)?;
        if draft.state != EnrollmentState::ImmutablePlanReview {
            bail!("enrollment draft is not ready for execution");
        }
        if immutable_plan.verify_hash().is_err()
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
        draft.attempts = draft.attempts.saturating_add(1);
        draft.state = EnrollmentState::ExecuteVerifyReceipt;
        draft.blocker = EnrollmentBlocker::None;
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
        }) {
            Ok(receipt) => {
                if receipt.plan_hash != immutable_plan.plan_hash {
                    return self.fail_execute(
                        &mut draft,
                        EnrollmentBlocker::PlanInvalidated,
                        "installer receipt did not match the reviewed plan",
                        secrets,
                    );
                }
                draft.receipt = Some(receipt.clone());
                draft.last_error = None;
                draft.blocker = EnrollmentBlocker::None;
                draft.cleanup_complete = true;
                draft.updated_at = Utc::now().to_rfc3339();
                self.store.save(&draft)?;
                Ok(receipt)
            }
            Err(error) => self.fail_execute(
                &mut draft,
                EnrollmentBlocker::CleanupRequired,
                &redact_error(&error.to_string(), secrets),
                secrets,
            ),
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
            blocker
        } else {
            EnrollmentBlocker::CleanupRequired
        };
        draft.cleanup_complete = cleanup_result.is_ok();
        draft.last_error = Some(redact_error(message, secrets));
        draft.updated_at = Utc::now().to_rfc3339();
        self.store.save(draft)?;
        cleanup_result.context("enrollment execution failed and cleanup failed")?;
        bail!("enrollment execution failed; retry remains available at the execute step")
    }

    pub fn retry_cleanup(&mut self, id: &str, secrets: &EnrollmentSecrets) -> Result<()> {
        let mut draft = self.store.load(id)?;
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
    let temp = parent.join(format!(
        ".{}.tmp-{}",
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

/// A production SSH adapter for the trust/probe half of the workflow.  The
/// install method is intentionally still typed and uses the same fixed
/// invocation rules; live password/PTY behavior is delegated to the user's
/// terminal, never to `sshpass`, an environment variable, or a temp file.
#[derive(Debug, Clone)]
pub struct SshEnrollmentBackend {
    target: String,
    known_hosts_path: PathBuf,
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
    pub fn new(target: impl Into<String>, known_hosts_path: impl Into<PathBuf>) -> Result<Self> {
        let target = target.into();
        validate_connection_part(&target, "SSH target")?;
        Ok(Self {
            target,
            known_hosts_path: known_hosts_path.into(),
        })
    }

    fn ssh_args(
        &self,
        connection: &ConnectionSpec,
        auth_method: &AuthMethod,
        interactive: bool,
    ) -> Result<Vec<String>> {
        validate_connection_part(&self.target, "SSH target")?;
        let mut args = vec![
            "-o".to_string(),
            "StrictHostKeyChecking=yes".to_string(),
            "-o".to_string(),
            format!("UserKnownHostsFile={}", self.known_hosts_path.display()),
            "-o".to_string(),
            "ConnectTimeout=10".to_string(),
        ];
        if !interactive {
            args.extend(["-o".to_string(), "BatchMode=yes".to_string()]);
        }
        if let AuthMethod::KeyPath { path } = auth_method {
            args.extend([
                "-i".to_string(),
                path.display().to_string(),
                "-o".to_string(),
                "IdentitiesOnly=yes".to_string(),
            ]);
        }
        match connection {
            ConnectionSpec::SshAlias { alias } => args.push(alias.clone()),
            ConnectionSpec::Manual { user, host, port } => {
                args.extend(["-p".to_string(), port.to_string(), format!("{user}@{host}")]);
            }
        }
        Ok(args)
    }

    fn run_operation(
        &self,
        connection: &ConnectionSpec,
        auth_method: &AuthMethod,
        operation: SshOperation,
        secrets: &EnrollmentSecrets,
        sudo: bool,
    ) -> Result<String> {
        let interactive = matches!(auth_method, AuthMethod::Password)
            || secrets.key_passphrase.is_some()
            || (sudo && secrets.sudo_password.is_none());
        let mut args = self.ssh_args(connection, auth_method, interactive)?;
        if sudo {
            args.push("sudo -S -p '' sh -c 'printf ok'".to_string());
        } else {
            args.push(operation.command().to_string());
        }
        let mut process = Command::new("ssh");
        process.args(&args);
        if interactive {
            process.stdin(Stdio::inherit());
        } else {
            process.stdin(Stdio::piped());
        }
        process.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = process
            .spawn()
            .context("starting SSH enrollment operation")?;
        if !interactive {
            if let Some(mut stdin) = child.stdin.take() {
                if let Some(password) = &secrets.sudo_password {
                    stdin.write_all(password.expose().as_bytes())?;
                    stdin.write_all(b"\n")?;
                }
            }
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let operation = if sudo {
                "SSH sudo operation failed"
            } else {
                "SSH enrollment operation failed"
            };
            bail!("{operation}: {}", redact_error(&stderr, secrets));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

impl EnrollmentBackend for SshEnrollmentBackend {
    fn observe_host_key(
        &mut self,
        connection: &ConnectionSpec,
        _auth_method: &AuthMethod,
        _known_hosts: &KnownHostStore,
        _secrets: &EnrollmentSecrets,
    ) -> Result<HostKeyObservation> {
        // `ssh-keyscan` is intentionally only an observation.  It does not
        // alter known-hosts and the workflow requires a separate confirmation.
        let mut keyscan_args = Vec::new();
        match connection {
            ConnectionSpec::SshAlias { alias } => keyscan_args.push(alias.clone()),
            ConnectionSpec::Manual { host, port, .. } => {
                keyscan_args.extend(["-p".to_string(), port.to_string(), host.clone()]);
            }
        }
        let output = Command::new("ssh-keyscan")
            .args(&keyscan_args)
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
        let fingerprint = format!("sha256:{}", hex::encode(Sha256::digest(line.as_bytes())));
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
        let plan = DeploymentPlan::for_facts(
            connection.display_target(),
            env!("CARGO_PKG_VERSION"),
            &facts,
            listener,
            false,
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
            secrets: _secrets,
        } = request;
        let auth = auth_from_persisted(&draft.auth_method)?;
        let AuthMethod::KeyPath { path: key_path } = auth else {
            // OpenSSH password authentication needs a live PTY from the
            // operator.  It is supported for trust/auth above, but an
            // unattended artifact stream cannot safely multiplex a password
            // prompt and binary stdin.  Refuse rather than silently falling
            // back to sshpass, an environment variable, or a temp file.
            bail!("password-authenticated install requires an interactive PTY; retry with a reviewed key-path method");
        };
        if plan.plan_hash != plan_hash {
            bail!("enrollment plan hash changed before execution");
        }
        let executor = SshRemoteExecutor::new(
            draft.connection.display_target(),
            self.known_hosts_path.clone(),
        )?
        .with_key_path(key_path)?;
        let request = DeploymentRequest {
            target: draft.connection.display_target(),
            release: artifact.manifest.manifest.release.clone(),
            listener: listener.clone(),
            database_seed: database_seed.map(ToOwned::to_owned),
            approved_plan_hash: Some(plan_hash.to_string()),
        };
        let mut runner = DeploymentRunner::new(executor);
        let receipt = runner.apply(&request, artifact)?;
        let mut enrollment_receipt = EnrollmentReceipt::from(receipt);
        enrollment_receipt.artifact_sha256 = artifact.descriptor.sha256.clone();
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
mod tests {
    use super::*;
    use crate::config::{RemoteConfig, SourceRoot};
    use crate::deployment::{
        ArtifactArch, ArtifactDescriptor, ArtifactManifest, ArtifactOs, MANIFEST_SCHEMA_VERSION,
    };
    use ed25519_dalek::{Signer, SigningKey};
    use tempfile::tempdir;

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
                release: artifact.manifest.manifest.release.clone(),
                artifact_sha256: artifact.descriptor.sha256.clone(),
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
            key_id: "test".to_string(),
            manifest,
            signature: String::new(),
        };
        signed.signature = hex::encode(key.sign(&signed.signing_bytes().unwrap()).to_bytes());
        signed
            .verify(&key.verifying_key().to_bytes())
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
        let workflow = EnrollmentWorkflow::new(store, known, backend);
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
            "alice@host.example:2222"
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
        let (mut workflow, draft, plan, dir) = workflow();
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
        workflow.review("machine-1", &plan).unwrap();
        let store = workflow.store.clone();
        let known = workflow.known_hosts.clone();
        let backend = std::mem::take(&mut workflow.backend);
        let mut restarted = EnrollmentWorkflow::new(store, known, backend);
        let receipt = restarted
            .execute(
                "machine-1",
                &plan,
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
        workflow.review("machine-1", &plan).unwrap();
        workflow.backend_mut().execute_error = Some("PASSWORD_SENTINEL".to_string());
        assert!(workflow
            .execute(
                "machine-1",
                &plan,
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
}
