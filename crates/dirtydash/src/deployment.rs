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

use crate::listener::{ListenerAccessMode, ListenerPlan, TailscaleServeState};
use crate::service::{ServicePlatform, ServiceSpec};

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_HUB_PORT: u16 = 4599;
pub const DEFAULT_REMOTE_BASE: &str = ".local/share/dirtydash";
pub const DEFAULT_CONFIG_BASE: &str = ".config/dirtydash";

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
pub struct VerifiedArtifactManifest {
    pub key_id: String,
    pub manifest: ArtifactManifest,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifact {
    pub descriptor: ArtifactDescriptor,
    pub bytes: Vec<u8>,
    pub manifest: VerifiedArtifactManifest,
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

    pub fn verify(&self, public_key: &[u8]) -> Result<VerifiedArtifactManifest> {
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
        if self.key_id.trim().is_empty() || self.key_id.len() > 128 {
            bail!("artifact signing key ID is invalid");
        }
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
            manifest: self.manifest.clone(),
            manifest_sha256,
        })
    }
}

impl VerifiedArtifactManifest {
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
        bail!("SSH target is not a safe alias or user@host target");
    }
    Ok(())
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
pub struct DeploymentPlan {
    pub target: String,
    pub release: String,
    pub platform: Option<TargetPlatform>,
    pub listener: ListenerPlan,
    pub database_seed: bool,
    pub paths: Option<DeploymentPaths>,
    pub steps: Vec<DeploymentStep>,
    pub rollback_steps: Vec<DeploymentStep>,
    pub plan_hash: String,
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
        let mut plan = Self {
            target,
            release,
            platform: None,
            listener,
            database_seed,
            paths: None,
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
        let mut plan = Self::skeleton(target, release, listener, database_seed)?;
        plan.platform = Some(facts.platform);
        plan.paths = Some(DeploymentPaths::for_facts(facts, &plan.release)?);
        plan.steps = concrete_steps(database_seed, facts.platform.service_platform());
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
    AtomicallyActivate {
        current: String,
        release: String,
    },
    InstallDatabaseSeed {
        seed_path: String,
        database_path: String,
        backup_path: String,
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
    },
    ConfigureTailscale {
        port: u16,
    },
    VerifyReceipt {
        release: String,
        port: u16,
    },
    Rollback {
        current: String,
        previous: Option<String>,
        database_path: Option<String>,
        database_backup: Option<String>,
    },
    Cleanup {
        release: String,
        remove_release: bool,
        database_backup: Option<String>,
        temporary_seed: Option<String>,
    },
}

/// The executor seam carries only typed actions.  It has no method accepting
/// an arbitrary shell string and no secret parameter.
pub trait RemoteExecutor {
    fn detect(&mut self) -> Result<RemoteFacts>;
    fn run(&mut self, action: RemoteAction) -> Result<RemoteResult>;
    fn upload(&mut self, destination: &str, bytes: &[u8], mode: u32) -> Result<RemoteResult>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentReceipt {
    pub target: String,
    pub release: String,
    pub platform: TargetPlatform,
    pub plan_hash: String,
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

    pub fn clear(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeploymentRequest {
    pub target: String,
    pub release: String,
    pub listener: ListenerPlan,
    pub database_seed: Option<Vec<u8>>,
    pub approved_plan_hash: Option<String>,
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
}

impl<E> DeploymentRunner<E> {
    pub fn new(executor: E) -> Self {
        Self {
            executor,
            state_store: None,
        }
    }

    pub fn with_state_store(mut self, store: DeploymentStateStore) -> Self {
        self.state_store = Some(store);
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
    pub fn apply(
        &mut self,
        request: &DeploymentRequest,
        artifact: &VerifiedArtifact,
    ) -> Result<DeploymentReceipt> {
        request.listener.validate()?;
        if artifact.manifest.manifest.release != request.release {
            bail!("verified artifact release does not match the deployment request");
        }
        let facts = self
            .executor
            .detect()
            .context("remote platform detection failed")?;
        if artifact.descriptor.platform != facts.platform {
            bail!("verified artifact target does not match the detected remote platform");
        }
        let plan = DeploymentPlan::for_facts(
            request.target.clone(),
            request.release.clone(),
            &facts,
            request.listener.clone(),
            request.database_seed.is_some(),
        )?;
        if let Some(approved) = &request.approved_plan_hash {
            if approved != &plan.plan_hash {
                bail!("deployment plan changed after review; refusing to execute stale approval");
            }
        }
        self.save_checkpoint(&DeploymentCheckpoint {
            target: plan.target.clone(),
            release: plan.release.clone(),
            plan_hash: plan.plan_hash.clone(),
            status: "reviewed".to_string(),
            tailscale_state: plan.listener.tailscale_state,
            receipt: None,
        })?;

        let paths = plan
            .paths
            .clone()
            .context("deployment plan has no remote paths")?;
        let service_spec = service_spec(&facts, &paths, &plan.listener)?;
        let rendered_services = service_spec.render()?;
        let mut activated = false;
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
                    &artifact.bytes,
                    0o755,
                ),
                "upload verified artifact",
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
                }),
                "activate release",
            )?;
            activated = true;
            require_success(
                self.executor.run(RemoteAction::RestartServices {
                    platform: facts.platform.service_platform(),
                }),
                "restart services",
            )?;
            let health = require_success(
                self.executor.run(RemoteAction::HealthCheck {
                    port: plan.listener.local_port,
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
                    temporary_seed: request
                        .database_seed
                        .as_ref()
                        .map(|_| format!("{}/dirtydash.sqlite3.seed", paths.data_dir)),
                }),
                "cleanup deployment temporary files",
            )?;
            let receipt = DeploymentReceipt {
                target: plan.target.clone(),
                release: plan.release.clone(),
                platform: facts.platform,
                plan_hash: plan.plan_hash.clone(),
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
                if activated {
                    if let Err(rollback) = self.executor.run(RemoteAction::Rollback {
                        current: paths.current.clone(),
                        previous: facts.current_release.clone(),
                        database_path: request.database_seed.as_ref().map(|_| paths.hub_db.clone()),
                        database_backup: request
                            .database_seed
                            .as_ref()
                            .map(|_| paths.hub_db_backup.clone()),
                    }) {
                        let _ = rollback;
                        cleanup_error = Some("rollback operation failed".to_string());
                    }
                }
                if let Err(cleanup) = self.executor.run(RemoteAction::Cleanup {
                    release: paths.release_dir,
                    remove_release: true,
                    database_backup: None,
                    temporary_seed: request
                        .database_seed
                        .as_ref()
                        .map(|_| format!("{}/dirtydash.sqlite3.seed", paths.data_dir)),
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
    target: String,
    known_hosts: PathBuf,
    key_path: Option<PathBuf>,
}

impl SshRemoteExecutor {
    pub fn new(target: impl Into<String>, known_hosts: impl Into<PathBuf>) -> Result<Self> {
        let target = target.into();
        let known_hosts = known_hosts.into();
        validate_ssh_target(&target)?;
        if let Some(parent) = known_hosts.parent() {
            fs::create_dir_all(parent)?;
        }
        if !known_hosts.exists() {
            atomic_write(&known_hosts, b"", 0o600)?;
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
        let mut args = vec![
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=yes".to_string(),
            "-o".to_string(),
            format!("UserKnownHostsFile={}", self.known_hosts.display()),
            "-o".to_string(),
            "ConnectTimeout=10".to_string(),
        ];
        if let Some(key_path) = &self.key_path {
            args.extend([
                "-i".to_string(),
                key_path.display().to_string(),
                "-o".to_string(),
                "IdentitiesOnly=yes".to_string(),
            ]);
        }
        args.push(self.target.clone());
        args
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
        RemoteAction::AtomicallyActivate { current, release } => {
            let temp = format!("{current}.next-{}", std::process::id());
            Ok(format!(
                "set -eu; rm -f {}; ln -s {} {}; mv -Tf {} {}",
                shell_quote(&temp),
                shell_quote(release),
                shell_quote(&temp),
                shell_quote(&temp),
                shell_quote(current),
            ))
        }
        RemoteAction::InstallDatabaseSeed { seed_path, database_path, backup_path } => {
            Ok(format!(
                "set -eu; if [ -f {db} ]; then cp -p {db} {backup}; else rm -f {backup}; fi; mv -f {seed} {db}",
                db = shell_quote(database_path),
                backup = shell_quote(backup_path),
                seed = shell_quote(seed_path),
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
            ServicePlatform::Systemd => "set -eu; systemctl --user daemon-reload; systemctl --user enable --now dirtydash-hub.service dirtydash-collector.service; systemctl --user restart dirtydash-hub.service dirtydash-collector.service".to_string(),
            ServicePlatform::Launchd => "set -eu; launchctl bootstrap gui/$(id -u) \"$HOME/Library/LaunchAgents/dev.dirtydash.hub.plist\" 2>/dev/null || true; launchctl bootstrap gui/$(id -u) \"$HOME/Library/LaunchAgents/dev.dirtydash.collector.plist\" 2>/dev/null || true; launchctl kickstart -k gui/$(id -u)/dev.dirtydash.hub; launchctl kickstart -k gui/$(id -u)/dev.dirtydash.collector".to_string(),
        }),
        RemoteAction::HealthCheck { port } => Ok(format!(
            "set -eu; command -v curl >/dev/null; curl --fail --silent --show-error --max-time 10 http://127.0.0.1:{port}/healthz >/dev/null",
        )),
        RemoteAction::ConfigureTailscale { port } => Ok(format!(
            "set -eu; command -v tailscale >/dev/null; tailscale serve --https=443 http://127.0.0.1:{port}",
        )),
        RemoteAction::VerifyReceipt { release, port } => Ok(format!(
            "set -eu; test -x {}/dirtydash; curl --fail --silent --show-error --max-time 10 http://127.0.0.1:{port}/healthz >/dev/null",
            shell_quote(release),
        )),
        RemoteAction::Rollback { current, previous, database_path, database_backup } => {
            let database_restore = match (database_path, database_backup) {
                (Some(database), Some(backup)) => format!(
                    "if [ -f {backup} ]; then mv -f {backup} {database}; else rm -f {database}; fi; ",
                    database = shell_quote(database),
                    backup = shell_quote(backup),
                ),
                _ => String::new(),
            };
            let Some(previous) = previous else {
                return Ok(format!("set -eu; {database_restore}rm -f {}", shell_quote(current), database_restore = database_restore));
            };
            let temp = format!("{current}.rollback-{}", std::process::id());
            Ok(format!(
                "set -eu; {database_restore}rm -f {}; ln -s {} {}; mv -Tf {} {}",
                shell_quote(&temp),
                shell_quote(previous),
                shell_quote(&temp),
                shell_quote(&temp),
                shell_quote(current),
                database_restore = database_restore,
            ))
        }
        RemoteAction::Cleanup { release, remove_release, database_backup, temporary_seed } => {
            let backup_cleanup = database_backup
                .as_ref()
                .map(|backup| format!("rm -f {}; ", shell_quote(backup)))
                .unwrap_or_default();
            let seed_cleanup = temporary_seed
                .as_ref()
                .map(|seed| format!("rm -f {}; ", shell_quote(seed)))
                .unwrap_or_default();
            if *remove_release {
                Ok(format!("set -eu; {backup_cleanup}{seed_cleanup}rm -rf -- {}", shell_quote(release), backup_cleanup = backup_cleanup, seed_cleanup = seed_cleanup))
            } else {
                Ok(format!("set -eu; {backup_cleanup}{seed_cleanup}true", backup_cleanup = backup_cleanup, seed_cleanup = seed_cleanup))
            }
        }
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
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
            key_id: "fixture-key".to_string(),
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
        let mut runner = DeploymentRunner::new(fake);
        let request = DeploymentRequest::new("alias", "0.1.2-test", ListenerPlan::default());
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
        let receipt = runner
            .apply(
                &DeploymentRequest::new("manual", "0.1.2-test", ListenerPlan::default()),
                &artifact,
            )
            .unwrap();
        assert_eq!(receipt.status, "consent-required");
        assert_eq!(
            checkpoint.load().unwrap().unwrap().status,
            "consent-required"
        );
    }

    #[test]
    fn remote_probe_rejects_root_and_parses_platform() {
        let facts = RemoteFacts::parse_probe(
            "os=Darwin\narch=arm64\nuser=alice\nuid=501\nhome=/Users/alice\n",
        )
        .unwrap();
        assert_eq!(facts.platform.service_platform(), ServicePlatform::Launchd);
        assert!(
            RemoteFacts::parse_probe("os=Linux\narch=x86_64\nuser=root\nuid=0\nhome=/root\n")
                .is_err()
        );
    }

    #[test]
    fn ssh_actions_use_fixed_options_and_no_secret_arguments() {
        let command = action_command(&RemoteAction::HealthCheck { port: 4599 }).unwrap();
        assert!(command.contains("127.0.0.1:4599"));
        assert!(!command.contains("password"));
        assert!(!command.contains("secret"));
    }
}
