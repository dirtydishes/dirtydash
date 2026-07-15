//! Signed release deployment for a Hub and its local Collector.
//!
//! The public seam is intentionally narrow:
//!
//! * [`ArtifactManifest`] verifies an Ed25519-signed, SHA-256-checksummed
//!   release and selects one of four supported Linux/macOS targets.
//! * [`DeploymentPlan`] is a typed, serializable, secret-free description of
//!   the remote mutations.
//! * [`RemoteExecutor`] is the only seam that can perform those mutations;
//!   the production adapter uses fixed-allowlist SSH options, a PTY only for
//!   authentication/ControlMaster setup, and a separate binary-safe channel
//!   for non-secret artifact/database bytes.
//! * [`DeploymentRunner`] executes the immutable plan and performs atomic
//!   current-pointer rollback and cleanup on every failed mutation.
//!
//! No source builds, Docker assumptions, shell environment secrets, or private
//! signing material are part of this module.

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
#[cfg(unix)]
use portable_pty::MasterPty;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

use crate::listener::{ListenerAccessMode, ListenerPlan, TailscaleServeState};
use crate::service::{ServicePlatform, ServiceSpec};
use crate::ssh::{validate_target_input, CanonicalSshTarget};

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_HUB_PORT: u16 = 4599;
pub const DEFAULT_REMOTE_BASE: &str = ".local/share/dirtydash";
pub const DEFAULT_CONFIG_BASE: &str = ".config/dirtydash";

/// Focused public seams over the deployment implementation. The concrete
/// adapters remain private to the runner; callers can reason about artifact
/// verification, plan review, and typed execution independently.
pub mod artifact {
    pub use super::{
        ArtifactArch, ArtifactDescriptor, ArtifactEvidence, ArtifactManifest, ArtifactOs,
        PublisherTrustPolicy, SignedArtifactManifest, TargetPlatform, VerifiedArtifact,
        VerifiedArtifactManifest,
    };
}

pub mod plan {
    pub use super::{
        BackfillIntent, DeploymentPaths, DeploymentPlan, DeploymentStep, DeploymentStepKind,
        ListenerExposure, RemoteFacts, RollbackData, SeedIntent,
    };
}

pub mod executor {
    pub use super::{
        RemoteAction, RemoteExecutor, RemoteResult, RemoteStatus, SshLiveSecrets, SshRemoteExecutor,
    };
}

pub mod runner {
    pub use super::{
        DeploymentCheckpoint, DeploymentReceipt, DeploymentRequest, DeploymentRunner,
        DeploymentStateStore, PublisherTrustPolicy,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactOs {
    Linux,
    Macos,
}

impl fmt::Display for ArtifactOs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactArch {
    X86_64,
    Arm64,
}

impl fmt::Display for ArtifactArch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::X86_64 => "x86_64",
            Self::Arm64 => "arm64",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TargetPlatform {
    pub os: ArtifactOs,
    pub arch: ArtifactArch,
}

impl fmt::Display for TargetPlatform {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}-{}", self.os, self.arch)
    }
}

impl TargetPlatform {
    pub fn from_uname(os: &str, arch: &str) -> Result<Self> {
        let os = match os.trim().to_ascii_lowercase().as_str() {
            "linux" => ArtifactOs::Linux,
            "darwin" | "macos" | "mac os" => ArtifactOs::Macos,
            other => bail!("unsupported remote operating system: {other}"),
        };
        let arch = match arch.trim().to_ascii_lowercase().as_str() {
            "x86_64" | "amd64" | "x64" => ArtifactArch::X86_64,
            "aarch64" | "arm64" => ArtifactArch::Arm64,
            other => bail!("unsupported remote architecture: {other}"),
        };
        Ok(Self { os, arch })
    }

    pub fn service_platform(self) -> ServicePlatform {
        match self.os {
            ArtifactOs::Linux => ServicePlatform::Systemd,
            ArtifactOs::Macos => ServicePlatform::Launchd,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDescriptor {
    pub platform: TargetPlatform,
    pub file: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    pub schema_version: u32,
    pub release: String,
    pub artifacts: Vec<ArtifactDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedArtifactManifest {
    pub key_id: String,
    pub manifest: ArtifactManifest,
    /// Lower-case hexadecimal Ed25519 signature over the canonical JSON
    /// payload containing `key_id` and `manifest`.
    pub signature: String,
}

/// An explicitly configured publisher trust anchor.
///
/// The key ID and fingerprint are supplied by durable configuration by the
/// application layer.  Keeping this policy separate from a signed manifest
/// means a caller must choose the anchor before it can obtain any verified
/// artifact bytes; there is no unchecked manifest-to-artifact constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublisherTrustPolicy {
    key_id: String,
    fingerprint: String,
    public_key: [u8; 32],
}

impl PublisherTrustPolicy {
    /// Construct a policy from an application-owned, pinned key ID and
    /// fingerprint.  Callers should load these values from durable trusted
    /// configuration, not from the release manifest or artifact directory.
    pub fn new(
        key_id: impl Into<String>,
        fingerprint: impl Into<String>,
        public_key: &[u8],
    ) -> Result<Self> {
        let key_id = key_id.into();
        let fingerprint = fingerprint.into().to_ascii_lowercase();
        validate_key_id(&key_id)?;
        let public_key: [u8; 32] = public_key
            .try_into()
            .map_err(|_| anyhow::anyhow!("Ed25519 public key must contain 32 bytes"))?;
        let actual = publisher_fingerprint(&public_key);
        if actual != fingerprint {
            bail!("publisher key fingerprint does not match the supplied public key");
        }
        Ok(Self {
            key_id,
            fingerprint,
            public_key,
        })
    }

    pub fn fingerprint(public_key: &[u8]) -> Result<String> {
        let public_key: [u8; 32] = public_key
            .try_into()
            .map_err(|_| anyhow::anyhow!("Ed25519 public key must contain 32 bytes"))?;
        Ok(publisher_fingerprint(&public_key))
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn fingerprint_value(&self) -> &str {
        &self.fingerprint
    }

    pub fn verify(&self, signed: &SignedArtifactManifest) -> Result<VerifiedArtifactManifest> {
        signed.verify_with_policy(self)
    }

    pub fn accepts_evidence(&self, evidence: &ArtifactEvidence) -> bool {
        evidence.key_id == self.key_id && evidence.publisher_fingerprint == self.fingerprint
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifactManifest {
    pub(crate) key_id: String,
    pub(crate) publisher_fingerprint: String,
    pub(crate) manifest: ArtifactManifest,
    pub(crate) manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactEvidence {
    digest: String,
    size: u64,
    key_id: String,
    publisher_fingerprint: String,
    manifest_sha256: String,
}

impl ArtifactEvidence {
    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn publisher_fingerprint(&self) -> &str {
        &self.publisher_fingerprint
    }

    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifact {
    descriptor: ArtifactDescriptor,
    bytes: Vec<u8>,
    manifest: VerifiedArtifactManifest,
}

impl VerifiedArtifact {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn descriptor(&self) -> &ArtifactDescriptor {
        &self.descriptor
    }

    pub fn manifest(&self) -> &VerifiedArtifactManifest {
        &self.manifest
    }

    pub fn evidence(&self) -> ArtifactEvidence {
        ArtifactEvidence {
            digest: self.descriptor.sha256.clone(),
            size: self.descriptor.size,
            key_id: self.manifest.key_id.clone(),
            publisher_fingerprint: self.manifest.publisher_fingerprint.clone(),
            manifest_sha256: self.manifest.manifest_sha256.clone(),
        }
    }
}

impl SignedArtifactManifest {
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        #[derive(Serialize)]
        struct Payload<'a> {
            key_id: &'a str,
            manifest: &'a ArtifactManifest,
        }
        serde_json::to_vec(&Payload {
            key_id: &self.key_id,
            manifest: &self.manifest,
        })
        .context("serializing artifact manifest signing payload")
    }

    /// Verify only through an explicitly anchored publisher policy.  The key
    /// ID and fingerprint are both checked before the signature so replacing
    /// a manifest and a public-key file together cannot silently authorize a
    /// different publisher.
    fn verify_with_policy(
        &self,
        policy: &PublisherTrustPolicy,
    ) -> Result<VerifiedArtifactManifest> {
        if self.key_id != policy.key_id {
            bail!("signed artifact key ID is not on the allowed publisher list");
        }
        let actual_fingerprint = publisher_fingerprint(&policy.public_key);
        if actual_fingerprint != policy.fingerprint {
            bail!("allowed publisher fingerprint is invalid");
        }
        self.verify_signature(&policy.public_key)
    }

    fn verify_signature(&self, public_key: &[u8]) -> Result<VerifiedArtifactManifest> {
        if self.manifest.schema_version != MANIFEST_SCHEMA_VERSION {
            bail!("unsupported artifact manifest schema version");
        }
        validate_release(&self.manifest.release)?;
        let mut platforms = std::collections::BTreeSet::new();
        for artifact in &self.manifest.artifacts {
            validate_artifact_descriptor(artifact)?;
            if !platforms.insert(artifact.platform) {
                bail!("signed release contains duplicate platform artifacts");
            }
        }
        validate_key_id(&self.key_id)?;
        let key_bytes: [u8; 32] = public_key
            .try_into()
            .map_err(|_| anyhow::anyhow!("Ed25519 public key must contain 32 bytes"))?;
        let key = VerifyingKey::from_bytes(&key_bytes)
            .map_err(|_| anyhow::anyhow!("Ed25519 public key is invalid"))?;
        let signature_bytes = decode_hex(&self.signature, 64)
            .context("artifact manifest signature must be 64-byte hexadecimal")?;
        let signature = Signature::from_slice(&signature_bytes)
            .map_err(|_| anyhow::anyhow!("artifact manifest signature is invalid"))?;
        let payload = self.signing_bytes()?;
        key.verify(&payload, &signature)
            .map_err(|_| anyhow::anyhow!("artifact manifest signature verification failed"))?;
        let manifest_sha256 = hex::encode(Sha256::digest(&payload));
        Ok(VerifiedArtifactManifest {
            key_id: self.key_id.clone(),
            publisher_fingerprint: publisher_fingerprint(&key_bytes),
            manifest: self.manifest.clone(),
            manifest_sha256,
        })
    }
}

impl VerifiedArtifactManifest {
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn manifest(&self) -> &ArtifactManifest {
        &self.manifest
    }

    pub fn publisher_fingerprint(&self) -> &str {
        &self.publisher_fingerprint
    }

    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    pub fn select(&self, platform: TargetPlatform) -> Result<&ArtifactDescriptor> {
        let mut matches = self
            .manifest
            .artifacts
            .iter()
            .filter(|entry| entry.platform == platform);
        let Some(entry) = matches.next() else {
            bail!("signed release has no artifact for {platform}");
        };
        if matches.next().is_some() {
            bail!("signed release contains duplicate artifacts for {platform}");
        }
        validate_artifact_descriptor(entry)?;
        Ok(entry)
    }

    pub fn verify_artifact(
        &self,
        platform: TargetPlatform,
        bytes: Vec<u8>,
    ) -> Result<VerifiedArtifact> {
        let descriptor = self.select(platform)?.clone();
        if descriptor.size != bytes.len() as u64 {
            bail!("artifact size does not match the signed manifest");
        }
        let actual = hex::encode(Sha256::digest(&bytes));
        if actual != descriptor.sha256.to_ascii_lowercase() {
            bail!("artifact checksum does not match the signed manifest");
        }
        Ok(VerifiedArtifact {
            descriptor,
            bytes,
            manifest: self.clone(),
        })
    }
}

fn validate_artifact_descriptor(entry: &ArtifactDescriptor) -> Result<()> {
    let digest = entry.sha256.trim();
    if digest.len() != 64
        || !digest
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        bail!("signed artifact checksum is not a SHA-256 hex digest");
    }
    validate_filename(&entry.file)
}

fn validate_release(release: &str) -> Result<()> {
    if release.is_empty()
        || !release.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
    {
        bail!("release identifier contains unsupported characters");
    }
    Ok(())
}

fn validate_filename(file: &str) -> Result<()> {
    if file.is_empty()
        || file == "."
        || file == ".."
        || file.contains('/')
        || file.contains('\\')
        || file.chars().any(|character| character.is_control())
    {
        bail!("artifact filename must be a single safe path component");
    }
    Ok(())
}

fn validate_key_id(value: &str) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > 128
        || value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || !character.is_ascii()
                || matches!(character, '\'' | '"' | '`' | ';' | '&' | '|' | '$')
        })
    {
        bail!("artifact signing key ID is invalid");
    }
    Ok(())
}

fn publisher_fingerprint(public_key: &[u8; 32]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(public_key)))
}

fn decode_hex(value: &str, expected_bytes: usize) -> Result<Vec<u8>> {
    if value.len() != expected_bytes * 2
        || !value.chars().all(|character| character.is_ascii_hexdigit())
    {
        bail!("invalid hexadecimal value");
    }
    hex::decode(value).context("decoding hexadecimal value")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteFacts {
    pub platform: TargetPlatform,
    pub user: String,
    pub uid: u32,
    pub home: String,
    pub current_release: Option<String>,
}

impl RemoteFacts {
    pub fn parse_probe(output: &str) -> Result<Self> {
        let mut fields = std::collections::BTreeMap::new();
        for line in output.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            fields.insert(key.trim(), value.trim());
        }
        let platform = TargetPlatform::from_uname(
            fields
                .get("os")
                .copied()
                .context("platform probe omitted os")?,
            fields
                .get("arch")
                .copied()
                .context("platform probe omitted arch")?,
        )?;
        let user = fields
            .get("user")
            .copied()
            .context("platform probe omitted user")?;
        let home = fields
            .get("home")
            .copied()
            .context("platform probe omitted home")?;
        let uid = fields
            .get("uid")
            .copied()
            .context("platform probe omitted uid")?
            .parse::<u32>()
            .context("platform probe returned an invalid uid")?;
        if uid == 0 || user == "root" {
            bail!("remote deployment requires a non-root SSH user");
        }
        validate_remote_text(user, "remote user")?;
        validate_remote_home(home)?;
        let current_release = fields
            .get("current")
            .copied()
            .filter(|value| !value.is_empty())
            .map(|value| {
                validate_remote_text(value, "current release")?;
                if !value.starts_with('/') {
                    bail!("current release must be an absolute path");
                }
                Ok(value.to_string())
            })
            .transpose()?;
        Ok(Self {
            platform,
            user: user.to_string(),
            uid,
            home: home.to_string(),
            current_release,
        })
    }
}

