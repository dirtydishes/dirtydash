use super::*;

use axum::body::Body;
use axum::extract::connect_info::IntoMakeServiceWithConnectInfo;
use axum::extract::{ConnectInfo, Path as AxumPath, Query, State};
use axum::http::{
    header, HeaderMap, HeaderName, HeaderValue, Response as HttpResponse, StatusCode, Uri,
};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use include_dir::{include_dir, Dir};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

use crate::deployment::SignedArtifactManifest;
use crate::enrollment::{
    AuthMethod, ConnectionSpec, EnrollmentDraft, EnrollmentSecrets, EnrollmentStore,
    EnrollmentWorkflow, HostTrustOutcome, KnownHostStore, PersistedAuthMethod,
    SshEnrollmentBackend,
};
use base64::Engine;
use std::path::PathBuf;

static DASHBOARD_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../dashboard/dist");

/// Backwards-compatible router builder for composition and in-process tests.
///
/// Production callers serving real sockets must use
/// [`build_router_with_config_and_connect_info`] so every request receives the
/// transport-authenticated peer address.
pub fn build_router(repo: HubRepository, trust_mode: ListenerTrustMode) -> Router {
    build_router_with_config(repo, HubRouterConfig::for_listener(trust_mode))
}

/// Build the production Hub service with transport-authenticated peer addresses.
///
/// The returned make-service is ready for `axum::serve`; using this seam makes
/// `ConnectInfo<SocketAddr>` available to every request without trusting request
/// headers for the peer identity.
pub fn build_router_with_config_and_connect_info(
    repo: HubRepository,
    config: HubRouterConfig,
) -> IntoMakeServiceWithConnectInfo<Router, SocketAddr> {
    build_router_with_config(repo, config).into_make_service_with_connect_info::<SocketAddr>()
}

pub fn build_router_with_config(repo: HubRepository, config: HubRouterConfig) -> Router {
    let db_path = repo.db_path();
    let root = db_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/v1/admin/bootstrap", post(admin_bootstrap))
        .route("/api/v1/admin/session", get(admin_session))
        .route("/api/v1/admin/session/csrf", get(admin_csrf))
        .route("/api/v1/admin/session/login", post(admin_login))
        .route(
            "/api/v1/admin/session/tailscale",
            post(admin_tailscale_login),
        )
        .route("/api/v1/admin/session/logout", post(admin_logout))
        .route(
            "/api/v1/admin/collector-credentials/rotate",
            post(admin_rotate_collector_credential),
        )
        .route(
            "/api/v1/admin/collector-credentials/revoke",
            post(admin_revoke_collector_credential),
        )
        .route(
            "/api/v1/admin/collector-commands",
            post(admin_issue_collector_command),
        )
        .route("/api/v1/admin/machines", get(admin_list_machines))
        .route("/api/v1/admin/machines/:machine_id", get(admin_get_machine))
        .route(
            "/api/v1/admin/machines/:machine_id/refresh",
            post(admin_refresh_machine),
        )
        .route(
            "/api/v1/admin/machines/:machine_id/repair",
            post(admin_repair_machine),
        )
        .route(
            "/api/v1/admin/machines/:machine_id/diagnostics",
            post(admin_diagnostics_machine),
        )
        .route(
            "/api/v1/admin/machines/:machine_id/rotate",
            post(admin_rotate_machine),
        )
        .route(
            "/api/v1/admin/machines/:machine_id/credentials/rotate",
            post(admin_rotate_machine),
        )
        .route(
            "/api/v1/admin/machines/:machine_id/archive",
            post(admin_archive_machine),
        )
        .route(
            "/api/v1/admin/machines/:machine_id/remove",
            post(admin_remove_machine),
        )
        .route(
            "/api/v1/admin/machines/:machine_id/delete",
            post(admin_delete_machine),
        )
        .route("/api/v1/admin/updates", get(admin_list_updates))
        .route("/api/v1/admin/updates/:update_id", get(admin_get_update))
        .route("/api/v1/admin/updates/plan", post(admin_plan_update))
        .route(
            "/api/v1/admin/updates/:update_id/snapshot",
            post(admin_snapshot_update),
        )
        .route(
            "/api/v1/admin/updates/:update_id/health",
            post(admin_health_update),
        )
        .route(
            "/api/v1/admin/updates/:update_id/collectors/:machine_id/start",
            post(admin_start_collector_update),
        )
        .route(
            "/api/v1/admin/updates/:update_id/collectors/:machine_id/complete",
            post(admin_complete_collector_update),
        )
        .route(
            "/api/v1/admin/enrollment",
            get(admin_list_enrollment).post(admin_create_enrollment),
        )
        .route(
            "/api/v1/admin/enrollments",
            get(admin_list_enrollment).post(admin_create_enrollment),
        )
        .route(
            "/api/v1/admin/enrollment/:enrollment_id",
            get(admin_get_enrollment),
        )
        .route(
            "/api/v1/admin/enrollments/:enrollment_id",
            get(admin_get_enrollment),
        )
        .route(
            "/api/v1/admin/enrollment/:enrollment_id/trust",
            post(admin_enrollment_trust),
        )
        .route(
            "/api/v1/admin/enrollment/:enrollment_id/probe",
            post(admin_enrollment_probe),
        )
        .route(
            "/api/v1/admin/enrollment/:enrollment_id/review",
            post(admin_enrollment_review),
        )
        .route(
            "/api/v1/admin/enrollment/:enrollment_id/execute",
            post(admin_enrollment_execute),
        )
        .route(
            "/api/v1/admin/enrollments/:enrollment_id/trust",
            post(admin_enrollment_trust),
        )
        .route(
            "/api/v1/admin/enrollments/:enrollment_id/probe",
            post(admin_enrollment_probe),
        )
        .route(
            "/api/v1/admin/enrollments/:enrollment_id/review",
            post(admin_enrollment_review),
        )
        .route(
            "/api/v1/admin/enrollments/:enrollment_id/execute",
            post(admin_enrollment_execute),
        )
        .route("/api/v1/collector/commands", get(collector_poll_command))
        .route(
            "/api/v1/collector/commands/ack",
            post(collector_ack_command),
        )
        .route(
            "/api/v1/collector/credentials/rotation/activate",
            post(collector_activate_credential_rotation),
        )
        .route(
            "/api/v1/collector/credentials/rotation/prove",
            post(collector_prove_credential_rotation),
        )
        .route("/api/v1/ingest/batches", post(collector_ingest_batch))
        .fallback(static_asset)
        .with_state(HubState {
            repo,
            config,
            enrollment_root: root.join("enrollments"),
            known_hosts_path: root.join("known_hosts"),
        })
}

