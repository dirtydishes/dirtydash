//! Signed release deployment for a Hub and its local Collector.
//!
//! The public seam is intentionally narrow:
//!
//! * [`ArtifactManifest`] verifies an Ed25519-signed, SHA-256-checksummed
//!   release and selects one of four supported Linux/macOS targets.
//! * [`DeploymentPlan`] is a typed, serializable, secret-free description of
//!   the remote mutations.
//! * [`RemoteExecutor`] is the only seam that can perform those mutations;
//!   the production adapter uses fixed-allowlist SSH options and stdin only
//!   for non-secret artifact/database bytes.
//! * [`DeploymentRunner`] executes the immutable plan and performs atomic
//!   current-pointer rollback and cleanup on every failed mutation.
//!
//! No source builds, Docker assumptions, shell environment secrets, or private
//! signing material are part of this module.

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

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
        PublisherKey, SignedArtifactManifest, TargetPlatform, VerifiedArtifact,
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
    pub use super::{RemoteAction, RemoteExecutor, RemoteResult, RemoteStatus, SshRemoteExecutor};
}

pub mod runner {
    pub use super::{
        DeploymentCheckpoint, DeploymentReceipt, DeploymentRequest, DeploymentRunner,
        DeploymentStateStore,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublisherKey {
    key_id: String,
    fingerprint: String,
    public_key: [u8; 32],
}

impl PublisherKey {
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

    /// Verify using the explicitly pinned publisher key.  The key ID and
    /// fingerprint are both checked before the signature so replacing a
    /// manifest and a public-key file together cannot silently authorize a
    /// different publisher.
    pub fn verify_with_publisher(
        &self,
        publisher: &PublisherKey,
    ) -> Result<VerifiedArtifactManifest> {
        if self.key_id != publisher.key_id {
            bail!("signed artifact key ID is not on the allowed publisher list");
        }
        let actual_fingerprint = publisher_fingerprint(&publisher.public_key);
        if actual_fingerprint != publisher.fingerprint {
            bail!("allowed publisher fingerprint is invalid");
        }
        self.verify_signature(&publisher.public_key)
    }

    /// The default verifier accepts only the canonical key-fingerprint ID.
    /// Callers with a release-specific key ID must use
    /// [`Self::verify_with_publisher`] and persist that allowlist outside the
    /// ordinary deployment plan.
    pub fn verify(&self, public_key: &[u8]) -> Result<VerifiedArtifactManifest> {
        let fingerprint = PublisherKey::fingerprint(public_key)?;
        if self.key_id != fingerprint {
            bail!("signed artifact key ID must equal the pinned public-key fingerprint");
        }
        let publisher = PublisherKey::new(self.key_id.clone(), fingerprint, public_key)?;
        self.verify_with_publisher(&publisher)
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
    pub target: String,
    pub ssh_target: Option<CanonicalSshTarget>,
    pub release: String,
    pub platform: Option<TargetPlatform>,
    pub target_facts: Option<RemoteFacts>,
    pub artifact: Option<ArtifactEvidence>,
    pub listener: ListenerPlan,
    pub exposure: ListenerExposure,
    pub seed_intent: SeedIntent,
    pub backfill_intent: BackfillIntent,
    pub database_seed: bool,
    pub paths: Option<DeploymentPaths>,
    pub rollback: RollbackData,
    pub steps: Vec<DeploymentStep>,
    pub rollback_steps: Vec<DeploymentStep>,
    pub plan_hash: String,
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

    pub fn refresh_hash(&mut self) -> Result<()> {
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

    pub fn save_plan(&self, plan: &DeploymentPlan) -> Result<()> {
        plan.verify_hash()?;
        let path = self.plan_path();
        let bytes = serde_json::to_vec_pretty(plan)?;
        atomic_write(&path, &bytes, 0o600)
    }

    pub fn load_plan(&self) -> Result<Option<DeploymentPlan>> {
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

    pub fn mark_reviewed(&self, plan: &DeploymentPlan) -> Result<()> {
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
    state_store: Option<DeploymentStateStore>,
    reviewed_plan: Option<DeploymentPlan>,
}

impl<E> DeploymentRunner<E> {
    pub fn new(executor: E) -> Self {
        Self {
            executor,
            state_store: None,
            reviewed_plan: None,
        }
    }

    pub fn with_state_store(mut self, store: DeploymentStateStore) -> Self {
        self.state_store = Some(store);
        self
    }

    /// Bind a plan already durably reviewed by another workflow seam (the
    /// enrollment draft).  The plan is still hash-verified and recomputed
    /// against fresh facts before mutation.
    pub fn with_reviewed_plan(mut self, plan: DeploymentPlan) -> Self {
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
        let result = (|| -> Result<DeploymentReceipt> {
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
                self.executor.run(RemoteAction::SnapshotRollbackState {
                    paths: paths.clone(),
                    platform: facts.platform.service_platform(),
                    listener: plan.listener.clone(),
                }),
                "snapshot remote rollback state",
            )?;
            mutated = true;
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
                if mutated {
                    if let Err(rollback) = self.executor.run(RemoteAction::Rollback {
                        current: paths.current.clone(),
                        previous: plan.rollback.previous_release.clone(),
                        database_path: request.database_seed.as_ref().map(|_| paths.hub_db.clone()),
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
                        platform: facts.platform.service_platform(),
                        listener: Some(previous_listener_state(&plan)),
                        snapshot_dir: plan.rollback.rollback_snapshot_dir.clone(),
                    }) {
                        let _ = rollback;
                        cleanup_error = Some("rollback operation failed".to_string());
                    } else {
                        rollback_succeeded = true;
                    }
                }
                if let Err(cleanup) = self.executor.run(RemoteAction::Cleanup {
                    release: paths.release_dir,
                    remove_release: true,
                    database_backup: None,
                    database_wal_backup: None,
                    database_shm_backup: None,
                    temporary_seed: request
                        .database_seed
                        .as_ref()
                        .map(|_| format!("{}/dirtydash.sqlite3.seed", paths.data_dir)),
                    rollback_snapshot: rollback_succeeded
                        .then(|| plan.rollback.rollback_snapshot_dir.clone())
                        .flatten(),
                }) {
                    let _ = cleanup;
                    cleanup_error = Some("cleanup operation failed".to_string());
                }
                let message = match cleanup_error {
                    Some(cleanup) => {
                        format!("deployment failed; rollback/cleanup also failed: {cleanup}")
                    }
                    None => {
                        "deployment failed; remote state was rolled back and cleaned".to_string()
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

/// Production SSH adapter.  Its command line is fixed and carries no
/// password, passphrase, sudo secret, or signing key.  Artifact and seed bytes
/// are written to the remote command's stdin and never placed in an argument,
/// environment variable, temporary local file, or diagnostic.
#[derive(Debug, Clone)]
pub struct SshRemoteExecutor {
    target: CanonicalSshTarget,
    known_hosts: PathBuf,
    key_path: Option<PathBuf>,
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
        })
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
        self.target
            .ssh_args(&self.known_hosts, self.key_path.as_deref(), true)
    }

    fn invocation(&self, command: &str) -> Result<std::process::Output> {
        let mut process = Command::new("ssh");
        process.args(self.base_args()).arg(command);
        process
            .output()
            .context("running fixed-allowlist SSH operation")
    }
}

impl RemoteExecutor for SshRemoteExecutor {
    fn canonical_target(&self) -> Option<&CanonicalSshTarget> {
        Some(&self.target)
    }

    fn detect(&mut self) -> Result<RemoteFacts> {
        let output = self.invocation(
            "set -eu; printf 'os=%s\\n' \"$(uname -s)\"; printf 'arch=%s\\n' \"$(uname -m)\"; printf 'user=%s\\n' \"$(id -un)\"; printf 'uid=%s\\n' \"$(id -u)\"; printf 'home=%s\\n' \"$HOME\"; if [ -L \"$HOME/.local/share/dirtydash/current\" ]; then printf 'current=%s\\n' \"$(readlink \"$HOME/.local/share/dirtydash/current\")\"; fi",
        )?;
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
        let command = format!(
            "set -eu; umask 077; cat > {}; chmod {:o} {}; mv -f {} {}",
            shell_quote(&temp),
            mode,
            shell_quote(&temp),
            shell_quote(&temp),
            shell_quote(destination),
        );
        let mut process = Command::new("ssh");
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
        RemoteAction::SnapshotRollbackState { paths, platform: _, listener: _ } => Ok(format!(
            "set -eu; umask 077; snapshot={}; mkdir -p {}; if [ -f {} ]; then cp -p {} {}/config.toml; else : > {}/.missing-config; fi; for service in {}/dirtydash-hub.service {}/dirtydash-collector.service {}/dev.dirtydash.hub.plist {}/dev.dirtydash.collector.plist; do if [ -f \"$service\" ]; then cp -p \"$service\" \"$snapshot/$(basename \"$service\")\"; else : > \"$snapshot/.missing-$(basename \"$service\")\"; fi; done",

            shell_quote(&format!("{}/deployment-rollback", paths.state_dir)),
            shell_quote(&format!("{}/deployment-rollback", paths.state_dir)),
            shell_quote(&paths.config_file),
            shell_quote(&paths.config_file),
            shell_quote(&format!("{}/deployment-rollback", paths.state_dir)),
            shell_quote(&format!("{}/deployment-rollback", paths.state_dir)),
            shell_quote(&paths.service_dir),
            shell_quote(&paths.service_dir),
            shell_quote(&paths.service_dir),
            shell_quote(&paths.service_dir),
        )),
        RemoteAction::QuiesceServices { platform } => Ok(match platform {
            ServicePlatform::Systemd => "set -eu; systemctl --user stop dirtydash-hub.service; systemctl --user stop dirtydash-collector.service".to_string(),
            ServicePlatform::Launchd => "set -eu; domain=gui/$(id -u); launchctl kill TERM \"$domain/dev.dirtydash.hub\" 2>/dev/null || true; launchctl kill TERM \"$domain/dev.dirtydash.collector\" 2>/dev/null || true".to_string(),
        }),
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
            // transaction so a WAL-backed database cannot be restored into an
            // inconsistent main file.
            Ok(format!(
                "set -eu; test -s {seed}; if command -v sqlite3 >/dev/null 2>&1; then sqlite3 {seed} 'PRAGMA integrity_check' | grep -qx ok; else test \"$(dd if={seed} bs=1 count=16 2>/dev/null)\" = \"SQLite format 3\\000\"; fi; if [ -e {db} ]; then cp -p {db} {backup}; test -s {backup}; if command -v sqlite3 >/dev/null 2>&1; then sqlite3 {backup} 'PRAGMA integrity_check' | grep -qx ok; fi; if [ -e {db}-wal ]; then cp -p {db}-wal {wal}; else rm -f {wal}; fi; if [ -e {db}-shm ]; then cp -p {db}-shm {shm}; else rm -f {shm}; fi; else test ! -e {backup}; test ! -e {wal}; test ! -e {shm}; fi; rm -f {db}-wal {db}-shm; mv -f {seed} {db}",
                seed = shell_quote(seed_path),
                db = shell_quote(database_path),
                backup = shell_quote(backup_path),
                wal = shell_quote(wal_backup_path),
                shm = shell_quote(shm_backup_path),
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
            ServicePlatform::Launchd => "set -eu; domain=gui/$(id -u); hub=$domain/dev.dirtydash.hub; collector=$domain/dev.dirtydash.collector; if launchctl print \"$hub\" >/dev/null 2>&1; then :; else launchctl bootstrap \"$domain\" \"$HOME/Library/LaunchAgents/dev.dirtydash.hub.plist\"; fi; if launchctl print \"$collector\" >/dev/null 2>&1; then :; else launchctl bootstrap \"$domain\" \"$HOME/Library/LaunchAgents/dev.dirtydash.collector.plist\"; fi; launchctl kickstart -k \"$hub\"; launchctl kickstart -k \"$collector\"; launchctl print \"$hub\" >/dev/null; launchctl print \"$collector\" >/dev/null".to_string(),
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
            platform,
            listener,
            snapshot_dir,
        } => {
            let database_restore = match (database_path, database_backup) {
                (Some(database), Some(backup)) => {
                    let wal_restore = database_wal_backup
                        .as_ref()
                        .zip(database_shm_backup.as_ref())
                        .map(|(wal, shm)| format!("if [ -e {wal} ]; then mv -f {wal} {database}-wal; fi; if [ -e {shm} ]; then mv -f {shm} {database}-shm; fi; ", wal = shell_quote(wal), shm = shell_quote(shm), database = shell_quote(database)))
                        .unwrap_or_default();
                    format!(
                        "test -f {backup}; mv -f {backup} {database}; {wal_restore}",
                        database = shell_quote(database),
                        backup = shell_quote(backup),
                        wal_restore = wal_restore,
                    )
                }
                (Some(_), None) => bail!("rollback refuses to delete a database without a validated backup"),
                _ => String::new(),
            };
            let listener_restore = listener
                .as_ref()
                .map(listener_restore_command)
                .unwrap_or_default();
            let snapshot_restore = snapshot_dir
                .as_ref()
                .map(|snapshot| format!("if [ -f {snapshot}/config.toml ]; then mv -f {snapshot}/config.toml \"$HOME/.config/dirtydash/config.toml\"; elif [ -f {snapshot}/.missing-config ]; then rm -f \"$HOME/.config/dirtydash/config.toml\"; fi; for service in dirtydash-hub.service dirtydash-collector.service; do if [ -f {snapshot}/$service ]; then mv -f {snapshot}/$service \"$HOME/.config/dirtydash/systemd/user/$service\"; elif [ -f {snapshot}/.missing-$service ]; then rm -f \"$HOME/.config/dirtydash/systemd/user/$service\"; fi; done; for service in dev.dirtydash.hub.plist dev.dirtydash.collector.plist; do if [ -f {snapshot}/$service ]; then mv -f {snapshot}/$service \"$HOME/Library/LaunchAgents/$service\"; elif [ -f {snapshot}/.missing-$service ]; then rm -f \"$HOME/Library/LaunchAgents/$service\"; fi; done; ", snapshot = shell_quote(snapshot)))
                .unwrap_or_default();
            let Some(previous) = previous else {
                let quiesce = service_quiesce_command(*platform);
                return Ok(format!(
                    "set -eu; {database_restore}{quiesce}; rm -f {current}; {snapshot_restore}{listener_restore}{restart}",
                    database_restore = database_restore,
                    quiesce = quiesce,
                    current = shell_quote(current),
                    snapshot_restore = snapshot_restore,
                    listener_restore = listener_restore,
                    restart = service_restart_command(*platform),
                ));
            };
            let temp = format!("{current}.rollback-{}", std::process::id());
            Ok(format!(
                "set -eu; {database_restore}{quiesce}; ln -s {} {}; {}; {snapshot_restore}{listener_restore}{restart}",
                shell_quote(previous),
                shell_quote(&temp),
                activation_move_command(&temp, current, *platform),
                database_restore = database_restore,
                quiesce = service_quiesce_command(*platform),
                snapshot_restore = snapshot_restore,
                listener_restore = listener_restore,
                restart = service_restart_command(*platform),
            ))
        }
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
        ServicePlatform::Launchd => "domain=gui/$(id -u); launchctl print \"$domain/dev.dirtydash.hub\" >/dev/null; launchctl print \"$domain/dev.dirtydash.collector\" >/dev/null",
    }
}

fn service_quiesce_command(platform: ServicePlatform) -> &'static str {
    match platform {
        ServicePlatform::Systemd => "systemctl --user stop dirtydash-hub.service; systemctl --user stop dirtydash-collector.service",
        ServicePlatform::Launchd => "domain=gui/$(id -u); launchctl kill TERM \"$domain/dev.dirtydash.hub\" 2>/dev/null || true; launchctl kill TERM \"$domain/dev.dirtydash.collector\" 2>/dev/null || true",
    }
}

fn service_restart_command(platform: ServicePlatform) -> &'static str {
    match platform {
        ServicePlatform::Systemd => "systemctl --user daemon-reload; systemctl --user restart dirtydash-hub.service; systemctl --user restart dirtydash-collector.service; systemctl --user is-active --quiet dirtydash-hub.service; systemctl --user is-active --quiet dirtydash-collector.service",
        ServicePlatform::Launchd => "domain=gui/$(id -u); hub=$domain/dev.dirtydash.hub; collector=$domain/dev.dirtydash.collector; if launchctl print \"$hub\" >/dev/null 2>&1; then :; else launchctl bootstrap \"$domain\" \"$HOME/Library/LaunchAgents/dev.dirtydash.hub.plist\"; fi; if launchctl print \"$collector\" >/dev/null 2>&1; then :; else launchctl bootstrap \"$domain\" \"$HOME/Library/LaunchAgents/dev.dirtydash.collector.plist\"; fi; launchctl kickstart -k \"$hub\"; launchctl kickstart -k \"$collector\"; launchctl print \"$hub\" >/dev/null; launchctl print \"$collector\" >/dev/null",
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