fn validate_ssh_target(value: &str) -> Result<()> {
    validate_target_input(value)
}

fn validate_remote_text(value: &str, field: &str) -> Result<()> {
    if value.is_empty() || value.chars().any(|character| character.is_control()) {
        bail!("{field} is empty or contains control characters");
    }
    Ok(())
}

fn validate_remote_home(value: &str) -> Result<()> {
    validate_remote_text(value, "remote home")?;
    if !value.starts_with('/') || value == "/" {
        bail!("remote home must be an absolute non-root path");
    }
    Ok(())
}

const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const SQLITE_HEADER_HEX: &str = "53514c69746520666f726d6174203300";

/// Validate the SQLite file header without invoking a platform SQLite CLI.
/// This is intentionally byte-level: shell variables never receive a NUL.
pub fn validate_sqlite_header(bytes: &[u8]) -> Result<()> {
    if bytes.len() < SQLITE_HEADER.len() || &bytes[..SQLITE_HEADER.len()] != SQLITE_HEADER {
        bail!("SQLite backup does not have the expected 16-byte header");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentPaths {
    pub home: String,
    pub config_dir: String,
    pub state_dir: String,
    pub data_dir: String,
    pub releases_dir: String,
    pub release_dir: String,
    pub current: String,
    pub hub_db: String,
    pub hub_db_backup: String,
    pub collector_db: String,
    pub config_file: String,
    pub known_hosts: String,
    pub service_dir: String,
}

impl DeploymentPaths {
    pub fn for_facts(facts: &RemoteFacts, release: &str) -> Result<Self> {
        validate_release(release)?;
        let home = facts.home.trim_end_matches('/');
        let config_dir = format!("{home}/{DEFAULT_CONFIG_BASE}");
        let state_dir = format!("{home}/.local/state/dirtydash");
        let data_dir = format!("{state_dir}/data");
        let releases_dir = format!("{home}/{DEFAULT_REMOTE_BASE}/releases");
        let release_dir = format!("{releases_dir}/{release}");
        let current = format!("{home}/{DEFAULT_REMOTE_BASE}/current");
        let service_dir = match facts.platform.service_platform() {
            ServicePlatform::Systemd => format!("{config_dir}/systemd/user"),
            ServicePlatform::Launchd => format!("{home}/Library/LaunchAgents"),
        };
        Ok(Self {
            home: home.to_string(),
            config_dir: config_dir.clone(),
            state_dir,
            data_dir: data_dir.clone(),
            releases_dir,
            release_dir: release_dir.clone(),
            current: current.clone(),
            hub_db: format!("{data_dir}/dirtydash.sqlite3"),
            hub_db_backup: format!("{data_dir}/dirtydash.sqlite3.previous"),
            collector_db: format!("{data_dir}/dirtydash-collector.sqlite3"),
            config_file: format!("{config_dir}/config.toml"),
            known_hosts: format!("{config_dir}/known_hosts"),
            service_dir,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeploymentStepKind {
    DetectPlatform,
    VerifySignedArtifact,
    PrepareUserOwnedPaths,
    UploadHubCollectorArtifact,
    SnapshotRollbackState,
    QuiesceServices,
    OptionalDatabaseSeed,
    InstallDatabaseSeed,
    InstallRuntimeConfig,
    InstallNonRootServices,
    AtomicallyActivateRelease,
    RestartServices,
    HealthCheck,
    ConfigureTailscaleServe,
    VerifyReceipt,
    Cleanup,
    Rollback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentStep {
    pub id: String,
    pub kind: DeploymentStepKind,
    pub description: String,
    pub mutates_remote: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentPlan {
    /// Human-readable input retained for receipts.  `ssh_target` is the
    /// canonical execution target and is the value used by every SSH seam.
    /// All fields are private so callers can inspect or persist a plan but
    /// cannot manufacture an approved plan that reaches mutation.
    pub(crate) target: String,
    pub(crate) ssh_target: Option<CanonicalSshTarget>,
    pub(crate) release: String,
    pub(crate) platform: Option<TargetPlatform>,
    pub(crate) target_facts: Option<RemoteFacts>,
    pub(crate) artifact: Option<ArtifactEvidence>,
    pub(crate) listener: ListenerPlan,
    pub(crate) exposure: ListenerExposure,
    pub(crate) seed_intent: SeedIntent,
    pub(crate) backfill_intent: BackfillIntent,
    pub(crate) database_seed: bool,
    pub(crate) paths: Option<DeploymentPaths>,
    pub(crate) rollback: RollbackData,
    pub(crate) steps: Vec<DeploymentStep>,
    pub(crate) rollback_steps: Vec<DeploymentStep>,
    pub(crate) plan_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListenerExposure {
    PrivateTailscale,
    PublicHttps,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedIntent {
    pub requested: bool,
    pub digest: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackfillIntent {
    pub requested: bool,
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackData {
    pub previous_release: Option<String>,
    pub database_path: Option<String>,
    pub database_backup: Option<String>,
    pub database_wal_backup: Option<String>,
    pub database_shm_backup: Option<String>,
    pub previous_config: Option<String>,
    /// Snapshot location is the source of truth after the remote mutation;
    /// this enum is only a plan-time fallback for legacy adapters.
    pub previous_services: Vec<String>,
    pub previous_listener_state: TailscaleServeState,
    pub rollback_snapshot_dir: Option<String>,
    pub activation_platform: Option<ServicePlatform>,
}

#[derive(Clone)]
pub(crate) struct DeploymentPlanDetails {
    pub(crate) artifact: Option<ArtifactEvidence>,
    pub(crate) database_seed: bool,
    pub(crate) seed_bytes: Option<Vec<u8>>,
    pub(crate) ssh_target: Option<CanonicalSshTarget>,
}

impl RollbackData {
    fn for_paths(
        paths: Option<&DeploymentPaths>,
        facts: Option<&RemoteFacts>,
        listener: &ListenerPlan,
    ) -> Self {
        Self {
            previous_release: facts.and_then(|facts| facts.current_release.clone()),
            database_path: paths.map(|paths| paths.hub_db.clone()),
            database_backup: paths.map(|paths| paths.hub_db_backup.clone()),
            database_wal_backup: paths.map(|paths| format!("{}-wal.previous", paths.hub_db_backup)),
            database_shm_backup: paths.map(|paths| format!("{}-shm.previous", paths.hub_db_backup)),
            previous_config: paths.map(|paths| paths.config_file.clone()),
            previous_services: paths
                .map(|paths| vec![paths.service_dir.clone()])
                .unwrap_or_default(),
            previous_listener_state: listener.tailscale_state,
            rollback_snapshot_dir: paths
                .map(|paths| format!("{}/deployment-rollback", paths.state_dir)),
            activation_platform: facts.map(|facts| facts.platform.service_platform()),
        }
    }
}

impl DeploymentPlan {
    pub fn skeleton(
        target: impl Into<String>,
        release: impl Into<String>,
        listener: ListenerPlan,
        database_seed: bool,
    ) -> Result<Self> {
        let target = target.into();
        let release = release.into();
        validate_ssh_target(&target)?;
        validate_release(&release)?;
        listener.validate()?;
        let exposure = match listener.access_mode {
            ListenerAccessMode::TailscaleServe => ListenerExposure::PrivateTailscale,
            ListenerAccessMode::PublicHttps => ListenerExposure::PublicHttps,
        };
        let rollback = RollbackData::for_paths(None, None, &listener);
        let mut plan = Self {
            target,
            ssh_target: None,
            release,
            platform: None,
            target_facts: None,
            artifact: None,
            listener,
            exposure,
            seed_intent: SeedIntent {
                requested: database_seed,
                digest: None,
                size: None,
            },
            backfill_intent: BackfillIntent {
                requested: database_seed,
                source: database_seed.then(|| "sqlite-seed".to_string()),
            },
            database_seed,
            paths: None,
            rollback,
            steps: generic_steps(database_seed),
            rollback_steps: rollback_steps(),
            plan_hash: String::new(),
        };
        plan.refresh_hash()?;
        Ok(plan)
    }

    pub fn for_facts(
        target: impl Into<String>,
        release: impl Into<String>,
        facts: &RemoteFacts,
        listener: ListenerPlan,
        database_seed: bool,
    ) -> Result<Self> {
        Self::for_facts_with_details(
            target,
            release,
            facts,
            listener,
            DeploymentPlanDetails {
                artifact: None,
                database_seed,
                seed_bytes: None,
                ssh_target: None,
            },
        )
    }

    pub(crate) fn for_facts_with_details(
        target: impl Into<String>,
        release: impl Into<String>,
        facts: &RemoteFacts,
        listener: ListenerPlan,
        details: DeploymentPlanDetails,
    ) -> Result<Self> {
        let mut plan = Self::skeleton(target, release, listener, details.database_seed)?;
        plan.platform = Some(facts.platform);
        plan.target_facts = Some(facts.clone());
        plan.ssh_target = details.ssh_target;
        plan.artifact = details.artifact;
        plan.paths = Some(DeploymentPaths::for_facts(facts, &plan.release)?);
        plan.seed_intent = SeedIntent {
            requested: details.database_seed,
            digest: details
                .seed_bytes
                .as_deref()
                .map(|bytes| hex::encode(Sha256::digest(bytes))),
            size: details
                .seed_bytes
                .as_deref()
                .map(|bytes| bytes.len() as u64),
        };
        plan.backfill_intent = BackfillIntent {
            requested: details.database_seed,
            source: details.database_seed.then(|| "sqlite-seed".to_string()),
        };
        plan.rollback = RollbackData::for_paths(plan.paths.as_ref(), Some(facts), &plan.listener);
        plan.steps = concrete_steps(details.database_seed, facts.platform.service_platform());
        plan.refresh_hash()?;
        Ok(plan)
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn release(&self) -> &str {
        &self.release
    }

    pub fn plan_hash(&self) -> &str {
        &self.plan_hash
    }

    pub fn listener(&self) -> &ListenerPlan {
        &self.listener
    }

    pub fn artifact_evidence(&self) -> Option<&ArtifactEvidence> {
        self.artifact.as_ref()
    }

    pub fn target_facts(&self) -> Option<&RemoteFacts> {
        self.target_facts.as_ref()
    }

    pub fn paths(&self) -> Option<&DeploymentPaths> {
        self.paths.as_ref()
    }

    pub fn rollback(&self) -> &RollbackData {
        &self.rollback
    }

    pub(crate) fn refresh_hash(&mut self) -> Result<()> {
        let previous = std::mem::take(&mut self.plan_hash);
        let _ = previous;
        let bytes = serde_json::to_vec(self)?;
        self.plan_hash = hex::encode(Sha256::digest(bytes));
        Ok(())
    }

    pub fn is_immutable(&self) -> bool {
        !self.plan_hash.is_empty()
    }

    pub fn verify_hash(&self) -> Result<()> {
        let expected = self.plan_hash.clone();
        let mut canonical = self.clone();
        canonical.refresh_hash()?;
        if expected != canonical.plan_hash {
            bail!("deployment plan hash does not match its immutable contents");
        }
        Ok(())
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("serializing deployment plan")
    }
}

fn generic_steps(database_seed: bool) -> Vec<DeploymentStep> {
    let mut steps = vec![
        step(
            "detect",
            DeploymentStepKind::DetectPlatform,
            "Detect remote OS, architecture, user, home, and current release",
            false,
        ),
        step(
            "verify-artifact",
            DeploymentStepKind::VerifySignedArtifact,
            "Verify the signed manifest, target selection, size, and SHA-256 checksum",
            false,
        ),
        step(
            "prepare",
            DeploymentStepKind::PrepareUserOwnedPaths,
            "Create versioned user-owned release, state, and config paths",
            true,
        ),
        step(
            "upload",
            DeploymentStepKind::UploadHubCollectorArtifact,
            "Upload one verified Hub/Collector artifact over SSH stdin",
            true,
        ),
        step(
            "snapshot-rollback",
            DeploymentStepKind::SnapshotRollbackState,
            "Snapshot current release, runtime config, service definitions, database, and listener state",
            true,
        ),
        step(
            "quiesce",
            DeploymentStepKind::QuiesceServices,
            "Quiesce Hub and Collector before SQLite or release replacement",
            true,
        ),
    ];
    if database_seed {
        steps.push(step(
            "seed",
            DeploymentStepKind::OptionalDatabaseSeed,
            "Transfer the optional database seed through stdin and atomically install it",
            true,
        ));
    }
    steps.extend([
        step(
            "config",
            DeploymentStepKind::InstallRuntimeConfig,
            "Install non-secret runtime trust configuration atomically",
            true,
        ),
        step(
            "services",
            DeploymentStepKind::InstallNonRootServices,
            "Install non-root systemd user or launchd service definitions",
            true,
        ),
        step(
            "activate",
            DeploymentStepKind::AtomicallyActivateRelease,
            "Atomically switch the current release symlink",
            true,
        ),
        step(
            "restart",
            DeploymentStepKind::RestartServices,
            "Restart Hub and local Collector services",
            true,
        ),
        step(
            "health",
            DeploymentStepKind::HealthCheck,
            "Verify the local Hub health endpoint and Collector process",
            false,
        ),
        step(
            "listener",
            DeploymentStepKind::ConfigureTailscaleServe,
            "Configure the private listener or preserve explicit public trust mode",
            true,
        ),
        step(
            "receipt",
            DeploymentStepKind::VerifyReceipt,
            "Verify service, release, and backfill receipt",
            false,
        ),
        step(
            "cleanup",
            DeploymentStepKind::Cleanup,
            "Remove only this deployment's temporary files",
            true,
        ),
    ]);
    steps
}

fn concrete_steps(database_seed: bool, _platform: ServicePlatform) -> Vec<DeploymentStep> {
    generic_steps(database_seed)
}

fn rollback_steps() -> Vec<DeploymentStep> {
    vec![
        step(
            "rollback",
            DeploymentStepKind::Rollback,
            "Restore the previous current release atomically",
            true,
        ),
        step(
            "cleanup",
            DeploymentStepKind::Cleanup,
            "Remove the failed release and temporary files",
            true,
        ),
    ]
}

fn step(
    id: &str,
    kind: DeploymentStepKind,
    description: &str,
    mutates_remote: bool,
) -> DeploymentStep {
    DeploymentStep {
        id: id.to_string(),
        kind,
        description: description.to_string(),
        mutates_remote,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteStatus {
    Success,
    ConsentRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteResult {
    pub status: RemoteStatus,
    pub stdout: String,
    pub stderr: String,
}

impl RemoteResult {
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            status: RemoteStatus::Success,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    pub fn consent_required(output: impl Into<String>) -> Self {
        Self {
            status: RemoteStatus::ConsentRequired,
            stdout: output.into(),
            stderr: String::new(),
        }
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "operation")]
pub enum RemoteAction {
    PreparePaths {
        paths: DeploymentPaths,
    },
    SnapshotRollbackState {
        paths: DeploymentPaths,
        platform: ServicePlatform,
        listener: ListenerPlan,
    },
    /// Stop both user services before touching SQLite or the current release.
    QuiesceServices {
        platform: ServicePlatform,
    },
    AtomicallyActivate {
        current: String,
        release: String,
        platform: ServicePlatform,
    },
    InstallDatabaseSeed {
        seed_path: String,
        database_path: String,
        backup_path: String,
        wal_backup_path: String,
        shm_backup_path: String,
    },
    InstallRuntimeConfig {
        config_path: String,
        config: String,
    },
    InstallService {
        path: String,
        contents: String,
        mode: u32,
    },
    RestartServices {
        platform: ServicePlatform,
    },
    HealthCheck {
        port: u16,
        platform: ServicePlatform,
    },
    ConfigureTailscale {
        port: u16,
    },
    VerifyReceipt {
        release: String,
        port: u16,
        platform: ServicePlatform,
    },
    Rollback {
        current: String,
        previous: Option<String>,
        database_path: Option<String>,
        database_backup: Option<String>,
        database_wal_backup: Option<String>,
        database_shm_backup: Option<String>,
        config_path: Option<String>,
        service_dir: Option<String>,
        platform: ServicePlatform,
        listener: Option<ListenerPlan>,
        snapshot_dir: Option<String>,
    },
    Cleanup {
        release: String,
        remove_release: bool,
        database_backup: Option<String>,
        database_wal_backup: Option<String>,
        database_shm_backup: Option<String>,
        temporary_seed: Option<String>,
        rollback_snapshot: Option<String>,
    },
}

/// The executor seam carries only typed actions.  It has no method accepting
/// an arbitrary shell string and no secret parameter.
pub trait RemoteExecutor {
    fn detect(&mut self) -> Result<RemoteFacts>;
    fn run(&mut self, action: RemoteAction) -> Result<RemoteResult>;
    fn upload(&mut self, destination: &str, bytes: &[u8], mode: u32) -> Result<RemoteResult>;

    /// Production adapters expose the one canonical target used for all
    /// operations.  Test adapters may omit it because their typed actions do
    /// not spawn SSH.
    fn canonical_target(&self) -> Option<&CanonicalSshTarget> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentReceipt {
    pub target: String,
    pub release: String,
    pub platform: TargetPlatform,
    pub plan_hash: String,
    pub artifact_sha256: String,
    pub artifact_size: u64,
    pub publisher_key_id: String,
    pub hub_health_verified: bool,
    pub collector_service_verified: bool,
    pub database_seeded: bool,
    pub tailscale_state: TailscaleServeState,
    pub status: String,
    pub cleanup_performed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentCheckpoint {
    pub target: String,
    pub release: String,
    pub plan_hash: String,
    pub status: String,
    pub tailscale_state: TailscaleServeState,
    pub receipt: Option<DeploymentReceipt>,
}

#[derive(Debug, Clone)]
pub struct DeploymentStateStore {
    path: PathBuf,
}

impl DeploymentStateStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> Result<Option<DeploymentCheckpoint>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&self.path)
            .with_context(|| format!("reading deployment checkpoint {}", self.path.display()))?;
        serde_json::from_slice(&bytes).context("parsing deployment checkpoint")
    }

    pub fn save(&self, checkpoint: &DeploymentCheckpoint) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("deployment checkpoint has no parent")?;
        fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(checkpoint)?;
        atomic_write(&self.path, &bytes, 0o600)
    }

    pub(crate) fn save_plan(&self, plan: &DeploymentPlan) -> Result<()> {
        plan.verify_hash()?;
        let path = self.plan_path();
        let bytes = serde_json::to_vec_pretty(plan)?;
        atomic_write(&path, &bytes, 0o600)
    }

    pub(crate) fn load_plan(&self) -> Result<Option<DeploymentPlan>> {
        let path = self.plan_path();
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path)
            .with_context(|| format!("reading deployment plan {}", path.display()))?;
        let plan: DeploymentPlan =
            serde_json::from_slice(&bytes).context("parsing persisted deployment plan")?;
        plan.verify_hash()?;
        Ok(Some(plan))
    }

    pub(crate) fn mark_reviewed(&self, plan: &DeploymentPlan) -> Result<()> {
        self.save_plan(plan)?;
        self.save(&DeploymentCheckpoint {
            target: plan.target.clone(),
            release: plan.release.clone(),
            plan_hash: plan.plan_hash.clone(),
            status: "reviewed".to_string(),
            tailscale_state: plan.listener.tailscale_state,
            receipt: None,
        })
    }

    fn plan_path(&self) -> PathBuf {
        self.path.with_file_name(format!(
            "{}.plan.json",
            self.path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("deployment-checkpoint")
        ))
    }

    pub fn clear(&self) -> Result<()> {
        for path in [&self.path, &self.plan_path()] {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct DeploymentRequest {
    pub target: String,
    pub release: String,
    pub listener: ListenerPlan,
    pub database_seed: Option<Vec<u8>>,
    pub approved_plan_hash: Option<String>,
}

impl std::fmt::Debug for DeploymentRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeploymentRequest")
            .field("target", &self.target)
            .field("release", &self.release)
            .field("listener", &self.listener)
            .field(
                "database_seed",
                &self
                    .database_seed
                    .as_ref()
                    .map(|seed| ("[REDACTED]", seed.len())),
            )
            .field("approved_plan_hash", &self.approved_plan_hash)
            .finish()
    }
}

impl DeploymentRequest {
    pub fn new(
        target: impl Into<String>,
        release: impl Into<String>,
        listener: ListenerPlan,
    ) -> Self {
        Self {
            target: target.into(),
            release: release.into(),
            listener,
            database_seed: None,
            approved_plan_hash: None,
        }
    }
}

pub struct DeploymentRunner<E> {
    executor: E,
    publisher_policy: PublisherTrustPolicy,
    state_store: Option<DeploymentStateStore>,
    reviewed_plan: Option<DeploymentPlan>,
}

impl<E> DeploymentRunner<E> {
    pub fn new(executor: E, publisher_policy: PublisherTrustPolicy) -> Self {
        Self {
            executor,
            publisher_policy,
            state_store: None,
            reviewed_plan: None,
        }
    }

    pub fn with_state_store(mut self, store: DeploymentStateStore) -> Self {
        self.state_store = Some(store);
        self
    }

    /// Bind a plan already durably reviewed by another workflow seam (the
    /// enrollment draft).  This is crate-private deliberately: external API
    /// callers must use a persisted probe plan and cannot inject a caller-made
    /// approval into `apply`.
    pub(crate) fn with_reviewed_plan(mut self, plan: DeploymentPlan) -> Self {
        self.reviewed_plan = Some(plan);
        self
    }

    pub fn executor(&self) -> &E {
        &self.executor
    }

    pub fn executor_mut(&mut self) -> &mut E {
        &mut self.executor
    }
}

impl<E: RemoteExecutor> DeploymentRunner<E> {
    fn require_trusted_artifact(&self, artifact: &VerifiedArtifact) -> Result<()> {
        if !self.publisher_policy.accepts_evidence(&artifact.evidence()) {
            bail!("verified artifact is not anchored to the deployment publisher policy");
        }
        Ok(())
    }

    /// Run the concrete, read-only remote probe and persist the resulting
    /// secret-free plan.  Planning never uploads, installs, activates, or
    /// changes managed host-key trust.
    pub fn probe(
        &mut self,
        request: &DeploymentRequest,
        artifact: Option<&VerifiedArtifact>,
    ) -> Result<DeploymentPlan> {
        request.listener.validate()?;
        if let Some(artifact) = artifact {
            self.require_trusted_artifact(artifact)?;
            if artifact.manifest().manifest.release != request.release {
                bail!("verified artifact release does not match the deployment request");
            }
        }
        let facts = self
            .executor
            .detect()
            .context("remote platform detection failed")?;
        if let Some(artifact) = artifact {
            if artifact.descriptor().platform != facts.platform {
                bail!("verified artifact target does not match the detected remote platform");
            }
        }
        let plan = DeploymentPlan::for_facts_with_details(
            request.target.clone(),
            request.release.clone(),
            &facts,
            request.listener.clone(),
            DeploymentPlanDetails {
                artifact: artifact.map(VerifiedArtifact::evidence),
                database_seed: request.database_seed.is_some(),
                seed_bytes: request.database_seed.clone(),
                ssh_target: self.executor.canonical_target().cloned(),
            },
        )?;
        if let Some(store) = &self.state_store {
            store.save_plan(&plan)?;
            store.save(&DeploymentCheckpoint {
                target: plan.target.clone(),
                release: plan.release.clone(),
                plan_hash: plan.plan_hash.clone(),
                status: "probed".to_string(),
                tailscale_state: plan.listener.tailscale_state,
                receipt: None,
            })?;
        }
        Ok(plan)
    }

    /// Apply only a persisted plan whose hash was explicitly approved by the
    /// operator.  The facts and all derived contents are recomputed immediately
    /// before the first remote mutation; stale plans never reach `run`/`upload`.
    pub fn apply(
        &mut self,
        request: &DeploymentRequest,
        artifact: &VerifiedArtifact,
    ) -> Result<DeploymentReceipt> {
        let approved = request
            .approved_plan_hash
            .as_deref()
            .filter(|hash| !hash.is_empty())
            .context("an approved persisted plan hash is required before apply")?;
        let reviewed = if let Some(store) = &self.state_store {
            store.load_plan()?.context(
                "no persisted deployment plan is available; run the planning probe first",
            )?
        } else {
            self.reviewed_plan
                .clone()
                .context("deployment apply requires a persisted reviewed plan")?
        };
        if reviewed.plan_hash != approved {
            bail!("approved plan hash does not match the persisted deployment plan");
        }
        reviewed.verify_hash()?;
        self.require_trusted_artifact(artifact)?;
        if artifact.manifest().manifest.release != request.release {
            bail!("verified artifact release does not match the deployment request");
        }
        let facts = self
            .executor
            .detect()
            .context("remote platform detection failed")?;
        if artifact.descriptor().platform != facts.platform {
            bail!("verified artifact target does not match the detected remote platform");
        }
        let plan = DeploymentPlan::for_facts_with_details(
            request.target.clone(),
            request.release.clone(),
            &facts,
            request.listener.clone(),
            DeploymentPlanDetails {
                artifact: Some(artifact.evidence()),
                database_seed: request.database_seed.is_some(),
                seed_bytes: request.database_seed.clone(),
                ssh_target: self.executor.canonical_target().cloned(),
            },
        )?;
        if plan.plan_hash != reviewed.plan_hash || plan.plan_hash != approved {
            bail!("deployment facts or artifact changed after review; refusing to execute stale approval");
        }
        if let Some(store) = &self.state_store {
            store.mark_reviewed(&plan)?;
        }

        let paths = plan
            .paths
            .clone()
            .context("deployment plan has no remote paths")?;
        let service_spec = service_spec(&facts, &paths, &plan.listener)?;
        let rendered_services = service_spec.render()?;
        let mut mutated = false;
        let mut snapshot_complete = false;
        let result = (|| -> Result<DeploymentReceipt> {
            // Snapshot is the first remote mutation.  It must observe the
            // host before path preparation, artifact upload, service changes,
            // or database replacement can alter anything being restored.
            require_success(
                self.executor.run(RemoteAction::SnapshotRollbackState {
                    paths: paths.clone(),
                    platform: facts.platform.service_platform(),
                    listener: plan.listener.clone(),
                }),
                "snapshot remote rollback state",
            )?;
            snapshot_complete = true;
            mutated = true;
            require_success(
                self.executor.run(RemoteAction::PreparePaths {
                    paths: paths.clone(),
                }),
                "prepare remote paths",
            )?;
            require_success(
                self.executor.upload(
                    &format!("{}/dirtydash", paths.release_dir),
                    artifact.bytes(),
                    0o755,
                ),
                "upload verified artifact",
            )?;
            require_success(
                self.executor.run(RemoteAction::QuiesceServices {
                    platform: facts.platform.service_platform(),
                }),
                "quiesce services before replacement",
            )?;
            if let Some(seed) = &request.database_seed {
                let seed_path = format!("{}/dirtydash.sqlite3.seed", paths.data_dir);
                require_success(
                    self.executor.upload(&seed_path, seed, 0o600),
                    "upload database seed",
                )?;
                require_success(
                    self.executor.run(RemoteAction::InstallDatabaseSeed {
                        seed_path,
                        database_path: paths.hub_db.clone(),
                        backup_path: paths.hub_db_backup.clone(),
                        wal_backup_path: format!("{}-wal.previous", paths.hub_db_backup),
                        shm_backup_path: format!("{}-shm.previous", paths.hub_db_backup),
                    }),
                    "install database seed",
                )?;
            }
            let runtime_config = plan.listener.render_runtime_toml()?;
            require_success(
                self.executor.run(RemoteAction::InstallRuntimeConfig {
                    config_path: paths.config_file.clone(),
                    config: runtime_config,
                }),
                "install runtime configuration",
            )?;
            for service in rendered_services {
                require_success(
                    self.executor.run(RemoteAction::InstallService {
                        path: service.path.display().to_string(),
                        contents: service.contents,
                        mode: service.mode,
                    }),
                    "install service definition",
                )?;
            }
            require_success(
                self.executor.run(RemoteAction::AtomicallyActivate {
                    current: paths.current.clone(),
                    release: paths.release_dir.clone(),
                    platform: facts.platform.service_platform(),
                }),
                "activate release",
            )?;
            require_success(
                self.executor.run(RemoteAction::RestartServices {
                    platform: facts.platform.service_platform(),
                }),
                "restart services",
            )?;
            let health = require_success(
                self.executor.run(RemoteAction::HealthCheck {
                    port: plan.listener.local_port,
                    platform: facts.platform.service_platform(),
                }),
                "Hub health check",
            )?;
            if health.status != RemoteStatus::Success {
                bail!("Hub health check did not complete");
            }
            let mut listener = plan.listener.clone();
            let tailscale_state = if listener.access_mode == ListenerAccessMode::TailscaleServe {
                let tailscale = self.executor.run(RemoteAction::ConfigureTailscale {
                    port: listener.local_port,
                })?;
                if tailscale.status == RemoteStatus::ConsentRequired {
                    listener.apply_tailscale_output(&format!(
                        "{} {}",
                        tailscale.stdout, tailscale.stderr
                    ));
                    self.save_checkpoint(&DeploymentCheckpoint {
                        target: plan.target.clone(),
                        release: plan.release.clone(),
                        plan_hash: plan.plan_hash.clone(),
                        status: "consent-required".to_string(),
                        tailscale_state: listener.tailscale_state,
                        receipt: None,
                    })?;
                } else {
                    listener.tailscale_state = TailscaleServeState::Enabled;
                }
                listener.tailscale_state
            } else {
                TailscaleServeState::NotConfigured
            };
            require_success(
                self.executor.run(RemoteAction::VerifyReceipt {
                    release: plan.release.clone(),
                    port: plan.listener.local_port,
                    platform: facts.platform.service_platform(),
                }),
                "verify deployment receipt",
            )?;
            require_success(
                self.executor.run(RemoteAction::Cleanup {
                    release: paths.release_dir.clone(),
                    remove_release: false,
                    database_backup: request
                        .database_seed
                        .as_ref()
                        .map(|_| paths.hub_db_backup.clone()),
                    database_wal_backup: request
                        .database_seed
                        .as_ref()
                        .map(|_| format!("{}-wal.previous", paths.hub_db_backup)),
                    database_shm_backup: request
                        .database_seed
                        .as_ref()
                        .map(|_| format!("{}-shm.previous", paths.hub_db_backup)),
                    temporary_seed: request
                        .database_seed
                        .as_ref()
                        .map(|_| format!("{}/dirtydash.sqlite3.seed", paths.data_dir)),
                    rollback_snapshot: plan.rollback.rollback_snapshot_dir.clone(),
                }),
                "cleanup deployment temporary files",
            )?;
            let receipt = DeploymentReceipt {
                target: plan.target.clone(),
                release: plan.release.clone(),
                platform: facts.platform,
                plan_hash: plan.plan_hash.clone(),
                artifact_sha256: artifact.descriptor().sha256.clone(),
                artifact_size: artifact.descriptor().size,
                publisher_key_id: artifact.manifest().key_id().to_string(),
                hub_health_verified: true,
                collector_service_verified: true,
                database_seeded: request.database_seed.is_some(),
                tailscale_state,
                status: if tailscale_state == TailscaleServeState::ConsentRequired {
                    "consent-required".to_string()
                } else {
                    "complete".to_string()
                },
                cleanup_performed: true,
            };
            self.save_checkpoint(&DeploymentCheckpoint {
                target: plan.target.clone(),
                release: plan.release.clone(),
                plan_hash: plan.plan_hash.clone(),
                status: receipt.status.clone(),
                tailscale_state,
                receipt: Some(receipt.clone()),
            })?;
            Ok(receipt)
        })();

        match result {
            Ok(receipt) => Ok(receipt),
            Err(error) => {
                let mut cleanup_error = None;
                let mut rollback_succeeded = !mutated;
                let mut rollback_failed = false;
                if mutated {
                    if let Err(_rollback) = self.executor.run(RemoteAction::Rollback {
                        current: paths.current.clone(),
                        previous: plan.rollback.previous_release.clone(),
                        database_path: Some(paths.hub_db.clone()),
                        database_backup: request
                            .database_seed
                            .as_ref()
                            .map(|_| paths.hub_db_backup.clone()),
                        database_wal_backup: request
                            .database_seed
                            .as_ref()
                            .map(|_| format!("{}-wal.previous", paths.hub_db_backup)),
                        database_shm_backup: request
                            .database_seed
                            .as_ref()
                            .map(|_| format!("{}-shm.previous", paths.hub_db_backup)),
                        config_path: Some(paths.config_file.clone()),
                        service_dir: Some(paths.service_dir.clone()),
                        platform: facts.platform.service_platform(),
                        listener: Some(previous_listener_state(&plan)),
                        snapshot_dir: plan.rollback.rollback_snapshot_dir.clone(),
                    }) {
                        rollback_failed = true;
                        cleanup_error = Some("rollback operation failed".to_string());
                    } else {
                        rollback_succeeded = true;
                    }
                }
                if mutated {
                    if let Err(cleanup) = self.executor.run(RemoteAction::Cleanup {
                        release: paths.release_dir,
                        remove_release: true,
                        database_backup: request
                            .database_seed
                            .as_ref()
                            .filter(|_| rollback_succeeded)
                            .map(|_| paths.hub_db_backup.clone()),
                        database_wal_backup: request
                            .database_seed
                            .as_ref()
                            .filter(|_| rollback_succeeded)
                            .map(|_| format!("{}-wal.previous", paths.hub_db_backup)),
                        database_shm_backup: request
                            .database_seed
                            .as_ref()
                            .filter(|_| rollback_succeeded)
                            .map(|_| format!("{}-shm.previous", paths.hub_db_backup)),
                        temporary_seed: request
                            .database_seed
                            .as_ref()
                            .map(|_| format!("{}/dirtydash.sqlite3.seed", paths.data_dir)),
                        rollback_snapshot: (snapshot_complete && !rollback_failed)
                            .then(|| plan.rollback.rollback_snapshot_dir.clone())
                            .flatten(),
                    }) {
                        let _ = cleanup;
                        cleanup_error = Some("cleanup operation failed".to_string());
                    }
                }
                if rollback_failed {
                    let _ = self.save_checkpoint(&DeploymentCheckpoint {
                        target: plan.target.clone(),
                        release: plan.release.clone(),
                        plan_hash: plan.plan_hash.clone(),
                        status: "manual-recovery-required".to_string(),
                        tailscale_state: plan.listener.tailscale_state,
                        receipt: None,
                    });
                }
                let message = if rollback_failed {
                    "manual recovery required: deployment rollback failed; the retained rollback snapshot must be restored by an operator".to_string()
                } else {
                    match cleanup_error {
                        Some(cleanup) => {
                            format!("deployment failed; rollback/cleanup also failed: {cleanup}")
                        }
                        None => "deployment failed; remote state was rolled back and cleaned"
                            .to_string(),
                    }
                };
                // Do not copy remote stderr or arbitrary executor errors into
                // the caller-facing diagnostic.  The executor has already
                // emitted only a bounded status; this second barrier prevents
                // hostile remote output from becoming persisted/logged text.
                let _ = error;
                Err(anyhow::anyhow!(message))
            }
        }
    }

    fn save_checkpoint(&self, checkpoint: &DeploymentCheckpoint) -> Result<()> {
        if let Some(store) = &self.state_store {
            store.save(checkpoint)?;
        }
        Ok(())
    }
}

fn require_success(result: Result<RemoteResult>, operation: &str) -> Result<RemoteResult> {
    let result = result.map_err(|_| anyhow::anyhow!(format!("{operation} failed")))?;
    if result.status != RemoteStatus::Success {
        bail!("{operation} did not complete");
    }
    Ok(result)
}

fn service_spec(
    facts: &RemoteFacts,
    paths: &DeploymentPaths,
    listener: &ListenerPlan,
) -> Result<ServiceSpec> {
    Ok(ServiceSpec {
        platform: facts.platform.service_platform(),
        user: facts.user.clone(),
        uid: facts.uid,
        executable: PathBuf::from(format!("{}/dirtydash", paths.current)),
        config_path: PathBuf::from(&paths.config_file),
        usage_db_path: PathBuf::from(&paths.hub_db),
        collector_db_path: PathBuf::from(&paths.collector_db),
        hub_db_path: PathBuf::from(&paths.hub_db),
        hub_port: listener.local_port,
        listener: match listener.access_mode {
            ListenerAccessMode::TailscaleServe => "tailscale".to_string(),
            ListenerAccessMode::PublicHttps => "public".to_string(),
        },
        service_dir: PathBuf::from(&paths.service_dir),
    })
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().context("atomic file has no parent")?;
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
    set_mode(&temp, mode)?;
    fs::rename(&temp, path)?;
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

fn redact_error(value: &str, secrets: &[&str]) -> String {
    let mut redacted = value.to_string();
    for secret in secrets {
        if !secret.is_empty() {
            redacted = redacted.replace(secret, "[REDACTED]");
        }
    }
    if redacted.len() > 400 {
        redacted.truncate(400);
        redacted.push('…');
    }
    redacted
}

const MAX_LIVE_SECRET_BYTES: usize = 4096;
const MAX_LIVE_OUTPUT_BYTES: usize = 16 * 1024;
const OPTIONAL_KEY_PROMPT_GRACE: Duration = Duration::from_millis(750);
const MAX_LIVE_OPERATION_DURATION: Duration = Duration::from_secs(60);

/// Short-lived credentials for one live SSH connection.  This type is not
/// serializable or printable.  Its only consumer is the prompt-aware stdin
/// writer below; it never becomes an argument, environment value, checkpoint,
/// or transcript.
#[derive(Clone)]
pub struct SshLiveSecrets {
    password: Option<Zeroizing<Vec<u8>>>,
    key_passphrase: Option<Zeroizing<Vec<u8>>>,
    sudo_password: Option<Zeroizing<Vec<u8>>>,
}

impl SshLiveSecrets {
    pub fn new(
        password: Option<&[u8]>,
        key_passphrase: Option<&[u8]>,
        sudo_password: Option<&[u8]>,
    ) -> Result<Self> {
        for (label, value) in [
            ("SSH password", password),
            ("SSH key passphrase", key_passphrase),
            ("sudo password", sudo_password),
        ] {
            if value.is_some_and(|value| value.len() > MAX_LIVE_SECRET_BYTES) {
                bail!("{label} exceeds the bounded live-input size");
            }
        }
        Ok(Self {
            password: password.map(|value| Zeroizing::new(value.to_vec())),
            key_passphrase: key_passphrase.map(|value| Zeroizing::new(value.to_vec())),
            sudo_password: sudo_password.map(|value| Zeroizing::new(value.to_vec())),
        })
    }

    fn redaction_values(&self) -> Vec<&str> {
        [
            self.password.as_deref(),
            self.key_passphrase.as_deref(),
            self.sudo_password.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter_map(|value| std::str::from_utf8(value).ok())
        .filter(|value| !value.is_empty())
        .collect()
    }
}

impl fmt::Debug for SshLiveSecrets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SshLiveSecrets([REDACTED])")
    }
}

#[derive(Clone, Copy)]
enum LivePromptKind {
    SshPassword,
    KeyPassphrase,
}

struct LivePrompt<'a> {
    kind: LivePromptKind,
    secret: Option<&'a [u8]>,
    sent: bool,
    required: bool,
}

impl LivePrompt<'_> {
    fn matches(&self, output: &str) -> bool {
        let lower = output.to_ascii_lowercase();
        match self.kind {
            LivePromptKind::SshPassword => lower.contains("password:") && !lower.contains("sudo"),
            LivePromptKind::KeyPassphrase => {
                lower.contains("passphrase") && lower.contains("for key")
            }
        }
    }
}

struct LiveChunk {
    bytes: Zeroizing<Vec<u8>>,
}

fn spawn_live_reader<R: Read + Send + 'static>(mut reader: R, sender: mpsc::Sender<LiveChunk>) {
    thread::spawn(move || {
        let mut bytes = [0_u8; 1024];
        loop {
            match reader.read(&mut bytes) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let _ = sender.send(LiveChunk {
                        bytes: Zeroizing::new(bytes[..read].to_vec()),
                    });
                }
            }
        }
    });
}

fn append_live_output(output: &mut Zeroizing<Vec<u8>>, bytes: &[u8]) {
    let remaining = MAX_LIVE_OUTPUT_BYTES.saturating_sub(output.len());
    output.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
}

fn write_live_secret(stdin: &mut impl Write, secret: &[u8]) -> Result<()> {
    if secret.len() > MAX_LIVE_SECRET_BYTES {
        bail!("live input exceeds the bounded secret size");
    }
    stdin.write_all(secret)?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

fn redacted_live_output(output: &[u8], secrets: &SshLiveSecrets) -> String {
    let raw = Zeroizing::new(String::from_utf8_lossy(output).into_owned());
    let values = secrets.redaction_values();
    redact_error(&raw, &values)
}

#[cfg(unix)]
fn set_pty_raw(master: &dyn MasterPty) -> Result<()> {
    let fd = master
        .as_raw_fd()
        .context("controlled SSH PTY has no file descriptor")?;
    let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: tcgetattr initializes the termios structure before it is read.
    if unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) } != 0 {
        bail!("reading controlled SSH PTY attributes failed");
    }
    // SAFETY: tcgetattr succeeded above.
    let mut termios = unsafe { termios.assume_init() };
    termios.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON | libc::ISIG | libc::IEXTEN);
    termios.c_iflag &= !(libc::ICRNL | libc::INLCR | libc::IGNCR);
    termios.c_oflag &= !libc::OPOST;
    // SAFETY: termios was populated by tcgetattr and fd remains owned by the
    // PTY pair for the duration of this function.
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) } != 0 {
        bail!("configuring controlled SSH PTY failed");
    }
    Ok(())
}

/// The authentication leg uses a controlled PTY so OpenSSH can perform its
/// normal password/passphrase exchange when the caller is not itself a
/// terminal.  It is used only to establish a short-lived ControlMaster;
/// artifact, database, and other bytes never enter this function.
fn run_live_process(
    program: &Path,
    args: &[String],
    secrets: &SshLiveSecrets,
    require_password_prompt: bool,
) -> Result<RemoteResult> {
    let mut prompts = vec![LivePrompt {
        kind: LivePromptKind::SshPassword,
        secret: secrets.password.as_ref().map(|value| value.as_slice()),
        sent: false,
        required: require_password_prompt,
    }];
    if secrets.key_passphrase.is_some() {
        prompts.push(LivePrompt {
            kind: LivePromptKind::KeyPassphrase,
            secret: secrets
                .key_passphrase
                .as_ref()
                .map(|value| value.as_slice()),
            sent: false,
            required: false,
        });
    }
    for prompt in &prompts {
        if prompt.required && prompt.secret.is_none() {
            bail!("required live SSH prompt has no in-memory secret");
        }
    }

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 40,
            cols: 160,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("opening controlled SSH PTY")?;
    let mut builder = CommandBuilder::new(program);
    builder.args(args);
    let mut child = pair
        .slave
        .spawn_command(builder)
        .context("starting live SSH PTY connection")?;
    drop(pair.slave);
    #[cfg(unix)]
    set_pty_raw(pair.master.as_ref())?;
    let reader = pair
        .master
        .try_clone_reader()
        .context("opening live SSH PTY reader")?;
    let writer = pair
        .master
        .take_writer()
        .context("opening live SSH PTY writer")?;
    let (sender, receiver) = mpsc::channel();
    spawn_live_reader(reader, sender);
    let mut writer: Option<Box<dyn Write + Send>> = Some(writer);
    let mut output = Zeroizing::new(Vec::new());
    let mut prompt_bytes = Zeroizing::new(Vec::new());
    let started = SystemTime::now();

    loop {
        if started.elapsed().unwrap_or_default() > MAX_LIVE_OPERATION_DURATION {
            writer.take();
            let _ = child.kill();
            let _ = child.wait();
            bail!("live SSH prompt/operation exceeded its bounded duration");
        }
        if let Some(_status) = child.try_wait().context("polling live SSH PTY")? {
            while let Ok(chunk) = receiver.try_recv() {
                append_live_output(&mut output, &chunk.bytes);
                append_live_output(&mut prompt_bytes, &chunk.bytes);
            }
            break;
        }
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(chunk) => {
                append_live_output(&mut output, &chunk.bytes);
                append_live_output(&mut prompt_bytes, &chunk.bytes);
                let prompt_text =
                    Zeroizing::new(String::from_utf8_lossy(&prompt_bytes).into_owned());
                for prompt in &mut prompts {
                    if !prompt.sent && prompt.matches(&prompt_text) {
                        let secret = prompt
                            .secret
                            .context("live SSH prompt appeared without a supplied secret")?;
                        let live_writer = writer
                            .as_mut()
                            .context("live SSH PTY closed before authentication")?;
                        write_live_secret(live_writer, secret)?;
                        prompt.sent = true;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                writer.take();
                break;
            }
        }
        let required_sent = prompts
            .iter()
            .filter(|prompt| prompt.required)
            .all(|prompt| prompt.sent);
        let optional_key_waiting = prompts
            .iter()
            .any(|prompt| matches!(prompt.kind, LivePromptKind::KeyPassphrase) && !prompt.sent);
        let grace_elapsed = started.elapsed().unwrap_or_default() >= OPTIONAL_KEY_PROMPT_GRACE;
        if required_sent && (!optional_key_waiting || grace_elapsed) {
            // Closing the authentication writer tells a non-interactive SSH
            // process that all classified credentials have been delivered;
            // no caller payload is ever written to this PTY.
            writer.take();
        }
    }
    writer.take();
    let status = child.wait().context("waiting for live SSH PTY")?;
    let stdout = redacted_live_output(&output, secrets);
    if !status.success() {
        bail!(
            "live SSH operation failed: {}",
            redact_error(&stdout, &secrets.redaction_values())
        );
    }
    Ok(RemoteResult {
        status: RemoteStatus::Success,
        stdout,
        stderr: String::new(),
    })
}

fn run_binary_safe_process(
    program: &Path,
    args: &[String],
    input: Option<&[u8]>,
    secrets: &SshLiveSecrets,
) -> Result<RemoteResult> {
    let mut process = Command::new(program);
    process
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = process
        .spawn()
        .context("starting binary-safe authenticated SSH operation")?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(input)?;
    }
    let output = child
        .wait_with_output()
        .context("waiting for binary-safe authenticated SSH operation")?;
    let stdout = redacted_live_output(&output.stdout, secrets);
    let stderr = redacted_live_output(&output.stderr, secrets);
    if !output.status.success() {
        bail!(
            "authenticated SSH operation failed: {}",
            redact_error(&format!("{stdout} {stderr}"), &secrets.redaction_values())
        );
    }
    Ok(RemoteResult {
        status: RemoteStatus::Success,
        stdout,
        stderr,
    })
}

fn control_path() -> Result<(PathBuf, PathBuf)> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let directory =
        std::env::temp_dir().join(format!("dirtydash-ssh-{}-{nonce}", std::process::id()));
    fs::create_dir(&directory).context("creating private SSH ControlMaster directory")?;
    set_mode(&directory, 0o700)?;
    let socket = directory.join("control");
    Ok((directory, socket))
}

/// Production SSH adapter.  Its command line is fixed and carries no
/// password, passphrase, sudo secret, or signing key.  Password/key prompts
/// are handled only while establishing a private, short-lived ControlMaster;
/// artifact and seed bytes then travel through a separate pipe over that
/// authenticated socket, never through a PTY, argument, environment variable,
/// temporary local file, or diagnostic.
#[derive(Debug, Clone)]
pub struct SshRemoteExecutor {
    target: CanonicalSshTarget,
    known_hosts: PathBuf,
    key_path: Option<PathBuf>,
    ssh_program: PathBuf,
    live_secrets: Option<SshLiveSecrets>,
}

impl SshRemoteExecutor {
    pub fn new(target: impl Into<String>, known_hosts: impl Into<PathBuf>) -> Result<Self> {
        let target = CanonicalSshTarget::resolve(target.into())?;
        Self::from_canonical_target(target, known_hosts)
    }

    pub fn from_canonical_target(
        target: CanonicalSshTarget,
        known_hosts: impl Into<PathBuf>,
    ) -> Result<Self> {
        let known_hosts = known_hosts.into();
        if let Some(parent) = known_hosts.parent() {
            fs::create_dir_all(parent)?;
        }
        if !known_hosts.exists() {
            atomic_write(&known_hosts, b"", 0o600)?;
        }
        let index_path = known_hosts.with_extension("fingerprints.json");
        let trusted = if index_path.exists() {
            let bytes = fs::read(&index_path)?;
            let records: std::collections::BTreeMap<String, serde_json::Value> =
                serde_json::from_slice(&bytes).context("parsing managed SSH host-key index")?;
            records.contains_key(&target.host_key_name())
        } else {
            false
        };
        if !trusted {
            bail!("SSH target is not explicitly confirmed in the managed known-host store");
        }
        Ok(Self {
            target,
            known_hosts,
            key_path: None,
            ssh_program: PathBuf::from("ssh"),
            live_secrets: None,
        })
    }

    /// Select a test-only SSH binary without changing the production default.
    /// This remains crate-private so the public adapter cannot be redirected
    /// by an untrusted caller at runtime.
    pub(crate) fn with_ssh_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.ssh_program = program.into();
        self
    }

    /// Attach bounded, zeroized credentials for a live password/key/sudo
    /// connection.  They are consumed only by classified prompt handling and
    /// never become process arguments or environment values.
    pub fn with_live_secrets(mut self, secrets: SshLiveSecrets) -> Self {
        self.live_secrets = Some(secrets);
        self
    }

    pub fn with_key_path(mut self, key_path: impl Into<PathBuf>) -> Result<Self> {
        let key_path = key_path.into();
        if key_path.as_os_str().is_empty()
            || key_path
                .to_string_lossy()
                .chars()
                .any(|character| character.is_control())
        {
            bail!("SSH key path is invalid");
        }
        self.key_path = Some(key_path);
        Ok(self)
    }

    fn base_args(&self) -> Vec<String> {
        let mut args = self.target.ssh_args(
            &self.known_hosts,
            self.key_path.as_deref(),
            self.live_secrets.is_none(),
        );
        if self.live_secrets.is_some() && self.key_path.is_none() {
            args.extend([
                "-o".to_string(),
                "PreferredAuthentications=password".to_string(),
                "-o".to_string(),
                "PubkeyAuthentication=no".to_string(),
            ]);
        }
        args
    }

    fn invocation(&self, command: &str) -> Result<std::process::Output> {
        let mut process = Command::new(&self.ssh_program);
        process.args(self.base_args()).arg(command);
        process
            .output()
            .context("running fixed-allowlist SSH operation")
    }

    fn control_args(&self, socket: &Path) -> Vec<String> {
        let mut args = self
            .target
            .ssh_args(&self.known_hosts, self.key_path.as_deref(), true);
        let destination = args.pop().expect("ssh_args always contains a destination");
        args.extend([
            "-S".to_string(),
            socket.display().to_string(),
            "-o".to_string(),
            "ControlMaster=no".to_string(),
            "-o".to_string(),
            format!("ControlPath={}", socket.display()),
            "-T".to_string(),
            "-o".to_string(),
            "RequestTTY=no".to_string(),
            destination,
        ]);
        args
    }

    fn close_control_master(&self, socket: &Path) -> Result<()> {
        let mut args = self
            .target
            .ssh_args(&self.known_hosts, self.key_path.as_deref(), true);
        let destination = args.pop().expect("ssh_args always contains a destination");
        args.extend([
            "-S".to_string(),
            socket.display().to_string(),
            "-O".to_string(),
            "exit".to_string(),
            destination,
        ]);
        let output = Command::new(&self.ssh_program)
            .args(args)
            .output()
            .context("closing SSH ControlMaster")?;
        if !output.status.success() && socket.exists() {
            bail!("SSH ControlMaster did not close cleanly");
        }
        Ok(())
    }

    fn with_authenticated_control<F>(&self, operation: F) -> Result<RemoteResult>
    where
        F: FnOnce(&Path, &SshLiveSecrets) -> Result<RemoteResult>,
    {
        let secrets = self
            .live_secrets
            .as_ref()
            .context("live SSH credentials were not configured")?;
        let (directory, socket) = control_path()?;
        let mut master_args = self.base_args();
        let destination = master_args
            .pop()
            .expect("ssh_args always contains a destination");
        master_args.extend([
            "-M".to_string(),
            "-S".to_string(),
            socket.display().to_string(),
            "-o".to_string(),
            "ControlMaster=yes".to_string(),
            "-o".to_string(),
            "ControlPersist=60".to_string(),
            "-f".to_string(),
            "-N".to_string(),
            destination,
        ]);
        let authenticated = run_live_process(
            &self.ssh_program,
            &master_args,
            secrets,
            self.key_path.is_none(),
        );
        if let Err(error) = authenticated {
            let _ = fs::remove_dir_all(&directory);
            return Err(error);
        }
        let operation_result = operation(&socket, secrets);
        let close_result = self.close_control_master(&socket);
        let _ = fs::remove_dir_all(&directory);
        match (operation_result, close_result) {
            (Err(error), _) => Err(error),
            (Ok(_result), Err(error)) => Err(error),
            (Ok(result), Ok(())) => Ok(result),
        }
    }

    pub(crate) fn live_command(&mut self, command: &str, sudo: bool) -> Result<RemoteResult> {
        let mut sudo_input = None;
        if sudo {
            let secrets = self
                .live_secrets
                .as_ref()
                .context("live SSH credentials were not configured")?;
            let password = secrets
                .sudo_password
                .as_ref()
                .context("sudo operation requires an in-memory sudo password")?;
            let mut input = Zeroizing::new(password.to_vec());
            input.push(b'\n');
            sudo_input = Some(input);
        }
        self.with_authenticated_control(|socket, secrets| {
            let mut args = self.control_args(socket);
            args.push(command.to_string());
            run_binary_safe_process(
                &self.ssh_program,
                &args,
                sudo_input.as_deref().map(|value| value.as_slice()),
                secrets,
            )
        })
    }
}

impl RemoteExecutor for SshRemoteExecutor {
    fn canonical_target(&self) -> Option<&CanonicalSshTarget> {
        Some(&self.target)
    }

    fn detect(&mut self) -> Result<RemoteFacts> {
        let command = "set -eu; printf 'os=%s\\n' \"$(uname -s)\"; printf 'arch=%s\\n' \"$(uname -m)\"; printf 'user=%s\\n' \"$(id -un)\"; printf 'uid=%s\\n' \"$(id -u)\"; printf 'home=%s\\n' \"$HOME\"; if [ -L \"$HOME/.local/share/dirtydash/current\" ]; then printf 'current=%s\\n' \"$(readlink \"$HOME/.local/share/dirtydash/current\")\"; fi";
        if self.live_secrets.is_some() {
            let result = self.live_command(command, false)?;
            return RemoteFacts::parse_probe(&result.stdout);
        }
        let output = self.invocation(command)?;
        if !output.status.success() {
            bail!(
                "remote platform probe failed with status {}",
                output.status.code().unwrap_or(-1)
            );
        }
        RemoteFacts::parse_probe(&String::from_utf8_lossy(&output.stdout))
    }

    fn run(&mut self, action: RemoteAction) -> Result<RemoteResult> {
        match &action {
            RemoteAction::InstallDatabaseSeed {
                seed_path: _,
                database_path: _,
                backup_path: _,
                wal_backup_path: _,
                shm_backup_path: _,
            } => {
                let command = action_command(&action)?;
                if self.live_secrets.is_some() {
                    return self.live_command(&command, false);
                }
                let output = self.invocation(&command)?;
                if !output.status.success() {
                    bail!(
                        "remote database seed install failed with status {}",
                        output.status.code().unwrap_or(-1)
                    );
                }
                return Ok(RemoteResult::success(redact_error(
                    &String::from_utf8_lossy(&output.stdout),
                    &[],
                )));
            }
            RemoteAction::InstallRuntimeConfig {
                config_path,
                config,
            } => {
                if config.contains("password")
                    || config.contains("token")
                    || config.contains("secret")
                {
                    bail!("runtime config contains a secret-bearing field");
                }
                return self.upload(config_path, config.as_bytes(), 0o600);
            }
            RemoteAction::InstallService {
                path,
                contents,
                mode,
            } => {
                if contents.contains("password")
                    || contents.contains("token")
                    || contents.contains("secret")
                {
                    bail!("service definition contains a secret-bearing field");
                }
                return self.upload(path, contents.as_bytes(), *mode);
            }
            _ => {}
        }
        let command = action_command(&action)?;
        if self.live_secrets.is_some() {
            return self.live_command(&command, false);
        }
        let output = self.invocation(&command)?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if matches!(action, RemoteAction::ConfigureTailscale { .. })
            && (!output.status.success()
                && (stderr.to_ascii_lowercase().contains("consent")
                    || stderr.to_ascii_lowercase().contains("approve")
                    || stderr.to_ascii_lowercase().contains("permission")))
        {
            return Ok(RemoteResult::consent_required(redact_error(
                &format!("{stdout} {stderr}"),
                &[],
            )));
        }
        if !output.status.success() {
            bail!(
                "remote operation failed with status {}",
                output.status.code().unwrap_or(-1)
            );
        }
        Ok(RemoteResult {
            status: RemoteStatus::Success,
            stdout: redact_error(&stdout, &[]),
            stderr: redact_error(&stderr, &[]),
        })
    }

    fn upload(&mut self, destination: &str, bytes: &[u8], mode: u32) -> Result<RemoteResult> {
        validate_remote_text(destination, "remote upload destination")?;
        if !matches!(mode, 0o600 | 0o644 | 0o755) {
            bail!("remote upload mode is not allowlisted");
        }
        let temp = format!("{destination}.tmp-{}", std::process::id());
        let input_command = if self.live_secrets.is_some() {
            format!(
                "dd bs=1 count={} 2>/dev/null > {}",
                bytes.len(),
                shell_quote(&temp)
            )
        } else {
            format!("cat > {}", shell_quote(&temp))
        };
        let command = format!(
            "set -eu; umask 077; {input_command}; chmod {:o} {}; mv -f {} {}",
            mode,
            shell_quote(&temp),
            shell_quote(&temp),
            shell_quote(destination),
            input_command = input_command,
        );
        if self.live_secrets.is_some() {
            return self.with_authenticated_control(|socket, secrets| {
                let mut args = self.control_args(socket);
                args.push(command.clone());
                run_binary_safe_process(&self.ssh_program, &args, Some(bytes), secrets)
            });
        }
        let mut process = Command::new(&self.ssh_program);
        process
            .args(self.base_args())
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = process.spawn().context("starting SSH upload")?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(bytes)?;
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!(
                "remote upload failed with status {}",
                output.status.code().unwrap_or(-1)
            );
        }
        Ok(RemoteResult::success(redact_error(
            &String::from_utf8_lossy(&output.stdout),
            &[],
        )))
    }
}

fn sqlite_header_command(path: &str) -> String {
    let path = shell_quote(path);
    format!(
        "if command -v python3 >/dev/null 2>&1; then python3 - {path} <<'PY'\nimport sys\nwith open(sys.argv[1], 'rb') as handle:\n    if handle.read(16) != bytes.fromhex('{SQLITE_HEADER_HEX}'):\n        raise SystemExit(1)\nPY\nelif command -v od >/dev/null 2>&1; then actual=$(dd if={path} bs=1 count=16 2>/dev/null | od -An -v -t x1 | tr -d '[:space:]'); test \"$actual\" = {SQLITE_HEADER_HEX}; else echo 'no portable SQLite header validator' >&2; exit 1; fi",
    )
}

const LAUNCHD_RUNNING_PATTERN: &str = r"(^|[[:space:]])state = running([[:space:]]|$)|(^|[[:space:]])pid = [1-9][0-9]*([[:space:]]|$)";
const SYSTEMD_STOP_IF_PRESENT_COMMAND: &str =
    "state=$(systemctl --user show \"$service\" --property=LoadState --no-pager 2>/dev/null); case \"$state\" in *LoadState=not-found*) :;; *) systemctl --user stop \"$service\";; esac";

fn launchd_running_check(job: &str) -> String {
    format!(
        "launchctl print {job} 2>/dev/null | grep -Eq '{LAUNCHD_RUNNING_PATTERN}'",
        job = job,
    )
}

fn snapshot_command(
    paths: &DeploymentPaths,
    platform: ServicePlatform,
    listener: &ListenerPlan,
) -> Result<String> {
    let snapshot = format!("{}/deployment-rollback", paths.state_dir);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let snapshot_tmp = format!("{snapshot}.tmp-{}-{nonce}", std::process::id());
    let service_files = [
        format!("{}/dirtydash-hub.service", paths.service_dir),
        format!("{}/dirtydash-collector.service", paths.service_dir),
        format!("{}/dev.dirtydash.hub.plist", paths.service_dir),
        format!("{}/dev.dirtydash.collector.plist", paths.service_dir),
    ]
    .into_iter()
    .map(|path| shell_quote(&path))
    .collect::<Vec<_>>()
    .join(" ");
    let service_state = match platform {
        ServicePlatform::Systemd => {
            "for service in dirtydash-hub.service dirtydash-collector.service; do if state=$(systemctl --user show \"$service\" --property=LoadState --property=ActiveState --property=UnitFileState --no-pager 2>/dev/null); then printf '%s\\n' \"$state\" > \"$snapshot_tmp/$service.state\"; load=$(printf '%s\\n' \"$state\" | sed -n 's/^LoadState=//p' | head -n 1); active=$(printf '%s\\n' \"$state\" | sed -n 's/^ActiveState=//p' | head -n 1); unit=$(printf '%s\\n' \"$state\" | sed -n 's/^UnitFileState=//p' | head -n 1); case \"$load\" in loaded) printf 'loaded\\n' > \"$snapshot_tmp/$service.loaded\";; *) printf 'unloaded\\n' > \"$snapshot_tmp/$service.loaded\";; esac; case \"$active\" in active) printf 'active\\n' > \"$snapshot_tmp/$service.active\";; *) printf 'inactive\\n' > \"$snapshot_tmp/$service.active\";; esac; printf '%s\\n' \"$unit\" > \"$snapshot_tmp/$service.enabled\"; else printf 'unloaded\\n' > \"$snapshot_tmp/$service.loaded\"; printf 'inactive\\n' > \"$snapshot_tmp/$service.active\"; printf 'disabled\\n' > \"$snapshot_tmp/$service.enabled\"; printf 'LoadState=not-found\\nActiveState=inactive\\nUnitFileState=disabled\\n' > \"$snapshot_tmp/$service.state\"; fi; done".to_string()
        }
        ServicePlatform::Launchd => {
            format!(
                "domain=gui/$(id -u); for service in dev.dirtydash.hub dev.dirtydash.collector; do if state=$(launchctl print \"$domain/$service\" 2>/dev/null); then printf '%s\\n' \"$state\" > \"$snapshot_tmp/$service.state\"; printf 'loaded\\n' > \"$snapshot_tmp/$service.loaded\"; if printf '%s\\n' \"$state\" | grep -Eq '{LAUNCHD_RUNNING_PATTERN}'; then printf 'active\\n' > \"$snapshot_tmp/$service.active\"; else printf 'inactive\\n' > \"$snapshot_tmp/$service.active\"; fi; else printf 'unloaded\\n' > \"$snapshot_tmp/$service.loaded\"; printf 'inactive\\n' > \"$snapshot_tmp/$service.active\"; : > \"$snapshot_tmp/$service.state\"; fi; done"
            )
        }
    };
    Ok(format!(
        "set -eu; umask 077; snapshot={snapshot}; snapshot_tmp={snapshot_tmp}; if [ -e \"$snapshot\" ]; then echo 'existing rollback snapshot requires manual recovery' >&2; exit 125; fi; rm -rf -- \"$snapshot_tmp\"; trap 'rm -rf -- \"$snapshot_tmp\"' EXIT; mkdir -p \"$snapshot_tmp\"; if [ -e {release_dir} ] || [ -L {release_dir} ]; then echo 'target release already exists; refusing to overwrite it' >&2; exit 126; fi; for protected in {backup} {wal_backup} {shm_backup} {seed}; do if [ -e \"$protected\" ] || [ -L \"$protected\" ]; then echo 'deployment temporary path already exists' >&2; exit 127; fi; done; snapshot_copy() {{ source=$1; destination=$2; missing=$3; if [ -e \"$source\" ] || [ -L \"$source\" ]; then cp -p \"$source\" \"$snapshot_tmp/$destination\"; else : > \"$snapshot_tmp/$missing\"; fi; }}; if [ -L {current} ]; then readlink {current} > \"$snapshot_tmp/current.target\"; : > \"$snapshot_tmp/current.symlink\"; elif [ -e {current} ]; then if [ -d {current} ]; then echo 'current pointer is an unsupported directory' >&2; exit 128; fi; cp -p {current} \"$snapshot_tmp/current.file\"; : > \"$snapshot_tmp/current.regular\"; else : > \"$snapshot_tmp/current.missing\"; fi; snapshot_copy {config} config.toml .missing-config; snapshot_copy {database} database .missing-database; snapshot_copy {database}-wal database-wal .missing-database-wal; snapshot_copy {database}-shm database-shm .missing-database-shm; for service in {service_files}; do name=$(basename \"$service\"); snapshot_copy \"$service\" \"$name\" \".missing-$name\"; done; {service_state}; : > \"$snapshot_tmp/hub-port\"; for service in {service_files}; do if [ -f \"$service\" ]; then port=$(sed -nE 's/.*--port[ =]([0-9]+).*/\\1/p' \"$service\" | head -n 1); if [ -z \"$port\" ]; then port=$(grep -A1 '<string>--port</string>' \"$service\" 2>/dev/null | sed -n '2p' | sed -nE 's|.*<string>([0-9]+)</string>.*|\\1|p'); fi; if [ -n \"$port\" ]; then printf '%s\\n' \"$port\" > \"$snapshot_tmp/hub-port\"; break; fi; fi; done; printf '%s\\n' {requested_port} > \"$snapshot_tmp/requested-port\"; if command -v tailscale >/dev/null 2>&1; then if tailscale serve status --json > \"$snapshot_tmp/tailscale-serve.json\" 2>/dev/null; then tailscale serve status > \"$snapshot_tmp/tailscale-serve.txt\" 2>/dev/null || true; if [ -s \"$snapshot_tmp/tailscale-serve.json\" ] && ! grep -Eq '^[[:space:]]*(\\{{[[:space:]]*\\}}|null|\\[[[:space:]]*\\])' \"$snapshot_tmp/tailscale-serve.json\"; then printf 'enabled\\n' > \"$snapshot_tmp/listener-state\"; port=$(sed -nE 's/.*127\\.0\\.0\\.1:([0-9]+).*/\\1/p' \"$snapshot_tmp/tailscale-serve.txt\" | head -n 1); backend=$(sed -nE 's/.*proxy (https?:\\/\\/[^ ]+).*/\\1/p' \"$snapshot_tmp/tailscale-serve.txt\" | head -n 1); if [ -n \"$port\" ]; then printf '%s\\n' \"$port\" > \"$snapshot_tmp/listener-port\"; fi; if [ -n \"$backend\" ]; then printf '%s\\n' \"$backend\" > \"$snapshot_tmp/listener-backend\"; fi; else printf 'not-configured\\n' > \"$snapshot_tmp/listener-state\"; fi; else printf 'unavailable\\n' > \"$snapshot_tmp/listener-state\"; fi; else printf 'unavailable\\n' > \"$snapshot_tmp/listener-state\"; fi; mv -f \"$snapshot_tmp\" \"$snapshot\"; trap - EXIT",
        snapshot = shell_quote(&snapshot),
        snapshot_tmp = shell_quote(&snapshot_tmp),
        current = shell_quote(&paths.current),
        release_dir = shell_quote(&paths.release_dir),
        config = shell_quote(&paths.config_file),
        database = shell_quote(&paths.hub_db),
        backup = shell_quote(&paths.hub_db_backup),
        wal_backup = shell_quote(&format!("{}-wal.previous", paths.hub_db_backup)),
        shm_backup = shell_quote(&format!("{}-shm.previous", paths.hub_db_backup)),
        seed = shell_quote(&format!("{}/dirtydash.sqlite3.seed", paths.data_dir)),
        service_files = service_files,
        service_state = service_state,
        requested_port = listener.local_port,
    ))
}

fn snapshot_restore_command(
    snapshot: &str,
    config_path: Option<&str>,
    service_dir: Option<&str>,
    database_path: Option<&str>,
    platform: ServicePlatform,
) -> String {
    let snapshot = shell_quote(snapshot);
    let config = config_path
        .map(shell_quote)
        .unwrap_or_else(|| "\"$HOME/.config/dirtydash/config.toml\"".to_string());
    let service_dir = service_dir.map(shell_quote).unwrap_or_else(|| {
        match platform {
            ServicePlatform::Systemd => "\"$HOME/.config/dirtydash/systemd/user\"",
            ServicePlatform::Launchd => "\"$HOME/Library/LaunchAgents\"",
        }
        .to_string()
    });
    let config_parent = config_path
        .and_then(|path| Path::new(path).parent())
        .map(|path| shell_quote(&path.display().to_string()))
        .unwrap_or_else(|| "\"$HOME/.config/dirtydash\"".to_string());
    let service_restore = match platform {
        ServicePlatform::Systemd => {
            "for service in dirtydash-hub.service dirtydash-collector.service; do if [ -f $snapshot/$service ]; then cp -p $snapshot/$service $service_dir/$service; elif [ -f $snapshot/.missing-$service ]; then rm -f $service_dir/$service; fi; done;"
        }
        ServicePlatform::Launchd => {
            "for service in dev.dirtydash.hub.plist dev.dirtydash.collector.plist; do if [ -f $snapshot/$service ]; then cp -p $snapshot/$service $service_dir/$service; elif [ -f $snapshot/.missing-$service ]; then rm -f $service_dir/$service; fi; done;"
        }
    };
    let database_restore = database_path
        .map(|database| {
            let database = shell_quote(database);
            let parent = Path::new(database.trim_matches('\'')).parent();
            let parent = parent
                .map(|path| shell_quote(&path.display().to_string()))
                .unwrap_or_else(|| "\"$HOME/.local/state/dirtydash/data\"".to_string());
            format!(
                "mkdir -p {parent}; if [ -f {snapshot}/database ]; then cp -p {snapshot}/database {database}; elif [ -f {snapshot}/.missing-database ]; then rm -f {database}; fi; if [ -f {snapshot}/database-wal ]; then cp -p {snapshot}/database-wal {database}-wal; elif [ -f {snapshot}/.missing-database-wal ]; then rm -f {database}-wal; fi; if [ -f {snapshot}/database-shm ]; then cp -p {snapshot}/database-shm {database}-shm; elif [ -f {snapshot}/.missing-database-shm ]; then rm -f {database}-shm; fi; ",
                parent = parent,
                snapshot = snapshot,
                database = database,
            )
        })
        .unwrap_or_default();
    format!(
        "snapshot={snapshot}; service_dir={service_dir}; mkdir -p {config_parent} {service_dir}; if [ -f {snapshot}/config.toml ]; then cp -p {snapshot}/config.toml {config}; elif [ -f {snapshot}/.missing-config ]; then rm -f {config}; fi; {service_restore} {database_restore}",
        snapshot = snapshot,
        config_parent = config_parent,
        config = config,
        service_dir = service_dir,
        service_restore = service_restore,
        database_restore = database_restore,
    )
}

fn listener_snapshot_restore_command(snapshot: &str) -> String {
    let snapshot = shell_quote(snapshot);
    format!(
        "test -f {snapshot}/listener-state; if command -v tailscale >/dev/null 2>&1; then state=$(cat {snapshot}/listener-state); case \"$state\" in enabled) tailscale serve reset; if [ -s {snapshot}/listener-backend ]; then backend=$(cat {snapshot}/listener-backend); tailscale serve --https=443 \"$backend\"; else test -s {snapshot}/listener-port; port=$(cat {snapshot}/listener-port); case \"$port\" in ''|*[!0-9]*) exit 71;; esac; tailscale serve --https=443 http://127.0.0.1:\"$port\"; fi;; not-configured) tailscale serve reset;; unavailable) echo 'prior Tailscale listener state was not observable' >&2; exit 72;; *) echo 'prior Tailscale listener state is invalid' >&2; exit 73;; esac; else state=$(cat {snapshot}/listener-state); test \"$state\" = not-configured; fi; ",
    )
}

fn rollback_activation_command(
    current: &str,
    previous: Option<&str>,
    platform: ServicePlatform,
    snapshot: Option<&str>,
) -> String {
    let current_path = current.to_string();
    let current = shell_quote(current);
    let temp = format!("{current_path}.rollback-{}", std::process::id());
    let move_command = activation_move_command(&temp, &current_path, platform);
    let fallback = previous
        .map(|previous| format!("previous={}; ", shell_quote(previous)))
        .unwrap_or_else(|| "previous=; ".to_string());
    let snapshot_restore = snapshot
        .map(|snapshot| {
            let snapshot = shell_quote(snapshot);
            format!(
                "if [ -f {snapshot}/current.target ]; then previous=$(cat {snapshot}/current.target); if [ -n \"$previous\" ]; then ln -s \"$previous\" {temp}; {move_command}; else rm -f {current}; fi; elif [ -f {snapshot}/current.file ]; then rm -f {current}; cp -p {snapshot}/current.file {current}; elif [ -f {snapshot}/current.missing ]; then rm -f {current}; else {fallback}fi; ",
                snapshot = snapshot,
                temp = shell_quote(&temp),
                move_command = move_command,
                current = current,
                fallback = fallback,
            )
        })
        .unwrap_or(fallback);
    snapshot_restore
}

fn rollback_service_restart_command(
    platform: ServicePlatform,
    snapshot: Option<&str>,
    service_dir: Option<&str>,
) -> String {
    let Some(snapshot) = snapshot else {
        return service_restart_command(platform).to_string();
    };
    let snapshot = shell_quote(snapshot);
    let service_dir = service_dir.map(shell_quote).unwrap_or_else(|| {
        match platform {
            ServicePlatform::Systemd => "\"$HOME/.config/dirtydash/systemd/user\"",
            ServicePlatform::Launchd => "\"$HOME/Library/LaunchAgents\"",
        }
        .to_string()
    });
    match platform {
        ServicePlatform::Systemd => format!(
            "systemctl --user daemon-reload; for service in dirtydash-hub.service dirtydash-collector.service; do if grep -qx loaded {snapshot}/$service.loaded 2>/dev/null; then unit=$(cat {snapshot}/$service.enabled); case \"$unit\" in enabled*) systemctl --user enable \"$service\";; *) systemctl --user disable \"$service\" 2>/dev/null || true;; esac; if grep -qx active {snapshot}/$service.active 2>/dev/null; then systemctl --user start \"$service\"; systemctl --user is-active --quiet \"$service\"; else {systemd_stop_if_present}; if systemctl --user is-active --quiet \"$service\"; then exit 141; fi; fi; else {systemd_stop_if_present}; systemctl --user disable \"$service\" 2>/dev/null || true; systemctl --user reset-failed \"$service\" 2>/dev/null || true; if systemctl --user is-active --quiet \"$service\"; then exit 142; fi; fi; done",
            systemd_stop_if_present = SYSTEMD_STOP_IF_PRESENT_COMMAND,
        ),
        ServicePlatform::Launchd => format!(
            "domain=gui/$(id -u); for service in dev.dirtydash.hub dev.dirtydash.collector; do plist={service_dir}/$service.plist; job=\"$domain/$service\"; if ! launchctl bootout \"$job\" 2>/dev/null; then if launchctl print \"$job\" >/dev/null 2>&1; then exit 143; fi; fi; if launchctl print \"$job\" >/dev/null 2>&1; then exit 144; fi; if grep -qx loaded {snapshot}/$service.loaded 2>/dev/null; then launchctl bootstrap \"$domain\" \"$plist\"; if ! launchctl print \"$job\" >/dev/null 2>&1; then exit 145; fi; if grep -qx active {snapshot}/$service.active 2>/dev/null; then launchctl kickstart -k \"$job\"; if ! {running_check}; then exit 146; fi; else if {running_check}; then launchctl kill TERM \"$job\" 2>/dev/null || true; fi; if {running_check}; then exit 147; fi; fi; else if {running_check}; then exit 148; fi; fi; done",
            running_check = launchd_running_check("\"$job\""),
        ),
    }
}

fn rollback_health_command(
    current: &str,
    database_path: Option<&str>,
    config_path: Option<&str>,
    snapshot: Option<&str>,
    fallback_port: u16,
    platform: ServicePlatform,
) -> String {
    let current = shell_quote(current);
    let database = database_path
        .map(shell_quote)
        .unwrap_or_else(|| "\"$HOME/.local/state/dirtydash/data/dirtydash.sqlite3\"".to_string());
    let config = config_path
        .map(shell_quote)
        .unwrap_or_else(|| "\"$HOME/.config/dirtydash/config.toml\"".to_string());
    let port = snapshot
        .map(|snapshot| {
            let snapshot = shell_quote(snapshot);
            format!(
                "if [ -s {snapshot}/listener-port ]; then port=$(cat {snapshot}/listener-port); elif [ -s {snapshot}/hub-port ]; then port=$(cat {snapshot}/hub-port); else port={fallback_port}; fi; ",
            )
        })
        .unwrap_or_else(|| format!("port={fallback_port}; "));
    let service_health = if let Some(snapshot) = snapshot {
        let snapshot = shell_quote(snapshot);
        let hub_active = match platform {
            ServicePlatform::Systemd => {
                "systemctl --user is-active --quiet dirtydash-hub.service".to_string()
            }
            ServicePlatform::Launchd => format!(
                "domain=gui/$(id -u); launchctl print \"$domain/dev.dirtydash.hub\" >/dev/null 2>&1; {}",
                launchd_running_check("\"$domain/dev.dirtydash.hub\""),
            )
        };
        let collector_active = match platform {
            ServicePlatform::Systemd => "systemctl --user is-active --quiet dirtydash-collector.service".to_string(),
            ServicePlatform::Launchd => format!(
                "domain=gui/$(id -u); launchctl print \"$domain/dev.dirtydash.collector\" >/dev/null 2>&1; {}",
                launchd_running_check("\"$domain/dev.dirtydash.collector\""),
            ),
        };
        let inactive_hub_check = match platform {
            ServicePlatform::Systemd => {
                "systemctl --user is-active --quiet dirtydash-hub.service".to_string()
            }
            ServicePlatform::Launchd => launchd_running_check("\"$domain/dev.dirtydash.hub\""),
        };
        let inactive_collector_check = match platform {
            ServicePlatform::Systemd => {
                "systemctl --user is-active --quiet dirtydash-collector.service".to_string()
            }
            ServicePlatform::Launchd => {
                launchd_running_check("\"$domain/dev.dirtydash.collector\"")
            }
        };
        format!(
            "if grep -qx active {snapshot}/dirtydash-hub.service.active 2>/dev/null; then {hub_active}; command -v curl >/dev/null; curl --fail --silent --show-error --max-time 10 http://127.0.0.1:\"$port\"/healthz >/dev/null; else if {inactive_hub_check}; then exit 144; fi; fi; if grep -qx active {snapshot}/dirtydash-collector.service.active 2>/dev/null; then test -x {current}/dirtydash; {current}/dirtydash --config {config} --db {database} collector diagnostics --json >/dev/null; {collector_active}; else if {inactive_collector_check}; then exit 145; fi; fi; ",
            snapshot = snapshot,
            hub_active = hub_active,
            inactive_hub_check = inactive_hub_check,
            current = current,
            config = config,
            database = database,
            collector_active = collector_active,
            inactive_collector_check = inactive_collector_check,
        )
    } else {
        format!(
            "command -v curl >/dev/null; curl --fail --silent --show-error --max-time 10 http://127.0.0.1:\"$port\"/healthz >/dev/null; test -x {current}/dirtydash; {current}/dirtydash --config {config} --db {database} collector diagnostics --json >/dev/null; ",
            current = current,
            config = config,
            database = database,
        )
    };
    format!(
        "{port}case \"$port\" in ''|*[!0-9]*) exit 74;; esac; {service_health}",
        port = port,
        service_health = service_health,
    )
}

struct RollbackCommandInput<'a> {
    current: &'a str,
    previous: Option<&'a str>,
    database_path: Option<&'a str>,
    database_backup: Option<&'a str>,
    database_wal_backup: Option<&'a str>,
    database_shm_backup: Option<&'a str>,
    config_path: Option<&'a str>,
    service_dir: Option<&'a str>,
    platform: ServicePlatform,
    listener: Option<&'a ListenerPlan>,
    snapshot: Option<&'a str>,
}

fn rollback_command(input: RollbackCommandInput<'_>) -> Result<String> {
    let RollbackCommandInput {
        current,
        previous,
        database_path,
        database_backup,
        database_wal_backup,
        database_shm_backup,
        config_path,
        service_dir,
        platform,
        listener,
        snapshot,
    } = input;
    let database_restore = if snapshot.is_some() {
        String::new()
    } else {
        match (database_path, database_backup) {
            (Some(database), Some(backup)) => {
                let wal = database_wal_backup
                    .map(shell_quote)
                    .unwrap_or_else(|| shell_quote(&format!("{backup}-wal")));
                let shm = database_shm_backup
                    .map(shell_quote)
                    .unwrap_or_else(|| shell_quote(&format!("{backup}-shm")));
                format!(
                "{validate}; rm -f {database}-wal {database}-shm; mv -f {backup} {database}; if [ -e {wal} ]; then mv -f {wal} {database}-wal; fi; if [ -e {shm} ]; then mv -f {shm} {database}-shm; fi; ",
                validate = sqlite_header_command(backup),
                database = shell_quote(database),
                backup = shell_quote(backup),
                wal = wal,
                shm = shm,
            )
            }
            (Some(_), None) => {
                bail!("rollback refuses to delete a database without a validated backup")
            }
            _ => String::new(),
        }
    };
    let snapshot_restore = snapshot
        .map(|snapshot| {
            snapshot_restore_command(snapshot, config_path, service_dir, database_path, platform)
        })
        .unwrap_or_default();
    let listener_restore = snapshot
        .map(listener_snapshot_restore_command)
        .or_else(|| listener.map(listener_restore_command))
        .unwrap_or_default();
    let activation = rollback_activation_command(current, previous, platform, snapshot);
    let restart = rollback_service_restart_command(platform, snapshot, service_dir);
    let health = rollback_health_command(
        current,
        database_path,
        config_path,
        snapshot,
        listener
            .map(|listener| listener.local_port)
            .unwrap_or(DEFAULT_HUB_PORT),
        platform,
    );
    Ok(format!(
        "set -eu; {quiesce}; {database_restore}{activation}{snapshot_restore}{listener_restore}{restart}; {health}",
        quiesce = service_quiesce_command(platform),
        database_restore = database_restore,
        activation = activation,
        snapshot_restore = snapshot_restore,
        listener_restore = listener_restore,
        restart = restart,
        health = health,
    ))
}

fn action_command(action: &RemoteAction) -> Result<String> {
    match action {
        RemoteAction::PreparePaths { paths } => Ok(format!(
            "set -eu; umask 077; mkdir -p {} {} {} {} {} {} {}",
            shell_quote(&paths.config_dir),
            shell_quote(&paths.state_dir),
            shell_quote(&paths.data_dir),
            shell_quote(&paths.releases_dir),
            shell_quote(&paths.release_dir),
            shell_quote(&paths.service_dir),
            shell_quote(&paths.home),
        )),
        RemoteAction::SnapshotRollbackState {
            paths,
            platform,
            listener,
        } => snapshot_command(paths, *platform, listener),
        RemoteAction::QuiesceServices { platform } =>
            Ok(service_quiesce_command(*platform).to_string()),
        RemoteAction::AtomicallyActivate { current, release, platform } => {
            let temp = format!("{current}.next-{}", std::process::id());
            let move_command = match platform {
                ServicePlatform::Systemd => format!("mv -Tf {} {}", shell_quote(&temp), shell_quote(current)),
                ServicePlatform::Launchd => format!("mv -f {} {}", shell_quote(&temp), shell_quote(current)),
            };
            Ok(format!(
                "set -eu; ln -s {} {}; {}; test -L {}",
                shell_quote(release),
                shell_quote(&temp),
                move_command,
                shell_quote(current),
            ))
        }
        RemoteAction::InstallDatabaseSeed {
            seed_path,
            database_path,
            backup_path,
            wal_backup_path,
            shm_backup_path,
        } => {
            // The old database is never removed without a validated backup.
            // SQLite's sidecars are copied as part of the same quiesced
            // transaction so a WAL-backed database can be restored without
            // putting the 16-byte magic in a shell variable.  The Python/od
            // helper is deliberately independent of sqlite3 availability.
            let seed = shell_quote(seed_path);
            let db = shell_quote(database_path);
            let backup = shell_quote(backup_path);
            let wal = shell_quote(wal_backup_path);
            let shm = shell_quote(shm_backup_path);
            Ok(format!(
                "set -eu; test -s {seed}; {seed_header}; if [ -e {db} ]; then {db_header}; cp -p {db} {backup}; test -s {backup}; {backup_header}; if [ -e {db}-wal ]; then cp -p {db}-wal {wal}; else rm -f {wal}; fi; if [ -e {db}-shm ]; then cp -p {db}-shm {shm}; else rm -f {shm}; fi; if command -v sqlite3 >/dev/null 2>&1; then sqlite3 {backup} 'PRAGMA integrity_check' | grep -qx ok; fi; else test ! -e {backup}; test ! -e {wal}; test ! -e {shm}; fi; rm -f {db}-wal {db}-shm; mv -f {seed} {db}",
                seed = seed,
                seed_header = sqlite_header_command(seed_path),
                db = db,
                db_header = sqlite_header_command(database_path),
                backup = backup,
                backup_header = sqlite_header_command(backup_path),
                wal = wal,
                shm = shm,
            ))
        }
        RemoteAction::InstallRuntimeConfig { config_path, .. } => {
            Ok(format!("set -eu; test -f {}", shell_quote(config_path)))
        }
        RemoteAction::InstallService { path, mode, .. } => {
            if !matches!(mode, 0o600 | 0o644 | 0o755) {
                bail!("service definition mode is not allowlisted");
            }
            Ok(format!("set -eu; test -f {}", shell_quote(path)))
        }
        RemoteAction::RestartServices { platform } => Ok(match platform {
            ServicePlatform::Systemd => "set -eu; systemctl --user daemon-reload; systemctl --user enable dirtydash-hub.service dirtydash-collector.service; systemctl --user restart dirtydash-hub.service; systemctl --user restart dirtydash-collector.service; systemctl --user is-active --quiet dirtydash-hub.service; systemctl --user is-active --quiet dirtydash-collector.service".to_string(),
            ServicePlatform::Launchd => service_restart_command(*platform).to_string(),
        }),
        RemoteAction::HealthCheck { port, platform } => Ok(format!(
            "set -eu; command -v curl >/dev/null; curl --fail --silent --show-error --max-time 10 http://127.0.0.1:{port}/healthz >/dev/null; {}",
            service_health_command(*platform),
        )),
        RemoteAction::ConfigureTailscale { port } => Ok(format!(
            "set -eu; command -v tailscale >/dev/null; tailscale serve --https=443 http://127.0.0.1:{port}",
        )),
        RemoteAction::VerifyReceipt { release, port, platform } => Ok(format!(
            "set -eu; test -x {}/dirtydash; curl --fail --silent --show-error --max-time 10 http://127.0.0.1:{port}/healthz >/dev/null; {}",
            shell_quote(release),
            service_health_command(*platform),
        )),
        RemoteAction::Rollback {
            current,
            previous,
            database_path,
            database_backup,
            database_wal_backup,
            database_shm_backup,
            config_path,
            service_dir,
            platform,
            listener,
            snapshot_dir,
        } => rollback_command(RollbackCommandInput {
            current,
            previous: previous.as_deref(),
            database_path: database_path.as_deref(),
            database_backup: database_backup.as_deref(),
            database_wal_backup: database_wal_backup.as_deref(),
            database_shm_backup: database_shm_backup.as_deref(),
            config_path: config_path.as_deref(),
            service_dir: service_dir.as_deref(),
            platform: *platform,
            listener: listener.as_ref(),
            snapshot: snapshot_dir.as_deref(),
        }),
        RemoteAction::Cleanup {
            release,
            remove_release,
            database_backup,
            database_wal_backup,
            database_shm_backup,
            temporary_seed,
            rollback_snapshot,
        } => {
            let backup_cleanup = database_backup
                .as_ref()
                .map(|backup| format!("rm -f {}; ", shell_quote(backup)))
                .unwrap_or_default();
            let wal_cleanup = database_wal_backup
                .as_ref()
                .map(|backup| format!("rm -f {}; ", shell_quote(backup)))
                .unwrap_or_default();
            let shm_cleanup = database_shm_backup
                .as_ref()
                .map(|backup| format!("rm -f {}; ", shell_quote(backup)))
                .unwrap_or_default();
            let seed_cleanup = temporary_seed
                .as_ref()
                .map(|seed| format!("rm -f {}; ", shell_quote(seed)))
                .unwrap_or_default();
            let snapshot_cleanup = rollback_snapshot
                .as_ref()
                .map(|snapshot| format!("rm -rf -- {}; ", shell_quote(snapshot)))
                .unwrap_or_default();
            if *remove_release {
                Ok(format!("set -eu; {backup_cleanup}{wal_cleanup}{shm_cleanup}{seed_cleanup}{snapshot_cleanup}rm -rf -- {}", shell_quote(release), backup_cleanup = backup_cleanup, wal_cleanup = wal_cleanup, shm_cleanup = shm_cleanup, seed_cleanup = seed_cleanup, snapshot_cleanup = snapshot_cleanup))
            } else {
                Ok(format!("set -eu; {backup_cleanup}{wal_cleanup}{shm_cleanup}{seed_cleanup}{snapshot_cleanup}true", backup_cleanup = backup_cleanup, wal_cleanup = wal_cleanup, shm_cleanup = shm_cleanup, seed_cleanup = seed_cleanup, snapshot_cleanup = snapshot_cleanup))
            }
        }
    }
}

fn previous_listener_state(plan: &DeploymentPlan) -> ListenerPlan {
    let mut listener = plan.listener.clone();
    listener.tailscale_state = plan.rollback.previous_listener_state;
    listener
}

fn listener_restore_command(plan: &ListenerPlan) -> String {
    if plan.access_mode != ListenerAccessMode::TailscaleServe {
        return String::new();
    }
    match plan.tailscale_state {
        TailscaleServeState::NotConfigured => {
            "command -v tailscale >/dev/null && tailscale serve reset; ".to_string()
        }
        TailscaleServeState::Enabled => format!(
            "command -v tailscale >/dev/null && tailscale serve --https=443 http://127.0.0.1:{}; ",
            plan.local_port
        ),
        TailscaleServeState::ConsentRequired => String::new(),
    }
}

fn service_health_command(platform: ServicePlatform) -> &'static str {
    match platform {
        ServicePlatform::Systemd => "systemctl --user is-active --quiet dirtydash-hub.service; systemctl --user is-active --quiet dirtydash-collector.service",
        ServicePlatform::Launchd => "domain=gui/$(id -u); launchctl print \"$domain/dev.dirtydash.hub\" >/dev/null 2>&1; launchctl print \"$domain/dev.dirtydash.hub\" 2>/dev/null | grep -Eq '(^|[[:space:]])state = running([[:space:]]|$)|(^|[[:space:]])pid = [1-9][0-9]*([[:space:]]|$)'; launchctl print \"$domain/dev.dirtydash.collector\" >/dev/null 2>&1; launchctl print \"$domain/dev.dirtydash.collector\" 2>/dev/null | grep -Eq '(^|[[:space:]])state = running([[:space:]]|$)|(^|[[:space:]])pid = [1-9][0-9]*([[:space:]]|$)'",
    }
}