async fn admin_bootstrap(
    State(state): State<HubState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Json(request): Json<BootstrapOwnerRequest>,
) -> Result<Response, HubError> {
    if !bootstrap_allowed(
        state.config.bootstrap_boundary,
        peer.map(|info| info.0),
        &headers,
        state.config.bootstrap_setup_token.as_deref(),
    ) {
        let (code, message) = match state.config.bootstrap_boundary {
            BootstrapBoundary::Disabled => (
                "bootstrap-setup-required",
                "fresh-owner setup is disabled on this listener; use the explicit setup boundary",
            ),
            BootstrapBoundary::LoopbackOnly => (
                "bootstrap-loopback-only",
                "fresh-owner setup is available only from the Hub loopback boundary",
            ),
            BootstrapBoundary::SetupToken => (
                "bootstrap-setup-required",
                "fresh-owner setup requires the configured setup-only token",
            ),
        };
        return Err(HubError::forbidden(code, message));
    }
    let session = state.repo.bootstrap_owner(request)?;
    Ok(session_response(session, state.config.cookie_transport))
}

async fn admin_login(
    State(state): State<HubState>,
    Json(request): Json<OwnerLoginRequest>,
) -> Result<Response, HubError> {
    let session = state.repo.login_owner(request)?;
    Ok(session_response(session, state.config.cookie_transport))
}

