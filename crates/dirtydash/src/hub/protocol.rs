use super::*;
use chrono_tz::Tz;
use std::collections::BTreeSet;
use std::path::Path;

pub(crate) fn canonical_event_identity(agent: &str, collector_event_fingerprint: &str) -> String {
    format!("{agent}:{collector_event_fingerprint}")
}

pub(crate) fn validate_ingest_batch(
    request: IngestBatchRequest,
    authenticated_machine_id: &str,
) -> Result<ValidatedIngestBatch, HubError> {
    if !SUPPORTED_PROTOCOL_VERSIONS.contains(&request.protocol_version) {
        return Err(HubError::conflict(
            "incompatible-protocol-version",
            "this Hub accepts only the current and previous Collector protocol versions",
        ));
    }
    let machine_id = validate_identifier(&request.machine_id, "machine_id")?;
    if machine_id != authenticated_machine_id {
        return Err(HubError::unauthorized(
            "collector-machine-mismatch",
            "collector credential is not authorized for the requested machine_id",
        ));
    }
    let batch_id = validate_identifier(&request.batch_id, "batch_id")?;
    let request_fingerprint = sha256_json(&request)?;
    let sync_run = ValidatedSyncRun {
        sync_run_id: validate_identifier(&request.sync_run.sync_run_id, "sync_run_id")?,
        collector_version: request
            .sync_run
            .collector_version
            .as_deref()
            .map(|value| validate_non_empty(value, "collector_version"))
            .transpose()?,
        started_at: normalize_utc_timestamp(&request.sync_run.started_at)?,
        finished_at: normalize_utc_timestamp(&request.sync_run.finished_at)?,
    };

    let mut manifest_keys = BTreeSet::new();
    let mut source_manifests = Vec::with_capacity(request.source_manifests.len());
    for manifest in request.source_manifests {
        let source_key = validate_display_safe_key(&manifest.source_key, "source_key")?;
        if !manifest_keys.insert(source_key.clone()) {
            return Err(HubError::unprocessable(
                "duplicate-source-manifest",
                "source_manifests must not repeat source_key within a batch",
            ));
        }
        source_manifests.push(ValidatedSourceManifest {
            source_key,
            agent: validate_identifier(&manifest.agent, "agent")?,
            display_path: validate_display_safe_key(&manifest.display_path, "display_path")?,
            item_count: manifest.item_count,
            cursor: manifest
                .cursor
                .as_deref()
                .map(|value| validate_display_safe_key(value, "cursor"))
                .transpose()?,
            manifest_fingerprint: validate_identifier(
                &manifest.manifest_fingerprint,
                "manifest_fingerprint",
            )?,
        });
    }

    let mut checkpoint_keys = BTreeSet::new();
    let mut checkpoints = Vec::with_capacity(request.checkpoints.len());
    for checkpoint in request.checkpoints {
        let agent = validate_identifier(&checkpoint.agent, "checkpoint agent")?;
        let checkpoint_key = validate_identifier(&checkpoint.checkpoint_key, "checkpoint_key")?;
        if !checkpoint_keys.insert(format!("{agent}:{checkpoint_key}")) {
            return Err(HubError::unprocessable(
                "duplicate-checkpoint",
                "checkpoints must be unique per agent and checkpoint_key within a batch",
            ));
        }
        checkpoints.push(ValidatedCheckpoint {
            agent,
            checkpoint_key,
            checkpoint_value: validate_display_safe_key(
                &checkpoint.checkpoint_value,
                "checkpoint_value",
            )?,
        });
    }

    let mut event_identities = BTreeSet::new();
    let mut events = Vec::with_capacity(request.events.len());
    for event in request.events {
        if !event.metadata_only {
            return Err(HubError::unprocessable(
                "metadata-only-required",
                "metadata_only must stay true for /api/v1 ingestion",
            ));
        }
        let agent = validate_identifier(&event.agent, "agent")?;
        let collector_event_fingerprint = validate_identifier(
            &event.collector_event_fingerprint,
            "collector_event_fingerprint",
        )?;
        if !event_identities.insert(canonical_event_identity(
            &agent,
            &collector_event_fingerprint,
        )) {
            return Err(HubError::unprocessable(
                "duplicate-event-identity",
                "events must be unique by agent and collector_event_fingerprint within a batch",
            ));
        }
        events.push(ValidatedCollectorUsageEvent {
            agent,
            collector_event_fingerprint,
            occurred_at: normalize_utc_timestamp(&event.occurred_at)?,
            session_key: validate_display_safe_key(&event.session_key, "session_key")?,
            project_key: validate_display_safe_key(&event.project_key, "project_key")?,
            source_key: validate_display_safe_key(&event.source_key, "source_key")?,
            turn_id: event
                .turn_id
                .as_deref()
                .map(|value| validate_identifier(value, "turn_id"))
                .transpose()?,
            provider: validate_identifier(&event.provider, "provider")?,
            model: validate_non_empty(&event.model, "model")?,
            reasoning_effort: event
                .reasoning_effort
                .as_deref()
                .map(|value| validate_identifier(value, "reasoning_effort"))
                .transpose()?,
            prompt_tokens: event.prompt_tokens,
            completion_tokens: event.completion_tokens,
            cache_read_tokens: event.cache_read_tokens,
            cache_write_tokens: event.cache_write_tokens,
            reasoning_tokens: event.reasoning_tokens,
            total_tokens: event.total_tokens,
            estimated_cost_usd: event.estimated_cost_usd,
            confidence: event.confidence,
            parser_name: validate_identifier(&event.parser_name, "parser_name")?,
            parser_version: validate_identifier(&event.parser_version, "parser_version")?,
            pricing_version: validate_identifier(&event.pricing_version, "pricing_version")?,
            pricing_mode: event.pricing_mode,
        });
    }

    Ok(ValidatedIngestBatch {
        batch_id,
        machine_id,
        protocol_version: request.protocol_version,
        sync_run,
        source_manifests,
        checkpoints,
        events,
        request_fingerprint,
    })
}

