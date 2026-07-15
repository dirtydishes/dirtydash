use super::*;

use axum::extract::connect_info::IntoMakeServiceWithConnectInfo;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::net::SocketAddr;

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
    Router::new()
        .route("/api/v1/admin/bootstrap", post(admin_bootstrap))
        .route("/api/v1/admin/session", get(admin_session))
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
        .route("/api/v1/ingest/batches", post(collector_ingest_batch))
        .with_state(HubState { repo, config })
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

async fn collector_ingest_batch(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(request): Json<IngestBatchRequest>,
) -> Result<Json<IngestBatchResponse>, HubError> {
    let auth = collector_auth(&state.repo, &headers)?;
    let response = state.repo.ingest_batch(&auth, request)?;
    Ok(Json(response))
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