async fn admin_tailscale_login(
    State(state): State<HubState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> Result<Response, HubError> {
    let trusted_identity =
        trusted_tailscale_identity(peer.map(|info| info.0), &headers, &state.config)?.ok_or_else(
            || {
                HubError::unauthorized(
                    "trusted-tailscale-required",
                    "trusted Tailscale identity is required on this listener",
                )
            },
        )?;
    let session = state
        .repo
        .login_owner_via_tailscale(&trusted_identity, &state.config.tailscale_owner_mappings)?;
    Ok(session_response(session, state.config.cookie_transport))
}

async fn admin_session(
    State(state): State<HubState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> Result<Json<CurrentSessionResponse>, HubError> {
    let trusted_identity =
        trusted_tailscale_identity(peer.map(|info| info.0), &headers, &state.config)?;
    if let Some(session_id) = owner_session_cookie(&headers) {
        let session = state.repo.authenticate_owner_session(&session_id)?;
        return Ok(Json(CurrentSessionResponse {
            authenticated: true,
            owner_username: Some(session.owner_username),
            time_zone: Some(session.time_zone),
            trusted_tailscale_user: session.trusted_tailscale_user,
        }));
    }
    Ok(Json(CurrentSessionResponse {
        authenticated: false,
        owner_username: None,
        time_zone: None,
        trusted_tailscale_user: trusted_identity,
    }))
}

async fn admin_csrf(
    State(state): State<HubState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, HubError> {
    let session_id = owner_session_cookie(&headers).ok_or_else(|| {
        HubError::unauthorized(
            "owner-session-required",
            "a valid owner session is required",
        )
    })?;
    let token = state.repo.issue_owner_csrf(&session_id)?;
    Ok(Json(serde_json::json!({ "csrf_token": token })))
}

async fn admin_logout(
    State(state): State<HubState>,
    headers: HeaderMap,
) -> Result<Response, HubError> {
    let session = require_owner_session(&state, &headers, true)?;
    state.repo.logout_owner(&session.session_id)?;
    Ok(logout_response(state.config.cookie_transport))
}

async fn admin_rotate_collector_credential(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(request): Json<RotateCollectorCredentialRequest>,
) -> Result<Json<RotateCollectorCredentialResponse>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    let issued = state.repo.rotate_collector_credential(request)?;
    Ok(Json(RotateCollectorCredentialResponse {
        machine_id: issued.machine_id,
        credential_id: issued.credential_id,
        token: issued.token,
    }))
}

async fn admin_revoke_collector_credential(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(request): Json<RevokeCollectorCredentialRequest>,
) -> Result<StatusCode, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    state.repo.revoke_collector_credential(request)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn admin_issue_collector_command(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(request): Json<IssueCollectorCommandRequest>,
) -> Result<Json<IssueCollectorCommandResponse>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    Ok(Json(state.repo.issue_collector_command(request)?))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MachineActionRequest {
    pub expected_state_revision: i64,
}

async fn admin_list_machines(
    State(state): State<HubState>,
    headers: HeaderMap,
) -> Result<Json<Vec<MachineRecord>>, HubError> {
    let _session = require_owner_session(&state, &headers, false)?;
    Ok(Json(state.repo.list_machines()?))
}

async fn admin_get_machine(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(machine_id): AxumPath<String>,
) -> Result<Json<MachineRecord>, HubError> {
    let _session = require_owner_session(&state, &headers, false)?;
    Ok(Json(state.repo.machine(&machine_id)?))
}

async fn admin_machine_action(
    state: HubState,
    headers: HeaderMap,
    machine_id: String,
    request: MachineActionRequest,
    action: &str,
) -> Result<Json<MachineActionResponse>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    Ok(Json(state.repo.queue_machine_action(
        &machine_id,
        action,
        request.expected_state_revision,
    )?))
}

async fn admin_refresh_machine(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(machine_id): AxumPath<String>,
    Json(request): Json<MachineActionRequest>,
) -> Result<Json<MachineActionResponse>, HubError> {
    admin_machine_action(state, headers, machine_id, request, "refresh").await
}

async fn admin_repair_machine(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(machine_id): AxumPath<String>,
    Json(request): Json<MachineActionRequest>,
) -> Result<Json<MachineActionResponse>, HubError> {
    admin_machine_action(state, headers, machine_id, request, "repair").await
}

async fn admin_diagnostics_machine(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(machine_id): AxumPath<String>,
    Json(request): Json<MachineActionRequest>,
) -> Result<Json<MachineActionResponse>, HubError> {
    admin_machine_action(state, headers, machine_id, request, "diagnostics").await
}

async fn admin_rotate_machine(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(machine_id): AxumPath<String>,
    Json(request): Json<MachineActionRequest>,
) -> Result<Json<MachineActionResponse>, HubError> {
    admin_machine_action(state, headers, machine_id, request, "rotate").await
}

async fn admin_archive_machine(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(machine_id): AxumPath<String>,
    Json(request): Json<MachineLifecycleRequest>,
) -> Result<Json<MachineRecord>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    Ok(Json(state.repo.archive_machine(&machine_id, request)?))
}

async fn admin_remove_machine(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(machine_id): AxumPath<String>,
    Json(request): Json<MachineLifecycleRequest>,
) -> Result<Json<MachineRecord>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    Ok(Json(state.repo.remove_machine(&machine_id, request)?))
}

async fn admin_delete_machine(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(machine_id): AxumPath<String>,
    Json(request): Json<PermanentDeleteMachineRequest>,
) -> Result<StatusCode, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    state.repo.permanent_delete_machine(&machine_id, request)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn admin_list_updates(
    State(state): State<HubState>,
    headers: HeaderMap,
) -> Result<Json<Vec<FleetUpdateRun>>, HubError> {
    let _session = require_owner_session(&state, &headers, false)?;
    Ok(Json(state.repo.list_fleet_updates()?))
}

