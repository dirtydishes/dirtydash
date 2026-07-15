use super::*;

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use axum::response::{IntoResponse, Response};
use axum::{
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};

impl HubError {
    pub(crate) fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub(crate) fn internal<E: std::fmt::Display>(_error: E) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal-error",
            "the Hub could not complete the request",
        )
    }

    pub(crate) fn unauthorized(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, message)
    }

    pub(crate) fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    pub(crate) fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    pub(crate) fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    pub(crate) fn unprocessable(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
    }
}

impl IntoResponse for HubError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

pub(crate) fn hash_password(password: &str) -> Result<String, HubError> {
    if password.len() < 8 {
        return Err(HubError::unprocessable(
            "weak-password",
            "password must be at least 8 characters long",
        ));
    }
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(HubError::internal)
}

pub(crate) fn verify_password(hash: &str, password: &str) -> Result<(), HubError> {
    let parsed = PasswordHash::new(hash).map_err(HubError::internal)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| HubError::unauthorized("owner-auth-required", "owner credentials are invalid"))
}

pub(crate) fn normalize_utc_timestamp(raw: &str) -> Result<String, HubError> {
    parse_utc_timestamp(raw).map(|timestamp| timestamp.to_rfc3339())
}

pub(crate) fn parse_utc_timestamp(raw: &str) -> Result<DateTime<Utc>, HubError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| {
            HubError::unprocessable(
                "invalid-timestamp",
                "timestamps must be RFC3339 values that normalize to UTC",
            )
        })
}

pub(crate) fn now_utc() -> String {
    Utc::now().to_rfc3339()
}

pub(crate) fn plus_seconds(timestamp: &str, seconds: i64) -> Result<String, HubError> {
    let current = parse_utc_timestamp(timestamp)?;
    Ok((current + chrono::TimeDelta::seconds(seconds)).to_rfc3339())
}

pub(crate) fn random_token(bytes: usize) -> String {
    let mut raw = vec![0_u8; bytes];
    rand::thread_rng().fill_bytes(&mut raw);
    hex::encode(raw)
}

pub(crate) fn sha256_hex(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

pub(crate) fn sha256_json<T: Serialize>(value: &T) -> Result<String, HubError> {
    let serialized = serde_json::to_vec(value).map_err(HubError::internal)?;
    Ok(hex::encode(Sha256::digest(serialized)))
}

pub(crate) fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?.to_str().ok().map(ToOwned::to_owned)
}