fn service_quiesce_command(platform: ServicePlatform) -> &'static str {
    match platform {
        ServicePlatform::Systemd => "set -eu; for service in dirtydash-hub.service dirtydash-collector.service; do state=$(systemctl --user show \"$service\" --property=LoadState --no-pager 2>/dev/null); case \"$state\" in *LoadState=not-found*) :;; *) systemctl --user stop \"$service\";; esac; done",
        ServicePlatform::Launchd => "set -eu; domain=gui/$(id -u); for service in dev.dirtydash.hub dev.dirtydash.collector; do launchctl kill TERM \"$domain/$service\" 2>/dev/null || true; if ! launchctl bootout \"$domain/$service\" 2>/dev/null; then if launchctl print \"$domain/$service\" >/dev/null 2>&1; then exit 149; fi; fi; if launchctl print \"$domain/$service\" >/dev/null 2>&1; then exit 150; fi; done",
    }
}

fn service_restart_command(platform: ServicePlatform) -> &'static str {
    match platform {
        ServicePlatform::Systemd => "systemctl --user daemon-reload; systemctl --user restart dirtydash-hub.service; systemctl --user restart dirtydash-collector.service; systemctl --user is-active --quiet dirtydash-hub.service; systemctl --user is-active --quiet dirtydash-collector.service",
        ServicePlatform::Launchd => "domain=gui/$(id -u); hub=$domain/dev.dirtydash.hub; collector=$domain/dev.dirtydash.collector; if launchctl print \"$hub\" >/dev/null 2>&1; then :; else launchctl bootstrap \"$domain\" \"$HOME/Library/LaunchAgents/dev.dirtydash.hub.plist\"; fi; if launchctl print \"$collector\" >/dev/null 2>&1; then :; else launchctl bootstrap \"$domain\" \"$HOME/Library/LaunchAgents/dev.dirtydash.collector.plist\"; fi; launchctl kickstart -k \"$hub\"; launchctl kickstart -k \"$collector\"; launchctl print \"$hub\" >/dev/null 2>&1; launchctl print \"$hub\" 2>/dev/null | grep -Eq '(^|[[:space:]])state = running([[:space:]]|$)|(^|[[:space:]])pid = [1-9][0-9]*([[:space:]]|$)'; launchctl print \"$collector\" >/dev/null 2>&1; launchctl print \"$collector\" 2>/dev/null | grep -Eq '(^|[[:space:]])state = running([[:space:]]|$)|(^|[[:space:]])pid = [1-9][0-9]*([[:space:]]|$)'",
    }
}

fn activation_move_command(temp: &str, current: &str, platform: ServicePlatform) -> String {
    match platform {
        ServicePlatform::Systemd => {
            format!("mv -Tf {} {}", shell_quote(temp), shell_quote(current))
        }
        ServicePlatform::Launchd => format!("mv -f {} {}", shell_quote(temp), shell_quote(current)),
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
#[path = "deployment_tests.rs"]
mod tests;