async fn admin_get_update(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(update_id): AxumPath<String>,
) -> Result<Json<FleetUpdateRun>, HubError> {
    let _session = require_owner_session(&state, &headers, false)?;
    Ok(Json(state.repo.fleet_update(&update_id)?))
}

fn require_signed_update_policy(
    config: &HubRouterConfig,
    request: &FleetUpdateRequest,
) -> Result<(), HubError> {
    let Some(policy) = &config.publisher_policy else {
        return Err(HubError::forbidden(
            "publisher-policy-required",
            "signed fleet updates require a configured publisher trust policy",
        ));
    };
    if policy.key_id() != request.publisher_key_id
        || !policy
            .fingerprint_value()
            .eq_ignore_ascii_case(&request.publisher_fingerprint)
    {
        return Err(HubError::forbidden(
            "signed-update-untrusted",
            "signed update evidence is not anchored to the configured publisher",
        ));
    }
    let signed = request.signed_manifest.as_ref().ok_or_else(|| {
        HubError::unprocessable(
            "signed-manifest-required",
            "fleet updates require the complete signed release manifest",
        )
    })?;
    let verified = policy.verify(signed).map_err(|_| {
        HubError::unprocessable(
            "signed-manifest-invalid",
            "release manifest signature verification failed",
        )
    })?;
    if signed.manifest.release != request.version
        || verified.manifest_sha256() != request.manifest_sha256
        || signed.key_id != request.publisher_key_id
        || !signed.manifest.artifacts.iter().any(|artifact| {
            artifact
                .sha256
                .eq_ignore_ascii_case(&request.artifact_sha256)
        })
    {
        return Err(HubError::unprocessable(
            "signed-update-mismatch",
            "signed release evidence does not match the requested update",
        ));
    }
    Ok(())
}

async fn admin_plan_update(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(request): Json<FleetUpdateRequest>,
) -> Result<Json<FleetUpdatePlanResponse>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    require_signed_update_policy(&state.config, &request)?;
    Ok(Json(state.repo.create_fleet_update(request)?))
}

async fn admin_snapshot_update(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(update_id): AxumPath<String>,
    Json(evidence): Json<FleetUpdateEvidence>,
) -> Result<Json<FleetUpdateSnapshotResponse>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    Ok(Json(state.repo.record_hub_snapshot(&update_id, &evidence)?))
}

async fn admin_health_update(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(update_id): AxumPath<String>,
    Json(request): Json<FleetHubHealthRequest>,
) -> Result<Json<FleetUpdateRun>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    Ok(Json(state.repo.record_hub_health(&update_id, request)?))
}

async fn admin_start_collector_update(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath((update_id, machine_id)): AxumPath<(String, String)>,
) -> Result<Json<MachineActionResponse>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    Ok(Json(
        state.repo.start_collector_update(&update_id, &machine_id)?,
    ))
}

