use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::db::Database;

mod auth;
mod errors;
mod ingestion;
mod protocol;
mod repository;
mod router;
#[cfg(test)]
mod tests;

pub use router::{build_router, build_router_with_config};

pub(crate) use errors::{
    hash_password, header_value, normalize_utc_timestamp, now_utc, parse_utc_timestamp,
    plus_seconds, random_token, sha256_hex, sha256_json, verify_password,
};
pub(crate) use ingestion::upsert_usage_event_tx;
pub(crate) use protocol::{
    validate_identifier, validate_ingest_batch, validate_non_empty, validate_tailscale_identity,
    validate_time_zone,
};

const OWNER_SESSION_COOKIE: &str = "dirtydash_owner_session";
const OWNER_CSRF_HEADER: &str = "x-csrf-token";
const BOOTSTRAP_SETUP_TOKEN_HEADER: &str = "x-dirtydash-setup-token";
const TAILSCALE_USER_LOGIN: &str = "tailscale-user-login";
const SUPPORTED_PROTOCOL_VERSION: u32 = 1;
const OWNER_SESSION_TTL_SECONDS: i64 = 60 * 60 * 12;
const DEFAULT_CREDENTIAL_LABEL: &str = "default";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerTrustMode {
    /// The listener is reachable only through the Hub's Tailscale Serve boundary.
    PrivateTailscale,
    /// No request header is an administrator identity proof.
    Public,
    /// A configured reverse proxy supplies an identity and an explicit provenance marker.
    TrustedProxy,
    /// Explicit local-only HTTP mode; this is the only mode allowed to omit `Secure`.
    LoopbackHttp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookieTransportSecurity {
    /// Use for HTTPS, Tailscale Serve, and public reverse-proxy deployments.
    Secure,
    /// The only mode that omits `Secure`, and only for explicit loopback HTTP.
    LoopbackHttp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapBoundary {
    /// Fresh-owner setup is disabled on this listener.
    Disabled,
    /// Fresh-owner setup is accepted only from a direct loopback transport.
    LoopbackOnly,
    /// Fresh-owner setup requires the explicitly configured setup token header.
    SetupToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailscaleOwnerMapping {
    pub owner_username: String,
    pub tailscale_identity: String,
}

impl TailscaleOwnerMapping {
    pub fn new(owner_username: impl Into<String>, tailscale_identity: impl Into<String>) -> Self {
        Self {
            owner_username: owner_username.into(),
            tailscale_identity: tailscale_identity.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedProxyConfig {
    pub identity_header: String,
    pub provenance_header: String,
    pub provenance_value: String,
}

impl TrustedProxyConfig {
    pub fn new(
        identity_header: impl Into<String>,
        provenance_header: impl Into<String>,
        provenance_value: impl Into<String>,
    ) -> Self {
        Self {
            identity_header: identity_header.into(),
            provenance_header: provenance_header.into(),
            provenance_value: provenance_value.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HubRouterConfig {
    trust_mode: ListenerTrustMode,
    cookie_transport: CookieTransportSecurity,
    tailscale_owner_mappings: Vec<TailscaleOwnerMapping>,
    trusted_proxy: Option<TrustedProxyConfig>,
    bootstrap_boundary: BootstrapBoundary,
    bootstrap_setup_token: Option<String>,
}

impl HubRouterConfig {
    pub fn for_listener(trust_mode: ListenerTrustMode) -> Self {
        Self {
            trust_mode,
            cookie_transport: if trust_mode == ListenerTrustMode::LoopbackHttp {
                CookieTransportSecurity::LoopbackHttp
            } else {
                CookieTransportSecurity::Secure
            },
            tailscale_owner_mappings: Vec::new(),
            trusted_proxy: None,
            bootstrap_boundary: match trust_mode {
                ListenerTrustMode::LoopbackHttp => BootstrapBoundary::LoopbackOnly,
                ListenerTrustMode::PrivateTailscale
                | ListenerTrustMode::Public
                | ListenerTrustMode::TrustedProxy => BootstrapBoundary::Disabled,
            },
            bootstrap_setup_token: None,
        }
    }

    pub fn with_bootstrap_boundary(mut self, bootstrap_boundary: BootstrapBoundary) -> Self {
        self.bootstrap_boundary = bootstrap_boundary;
        if bootstrap_boundary != BootstrapBoundary::SetupToken {
            self.bootstrap_setup_token = None;
        }
        self
    }

    pub fn with_bootstrap_setup_token(mut self, setup_token: impl Into<String>) -> Self {
        self.bootstrap_boundary = BootstrapBoundary::SetupToken;
        self.bootstrap_setup_token = Some(setup_token.into());
        self
    }

    pub fn with_cookie_transport(mut self, cookie_transport: CookieTransportSecurity) -> Self {
        self.cookie_transport = if cookie_transport == CookieTransportSecurity::LoopbackHttp
            && self.trust_mode != ListenerTrustMode::LoopbackHttp
        {
            CookieTransportSecurity::Secure
        } else {
            cookie_transport
        };
        self
    }

    pub fn with_tailscale_mapping(mut self, mapping: TailscaleOwnerMapping) -> Self {
        self.tailscale_owner_mappings.push(mapping);
        self
    }

    pub fn with_trusted_proxy(mut self, trusted_proxy: TrustedProxyConfig) -> Self {
        self.trusted_proxy = Some(trusted_proxy);
        self.trust_mode = ListenerTrustMode::TrustedProxy;
        self.cookie_transport = CookieTransportSecurity::Secure;
        self
    }

    pub fn from_config(trust_mode: ListenerTrustMode, config: &crate::config::HubConfig) -> Self {
        let mut router_config = Self::for_listener(trust_mode);
        router_config.cookie_transport = match (trust_mode, config.cookie_transport) {
            (
                ListenerTrustMode::LoopbackHttp,
                crate::config::CookieTransportConfig::LoopbackHttp,
            ) => CookieTransportSecurity::LoopbackHttp,
            _ => CookieTransportSecurity::Secure,
        };
        router_config.tailscale_owner_mappings = config
            .tailscale_owner_mappings
            .iter()
            .map(|mapping| {
                TailscaleOwnerMapping::new(
                    mapping.owner_username.clone(),
                    mapping.tailscale_identity.clone(),
                )
            })
            .collect();
        router_config.trusted_proxy = config.trusted_proxy.as_ref().map(|proxy| {
            TrustedProxyConfig::new(
                proxy.identity_header.clone(),
                proxy.provenance_header.clone(),
                proxy.provenance_value.clone(),
            )
        });
        if let Some(setup_token) = &config.bootstrap_setup_token {
            router_config = router_config.with_bootstrap_setup_token(setup_token.clone());
        }
        router_config
    }
}

#[derive(Debug, Clone)]
struct HubState {
    repo: HubRepository,
    config: HubRouterConfig,
}

#[derive(Debug, Clone)]
pub struct HubRepository {
    db: Database,
    write_guard: Arc<Mutex<()>>,
    #[cfg(test)]
    final_insert_failure: Arc<Mutex<bool>>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UsageDayBucket {
    pub day: String,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapOwnerRequest {
    pub username: String,
    pub password: String,
    pub time_zone: String,
    /// Optional persisted exact-match identity for Tailscale administrator login.
    #[serde(default, alias = "tailscale_user", alias = "tailscale_login")]
    pub tailscale_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerLoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotateCollectorCredentialRequest {
    pub machine_id: String,
    pub display_name: String,
    #[serde(default = "default_credential_label")]
    pub credential_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeCollectorCredentialRequest {
    pub machine_id: String,
    pub credential_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestBatchRequest {
    pub protocol_version: u32,
    pub batch_id: String,
    pub machine_id: String,
    pub sync_run: SyncRunInput,
    #[serde(default)]
    pub source_manifests: Vec<SourceManifestInput>,
    #[serde(default)]
    pub checkpoints: Vec<CheckpointInput>,
    pub events: Vec<CollectorUsageEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncRunInput {
    pub sync_run_id: String,
    pub collector_version: Option<String>,
    pub started_at: String,
    pub finished_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestInput {
    pub source_key: String,
    pub agent: String,
    pub display_path: String,
    pub item_count: u64,
    pub cursor: Option<String>,
    pub manifest_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointInput {
    pub agent: String,
    pub checkpoint_key: String,
    pub checkpoint_value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectorUsageEvent {
    pub agent: String,
    pub collector_event_fingerprint: String,
    pub occurred_at: String,
    pub session_key: String,
    pub project_key: String,
    pub source_key: String,
    pub turn_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
    pub confidence: f64,
    pub parser_name: String,
    pub parser_version: String,
    pub pricing_version: String,
    #[serde(default = "default_metadata_only_true")]
    pub metadata_only: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AdminSessionResponse {
    pub owner_username: String,
    pub time_zone: String,
    pub csrf_token: String,
    pub trusted_tailscale_user: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CurrentSessionResponse {
    pub authenticated: bool,
    pub owner_username: Option<String>,
    pub time_zone: Option<String>,
    pub trusted_tailscale_user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RotateCollectorCredentialResponse {
    pub machine_id: String,
    pub credential_id: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IngestBatchResponse {
    pub batch_id: String,
    pub inserted_events: u64,
    pub updated_events: u64,
    pub skipped_events: u64,
    pub idempotent_replay: bool,
    pub committed_at: String,
}

#[derive(Debug, Clone, Serialize)]
struct ErrorResponse {
    code: &'static str,
    message: String,
}

#[derive(Debug, Clone)]
pub(crate) struct HubError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

#[derive(Debug, Clone)]
struct OwnerRecord {
    owner_id: String,
    username: String,
    password_hash: String,
    time_zone: String,
}

#[derive(Debug, Clone)]
pub(crate) struct OwnerSessionRecord {
    session_id: String,
    owner_username: String,
    time_zone: String,
    trusted_tailscale_user: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct IssuedOwnerSession {
    session_id: String,
    owner_username: String,
    time_zone: String,
    csrf_token: String,
    trusted_tailscale_user: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct AuthenticatedCollector {
    machine_id: String,
    credential_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct IssuedCollectorCredential {
    machine_id: String,
    credential_id: String,
    token: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ValidatedIngestBatch {
    batch_id: String,
    machine_id: String,
    sync_run: ValidatedSyncRun,
    source_manifests: Vec<ValidatedSourceManifest>,
    checkpoints: Vec<ValidatedCheckpoint>,
    events: Vec<ValidatedCollectorUsageEvent>,
    request_fingerprint: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ValidatedSyncRun {
    sync_run_id: String,
    collector_version: Option<String>,
    started_at: String,
    finished_at: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ValidatedSourceManifest {
    source_key: String,
    agent: String,
    display_path: String,
    item_count: u64,
    cursor: Option<String>,
    manifest_fingerprint: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ValidatedCheckpoint {
    agent: String,
    checkpoint_key: String,
    checkpoint_value: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ValidatedCollectorUsageEvent {
    agent: String,
    collector_event_fingerprint: String,
    occurred_at: String,
    session_key: String,
    project_key: String,
    source_key: String,
    turn_id: Option<String>,
    provider: String,
    model: String,
    reasoning_effort: Option<String>,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    estimated_cost_usd: f64,
    confidence: f64,
    parser_name: String,
    parser_version: String,
    pricing_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsageEventWrite {
    Inserted,
    Updated,
    Skipped,
}

fn default_metadata_only_true() -> bool {
    true
}

fn default_credential_label() -> String {
    DEFAULT_CREDENTIAL_LABEL.to_string()
}