pub(crate) fn validate_time_zone(time_zone: &str) -> Result<String, HubError> {
    let time_zone = validate_non_empty(time_zone, "time_zone")?;
    time_zone.parse::<Tz>().map_err(|_| {
        HubError::unprocessable(
            "invalid-time-zone",
            "time_zone must be a valid IANA time zone",
        )
    })?;
    Ok(time_zone)
}

pub(crate) fn validate_identifier(value: &str, field: &str) -> Result<String, HubError> {
    let value = validate_non_empty(value, field)?;
    if value.len() > 200 {
        return Err(HubError::unprocessable(
            "invalid-identifier",
            format!("{field} is too long"),
        ));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '@'))
    {
        return Err(HubError::unprocessable(
            "invalid-identifier",
            format!("{field} contains unsupported characters"),
        ));
    }
    Ok(value)
}

pub(crate) fn validate_non_empty(value: &str, field: &str) -> Result<String, HubError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(HubError::unprocessable(
            "missing-field",
            format!("{field} must not be empty"),
        ));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn validate_command_has_no_secret(value: &serde_json::Value) -> Result<(), HubError> {
    if command_value_contains_secret(value, false) {
        return Err(HubError::unprocessable(
            "collector-command-secret-forbidden",
            "collector commands must contain only non-secret rotation instructions",
        ));
    }
    Ok(())
}

pub(crate) fn validate_ack_result_has_no_secret(value: &serde_json::Value) -> Result<(), HubError> {
    if command_value_contains_secret(value, true) {
        return Err(HubError::unprocessable(
            "collector-command-secret-forbidden",
            "collector command acknowledgements must not contain credentials",
        ));
    }
    Ok(())
}

fn command_value_contains_secret(value: &serde_json::Value, inspect_values: bool) -> bool {
    match value {
        serde_json::Value::Object(object) => object.iter().any(|(key, value)| {
            let normalized = key.to_ascii_lowercase();
            normalized.contains("token")
                || normalized.contains("secret")
                || normalized == "password"
                || command_value_contains_secret(value, inspect_values)
        }),
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| command_value_contains_secret(value, inspect_values)),
        serde_json::Value::String(value) => {
            let normalized = value.to_ascii_lowercase();
            normalized.starts_with("ddcol_")
                || (inspect_values
                    && (normalized.contains("secret")
                        || normalized.contains("token")
                        || normalized.contains("sentinel")))
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            false
        }
    }
}

pub(crate) fn validate_display_safe_key(value: &str, field: &str) -> Result<String, HubError> {
    if value != value.trim() {
        return Err(HubError::unprocessable(
            "invalid-display-identifier",
            format!("{field} must not contain leading or trailing whitespace"),
        ));
    }
    let value = validate_non_empty(value, field)?;
    if value.len() > 200 {
        return Err(HubError::unprocessable(
            "invalid-display-identifier",
            format!("{field} is too long"),
        ));
    }
    if looks_like_absolute_path(&value) {
        return Err(HubError::unprocessable(
            "absolute-path-forbidden",
            format!("{field} must be a redacted display-safe identifier, not an absolute path"),
        ));
    }
    if value
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(HubError::unprocessable(
            "invalid-display-identifier",
            format!("{field} must not contain whitespace or control characters"),
        ));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '@' | '/'))
    {
        return Err(HubError::unprocessable(
            "invalid-display-identifier",
            format!("{field} contains unsupported display content"),
        ));
    }
    Ok(value)
}

fn looks_like_absolute_path(value: &str) -> bool {
    if value.starts_with('/') || value.starts_with('~') {
        return true;
    }
    if value.starts_with("\\\\") {
        return true;
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return true;
    }
    let bytes = value.as_bytes();
    bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'/' || bytes[2] == b'\\')
}

pub(crate) fn validate_tailscale_identity(value: &str) -> Result<String, HubError> {
    let validated = validate_identifier(value, "tailscale_identity")?;
    if validated != value {
        return Err(HubError::unprocessable(
            "invalid-tailscale-identity",
            "Tailscale identity matching requires an exact header value",
        ));
    }
    Ok(validated)
}