async fn admin_complete_collector_update(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath((update_id, machine_id)): AxumPath<(String, String)>,
    Json(request): Json<FleetUpdateNodeCompletion>,
) -> Result<Json<FleetUpdateRun>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    Ok(Json(state.repo.complete_collector_update(
        &update_id,
        &machine_id,
        request,
    )?))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum HostedConnection {
    Alias {
        alias: String,
    },
    Manual {
        user: String,
        host: String,
        port: u16,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
enum HostedAuth {
    Password,
    KeyPath { path: PathBuf },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateEnrollmentRequest {
    pub id: String,
    pub machine_id: String,
    pub display_name: String,
    pub connection: HostedConnection,
    pub auth: HostedAuth,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HostedSecrets {
    password: Option<String>,
    key_passphrase: Option<String>,
    sudo_password: Option<String>,
}

impl HostedSecrets {
    fn materialize(&self) -> EnrollmentSecrets {
        let mut secrets = self
            .password
            .as_deref()
            .map(EnrollmentSecrets::password)
            .unwrap_or_else(EnrollmentSecrets::none);
        if let Some(passphrase) = &self.key_passphrase {
            secrets = secrets.with_key_passphrase(passphrase);
        }
        if let Some(password) = &self.sudo_password {
            secrets = secrets.with_sudo_password(password);
        }
        secrets
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentTrustRequest {
    #[serde(flatten)]
    secrets: HostedSecrets,
    confirm_fingerprint: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentStepRequest {
    #[serde(flatten)]
    secrets: HostedSecrets,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentArtifactRequest {
    signed_manifest: SignedArtifactManifest,
    artifact_base64: String,
    #[serde(default)]
    database_seed_base64: Option<String>,
}

fn enrollment_store(state: &HubState) -> EnrollmentStore {
    EnrollmentStore::new(state.enrollment_root.clone())
}

fn enrollment_auth(draft: &EnrollmentDraft) -> Result<AuthMethod, HubError> {
    match &draft.auth_method {
        PersistedAuthMethod::Password => Ok(AuthMethod::password()),
        PersistedAuthMethod::KeyPath { path } => AuthMethod::key_path(path.clone())
            .map_err(|error| HubError::unprocessable("invalid-enrollment-auth", error.to_string())),
    }
}

fn enrollment_workflow(
    state: &HubState,
    draft: &EnrollmentDraft,
) -> Result<EnrollmentWorkflow<SshEnrollmentBackend>, HubError> {
    let Some(policy) = state.config.publisher_policy.clone() else {
        return Err(HubError::forbidden(
            "publisher-policy-required",
            "hosted enrollment requires the Hub publisher trust policy",
        ));
    };
    let backend = SshEnrollmentBackend::new(
        draft.connection.display_endpoint(),
        state.known_hosts_path.clone(),
        policy.clone(),
    )
    .map_err(|error| HubError::unprocessable("invalid-enrollment-target", error.to_string()))?;
    Ok(EnrollmentWorkflow::new(
        enrollment_store(state),
        KnownHostStore::new(state.known_hosts_path.clone()),
        backend,
        policy,
    ))
}

fn enrollment_step_error(error: anyhow::Error) -> HubError {
    HubError::unprocessable("enrollment-step-failed", error.to_string())
}

fn verified_enrollment_artifact(
    state: &HubState,
    draft: &EnrollmentDraft,
    request: &EnrollmentArtifactRequest,
) -> Result<(crate::deployment::VerifiedArtifact, Option<Vec<u8>>), HubError> {
    let Some(policy) = state.config.publisher_policy.as_ref() else {
        return Err(HubError::forbidden(
            "publisher-policy-required",
            "hosted enrollment requires the Hub publisher trust policy",
        ));
    };
    let manifest = policy
        .verify(&request.signed_manifest)
        .map_err(|error| HubError::unprocessable("invalid-signed-artifact", error.to_string()))?;
    let facts = draft.facts.as_ref().ok_or_else(|| {
        HubError::conflict(
            "enrollment-order",
            "probe must complete before artifact review",
        )
    })?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&request.artifact_base64)
        .map_err(|_| HubError::unprocessable("invalid-artifact", "artifact_base64 is invalid"))?;
    let artifact = manifest
        .verify_artifact(facts.platform, bytes)
        .map_err(|error| HubError::unprocessable("invalid-signed-artifact", error.to_string()))?;
    let seed = request
        .database_seed_base64
        .as_deref()
        .map(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| {
                    HubError::unprocessable(
                        "invalid-database-seed",
                        "database_seed_base64 is invalid",
                    )
                })
        })
        .transpose()?;
    Ok((artifact, seed))
}

async fn admin_list_enrollment(
    State(state): State<HubState>,
    headers: HeaderMap,
) -> Result<Json<Vec<EnrollmentDraft>>, HubError> {
    let _session = require_owner_session(&state, &headers, false)?;
    enrollment_store(&state)
        .list()
        .map(Json)
        .map_err(|error| HubError::internal(error.to_string()))
}

async fn admin_create_enrollment(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(request): Json<CreateEnrollmentRequest>,
) -> Result<Json<EnrollmentDraft>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    validate_identifier(&request.machine_id, "machine_id")?;
    validate_non_empty(&request.display_name, "display_name")?;
    let connection = match request.connection {
        HostedConnection::Alias { alias } => ConnectionSpec::alias(alias),
        HostedConnection::Manual { user, host, port } => ConnectionSpec::manual(user, host, port),
    }
    .map_err(|error| HubError::unprocessable("invalid-enrollment-target", error.to_string()))?;
    let auth = match request.auth {
        HostedAuth::Password => AuthMethod::password(),
        HostedAuth::KeyPath { path } => AuthMethod::key_path(path).map_err(|error| {
            HubError::unprocessable("invalid-enrollment-auth", error.to_string())
        })?,
    };
    let mut draft = EnrollmentDraft::new(request.id, connection, auth)
        .map_err(|error| HubError::unprocessable("invalid-enrollment-id", error.to_string()))?;
    draft.machine_id = Some(request.machine_id);
    draft.display_name = Some(request.display_name);
    enrollment_store(&state)
        .save(&draft)
        .map_err(|error| HubError::internal(error.to_string()))?;
    Ok(Json(draft))
}

async fn admin_get_enrollment(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(enrollment_id): AxumPath<String>,
) -> Result<Json<EnrollmentDraft>, HubError> {
    let _session = require_owner_session(&state, &headers, false)?;
    enrollment_store(&state)
        .load(&enrollment_id)
        .map(Json)
        .map_err(|error| HubError::not_found("enrollment-not-found", error.to_string()))
}

async fn admin_enrollment_trust(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(enrollment_id): AxumPath<String>,
    Json(request): Json<EnrollmentTrustRequest>,
) -> Result<Json<HostTrustOutcome>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    let draft = enrollment_store(&state)
        .load(&enrollment_id)
        .map_err(|error| HubError::not_found("enrollment-not-found", error.to_string()))?;
    let auth = enrollment_auth(&draft)?;
    let mut workflow = enrollment_workflow(&state, &draft)?;
    let secrets = request.secrets.materialize();
    workflow
        .trust_and_auth(
            &enrollment_id,
            &auth,
            &secrets,
            request.confirm_fingerprint.as_deref(),
        )
        .map(Json)
        .map_err(enrollment_step_error)
}

async fn admin_enrollment_probe(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(enrollment_id): AxumPath<String>,
    Json(request): Json<EnrollmentStepRequest>,
) -> Result<Json<crate::deployment::DeploymentPlan>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    let draft = enrollment_store(&state)
        .load(&enrollment_id)
        .map_err(|error| HubError::not_found("enrollment-not-found", error.to_string()))?;
    let auth = enrollment_auth(&draft)?;
    let mut workflow = enrollment_workflow(&state, &draft)?;
    let secrets = request.secrets.materialize();
    workflow
        .probe_and_plan(&enrollment_id, &auth, &secrets)
        .map(Json)
        .map_err(enrollment_step_error)
}

async fn admin_enrollment_review(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(enrollment_id): AxumPath<String>,
    Json(request): Json<EnrollmentArtifactRequest>,
) -> Result<Json<EnrollmentDraft>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    let draft = enrollment_store(&state)
        .load(&enrollment_id)
        .map_err(|error| HubError::not_found("enrollment-not-found", error.to_string()))?;
    let plan = draft.plan.clone().ok_or_else(|| {
        HubError::conflict("enrollment-order", "probe must complete before review")
    })?;
    let (artifact, seed) = verified_enrollment_artifact(&state, &draft, &request)?;
    let mut workflow = enrollment_workflow(&state, &draft)?;
    workflow
        .review_with_artifact(&enrollment_id, &plan, &artifact, seed.as_deref())
        .map_err(enrollment_step_error)?;
    enrollment_store(&state)
        .load(&enrollment_id)
        .map(Json)
        .map_err(|error| HubError::internal(error.to_string()))
}

async fn admin_enrollment_execute(
    State(state): State<HubState>,
    headers: HeaderMap,
    AxumPath(enrollment_id): AxumPath<String>,
    Json(request): Json<HostedExecuteEnrollmentRequest>,
) -> Result<Json<EnrollmentDraft>, HubError> {
    let _session = require_owner_session(&state, &headers, true)?;
    let draft = enrollment_store(&state)
        .load(&enrollment_id)
        .map_err(|error| HubError::not_found("enrollment-not-found", error.to_string()))?;
    let plan = draft.plan.clone().ok_or_else(|| {
        HubError::conflict("enrollment-order", "review must complete before execute")
    })?;
    let (artifact, seed) = verified_enrollment_artifact(&state, &draft, &request.artifact)?;
    let listener = plan.listener().clone();
    let mut workflow = enrollment_workflow(&state, &draft)?;
    let secrets = request.secrets.materialize();
    workflow
        .execute(
            &enrollment_id,
            &plan,
            &artifact,
            seed.as_deref(),
            &listener,
            &secrets,
        )
        .map_err(enrollment_step_error)?;
    let completed = enrollment_store(&state)
        .load(&enrollment_id)
        .map_err(|error| HubError::internal(error.to_string()))?;
    if let (Some(machine_id), Some(display_name)) = (
        completed.machine_id.as_deref(),
        completed.display_name.as_deref(),
    ) {
        state.repo.ensure_machine(machine_id, display_name)?;
    }
    Ok(Json(completed))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostedExecuteEnrollmentRequest {
    artifact: EnrollmentArtifactRequest,
    #[serde(flatten)]
    secrets: HostedSecrets,
}

async fn collector_poll_command(
    State(state): State<HubState>,
    headers: HeaderMap,
    Query(query): Query<CollectorCommandPollQuery>,
) -> Result<Json<CollectorCommandPollResponse>, HubError> {
    let auth = collector_auth(&state.repo, &headers)?;
    if let Some(command) = state.repo.poll_collector_command(&auth)? {
        return Ok(Json(CollectorCommandPollResponse {
            command: Some(command),
        }));
    }
    let wait_seconds = query.wait_seconds.unwrap_or(20).min(20);
    if wait_seconds > 0 {
        let notification = state.repo.command_notification();
        tokio::select! {
            _ = notification.notified() => {},
            _ = tokio::time::sleep(std::time::Duration::from_secs(wait_seconds)) => {},
        }
    }
    Ok(Json(CollectorCommandPollResponse {
        command: state.repo.poll_collector_command(&auth)?,
    }))
}

async fn collector_ack_command(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(request): Json<CollectorCommandAckRequest>,
) -> Result<StatusCode, HubError> {
    let auth = collector_auth(&state.repo, &headers)?;
    state.repo.acknowledge_collector_command(&auth, request)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn collector_activate_credential_rotation(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(request): Json<CollectorCredentialRotationActivationRequest>,
) -> Result<Json<CollectorCredentialRotationResponse>, HubError> {
    let auth = collector_auth(&state.repo, &headers)?;
    Ok(Json(
        state
            .repo
            .activate_collector_credential_rotation(&auth, request)?,
    ))
}

async fn collector_prove_credential_rotation(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(request): Json<CollectorCredentialRotationProofRequest>,
) -> Result<Json<CollectorCredentialRotationResponse>, HubError> {
    let auth = collector_auth(&state.repo, &headers)?;
    Ok(Json(
        state
            .repo
            .prove_collector_credential_rotation(&auth, request)?,
    ))
}

async fn collector_ingest_batch(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(request): Json<IngestBatchRequest>,
) -> Result<Json<IngestBatchResponse>, HubError> {
    let auth = collector_auth(&state.repo, &headers)?;
    let response = state.repo.ingest_batch(&auth, request)?;
    Ok(Json(response))
}

async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok", "service": "dirtydash-hub"}))
}

async fn static_asset(uri: Uri) -> HttpResponse<Body> {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let file = DASHBOARD_DIR
        .get_file(path)
        .or_else(|| DASHBOARD_DIR.get_file("index.html"));
    match file {
        Some(file) => {
            let mime = mime_guess::from_path(file.path()).first_or_octet_stream();
            HttpResponse::builder()
                .status(StatusCode::OK)
                .header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_str(mime.as_ref())
                        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
                )
                .body(Body::from(file.contents().to_vec()))
                .unwrap_or_else(|_| HttpResponse::new(Body::empty()))
        }
        None => HttpResponse::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("not found"))
            .unwrap_or_else(|_| HttpResponse::new(Body::empty())),
    }
}

fn bootstrap_allowed(
    boundary: BootstrapBoundary,
    peer: Option<SocketAddr>,
    headers: &HeaderMap,
    setup_token: Option<&str>,
) -> bool {
    match boundary {
        BootstrapBoundary::Disabled => false,
        BootstrapBoundary::LoopbackOnly => peer.is_some_and(|peer| peer.ip().is_loopback()),
        BootstrapBoundary::SetupToken => {
            let Some(setup_token) = setup_token else {
                return false;
            };
            exact_header_value(headers, BOOTSTRAP_SETUP_TOKEN_HEADER)
                .ok()
                .flatten()
                .as_deref()
                == Some(setup_token)
        }
    }
}

fn require_owner_session(
    state: &HubState,
    headers: &HeaderMap,
    require_csrf: bool,
) -> Result<OwnerSessionRecord, HubError> {
    let session_id = owner_session_cookie(headers).ok_or_else(|| {
        HubError::unauthorized(
            "owner-session-required",
            "a valid owner session is required",
        )
    })?;
    let session = state.repo.authenticate_owner_session(&session_id)?;
    if require_csrf {
        let csrf = header_value(headers, OWNER_CSRF_HEADER).ok_or_else(|| {
            HubError::forbidden(
                "csrf-mismatch",
                "state-changing admin requests require a matching CSRF token",
            )
        })?;
        state.repo.verify_owner_csrf(&session.session_id, &csrf)?;
    }
    Ok(session)
}

fn collector_auth(
    repo: &HubRepository,
    headers: &HeaderMap,
) -> Result<AuthenticatedCollector, HubError> {
    let auth_header = header_value(headers, header::AUTHORIZATION.as_str()).ok_or_else(|| {
        HubError::unauthorized(
            "collector-auth-required",
            "collector bearer authentication is required",
        )
    })?;
    let bearer = auth_header.strip_prefix("Bearer ").ok_or_else(|| {
        HubError::unauthorized(
            "collector-auth-required",
            "collector bearer authentication is required",
        )
    })?;
    repo.authenticate_collector_bearer(bearer)
}

fn session_response(session: IssuedOwnerSession, transport: CookieTransportSecurity) -> Response {
    let body = Json(AdminSessionResponse {
        owner_username: session.owner_username,
        time_zone: session.time_zone,
        csrf_token: session.csrf_token,
        trusted_tailscale_user: session.trusted_tailscale_user,
    });
    let mut response = body.into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        session_cookie_header(&session.session_id, transport),
    );
    response
}

fn logout_response(transport: CookieTransportSecurity) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let secure = secure_cookie_suffix(transport);
    let value =
        format!("{OWNER_SESSION_COOKIE}=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0{secure}");
    if let Ok(value) = HeaderValue::from_str(&value) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

fn session_cookie_header(session_id: &str, transport: CookieTransportSecurity) -> HeaderValue {
    let secure = secure_cookie_suffix(transport);
    HeaderValue::from_str(&format!(
        "{OWNER_SESSION_COOKIE}={session_id}; HttpOnly; Path=/; SameSite=Lax{secure}"
    ))
    .unwrap_or_else(|_| {
        HeaderValue::from_static(
            "dirtydash_owner_session=invalid; HttpOnly; Path=/; SameSite=Lax; Secure",
        )
    })
}

fn secure_cookie_suffix(transport: CookieTransportSecurity) -> &'static str {
    match transport {
        CookieTransportSecurity::Secure => "; Secure",
        CookieTransportSecurity::LoopbackHttp => "",
    }
}

fn owner_session_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = header_value(headers, header::COOKIE.as_str())?;
    raw.split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(&format!("{OWNER_SESSION_COOKIE}=")))
        .map(ToOwned::to_owned)
}

fn trusted_tailscale_identity(
    peer: Option<SocketAddr>,
    headers: &HeaderMap,
    config: &HubRouterConfig,
) -> Result<Option<String>, HubError> {
    match config.trust_mode {
        ListenerTrustMode::Public | ListenerTrustMode::LoopbackHttp => Ok(None),
        ListenerTrustMode::PrivateTailscale => {
            // Production Hub sockets are loopback-only in private mode; the
            // Tailscale Serve process is the transport-authenticated peer.
            // Keep a missing peer usable for the in-process router seam, but
            // never accept a forged header from a concrete non-loopback peer.
            if peer.is_some_and(|peer| !peer.ip().is_loopback()) {
                return Ok(None);
            }
            let Some(login) = exact_header_value(headers, TAILSCALE_USER_LOGIN)? else {
                return Ok(None);
            };
            Ok(Some(validate_tailscale_identity(&login)?))
        }
        ListenerTrustMode::TrustedProxy => {
            let Some(proxy) = &config.trusted_proxy else {
                return Err(HubError::internal(
                    "trusted proxy provenance is not configured",
                ));
            };
            let Some(peer) = peer else {
                return Err(HubError::unauthorized(
                    "trusted-proxy-peer-required",
                    "trusted proxy identity requires transport connection information",
                ));
            };
            if !proxy.trusts_peer(peer) {
                return Err(HubError::unauthorized(
                    "trusted-proxy-peer-untrusted",
                    "the transport peer is not an approved trusted proxy source",
                ));
            }
            let provenance = exact_header_value(headers, &proxy.provenance_header)?;
            if provenance.as_deref() != Some(proxy.provenance_value.as_str()) {
                return Ok(None);
            }
            exact_header_value(headers, &proxy.identity_header)?
                .map(|identity| validate_tailscale_identity(&identity))
                .transpose()
        }
    }
}

fn exact_header_value(headers: &HeaderMap, name: &str) -> Result<Option<String>, HubError> {
    let name = name
        .parse::<HeaderName>()
        .map_err(|_| HubError::internal("trusted identity header configuration is invalid"))?;
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    let first = first.to_str().map_err(|_| {
        HubError::unauthorized(
            "trusted-tailscale-invalid",
            "trusted identity headers must contain valid text",
        )
    })?;
    for value in values {
        let value = value.to_str().map_err(|_| {
            HubError::unauthorized(
                "trusted-tailscale-invalid",
                "trusted identity headers must contain valid text",
            )
        })?;
        if value != first {
            return Err(HubError::unauthorized(
                "trusted-tailscale-mismatch",
                "duplicate trusted identity headers do not agree",
            ));
        }
    }
    Ok(Some(first.to_string()))
}
