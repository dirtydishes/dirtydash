use super::*;
use rusqlite::{params, OptionalExtension, Transaction};
pub(crate) fn upsert_usage_event_tx(
    tx: &Transaction<'_>,
    machine_id: &str,
    batch_id: &str,
    event: &ValidatedCollectorUsageEvent,
    imported_at: &str,
) -> Result<UsageEventWrite, HubError> {
    let raw_event_hash = sha256_hex(&format!(
        "{machine_id}\n{}\n{}",
        event.agent, event.collector_event_fingerprint
    ));
    let existing = tx
        .query_row(
            r#"
            SELECT provider, model, turn_id, reasoning_effort, prompt_tokens, completion_tokens,
                cache_read_tokens, cache_write_tokens, reasoning_tokens, total_tokens,
                estimated_cost_usd, confidence, pricing_version, pricing_mode, event_timestamp,
                project_path, session_id, raw_path, parser_name, parser_version
            FROM usage_events
            WHERE machine_id = ?1
                AND agent = ?2
                AND collector_event_fingerprint = ?3
            "#,
            params![machine_id, event.agent, event.collector_event_fingerprint],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)? as u64,
                    row.get::<_, i64>(5)? as u64,
                    row.get::<_, i64>(6)? as u64,
                    row.get::<_, i64>(7)? as u64,
                    row.get::<_, i64>(8)? as u64,
                    row.get::<_, i64>(9)? as u64,
                    row.get::<_, f64>(10)?,
                    row.get::<_, f64>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, String>(17)?,
                    row.get::<_, String>(18)?,
                    row.get::<_, String>(19)?,
                ))
            },
        )
        .optional()
        .map_err(HubError::internal)?;
    if let Some(existing) = existing {
        let matches = existing.0 == event.provider
            && existing.1 == event.model
            && existing.2 == event.turn_id
            && existing.3 == event.reasoning_effort
            && existing.4 == event.prompt_tokens
            && existing.5 == event.completion_tokens
            && existing.6 == event.cache_read_tokens
            && existing.7 == event.cache_write_tokens
            && existing.8 == event.reasoning_tokens
            && existing.9 == event.total_tokens
            && (existing.10 - event.estimated_cost_usd).abs() < 0.0000001
            && (existing.11 - event.confidence).abs() < 0.0000001
            && existing.12 == event.pricing_version
            && existing.13 == event.pricing_mode.as_str()
            && existing.14.as_deref() == Some(event.occurred_at.as_str())
            && existing.15 == event.project_key
            && existing.16 == event.session_key
            && existing.17 == event.source_key
            && existing.18 == event.parser_name
            && existing.19 == event.parser_version;
        if matches {
            return Ok(UsageEventWrite::Skipped);
        }
        tx.execute(
            r#"
            UPDATE usage_events
            SET provider = ?4,
                model = ?5,
                turn_id = ?6,
                reasoning_effort = ?7,
                prompt_tokens = ?8,
                completion_tokens = ?9,
                cache_read_tokens = ?10,
                cache_write_tokens = ?11,
                reasoning_tokens = ?12,
                total_tokens = ?13,
                estimated_cost_usd = ?14,
                confidence = ?15,
                event_timestamp = ?16,
                project_path = ?17,
                session_id = ?18,
                raw_path = ?19,
                parser_name = ?20,
                parser_version = ?21,
                imported_at = ?22,
                pricing_version = ?23,
                pricing_mode = ?24,
                metadata_only = 1,
                raw_event_hash = ?25,
                machine = ?1,
                source = ?2,
                ingest_batch_id = ?3
            WHERE machine_id = ?1
                AND agent = ?2
                AND collector_event_fingerprint = ?26
            "#,
            params![
                machine_id,
                event.agent,
                batch_id,
                event.provider,
                event.model,
                event.turn_id,
                event.reasoning_effort,
                event.prompt_tokens,
                event.completion_tokens,
                event.cache_read_tokens,
                event.cache_write_tokens,
                event.reasoning_tokens,
                event.total_tokens,
                event.estimated_cost_usd,
                event.confidence,
                event.occurred_at,
                event.project_key,
                event.session_key,
                event.source_key,
                event.parser_name,
                event.parser_version,
                imported_at,
                event.pricing_version,
                event.pricing_mode.as_str(),
                raw_event_hash,
                event.collector_event_fingerprint,
            ],
        )
        .map_err(HubError::internal)?;
        return Ok(UsageEventWrite::Updated);
    }

    tx.execute(
        r#"
        INSERT INTO usage_events(
            machine, source, project_path, session_id, turn_id, provider, model,
            reasoning_effort, prompt_tokens, completion_tokens, cache_read_tokens,
            cache_write_tokens, reasoning_tokens, total_tokens, estimated_cost_usd,
            confidence, event_timestamp, raw_path, raw_span, parser_name, parser_version,
            raw_event_hash, imported_at, pricing_version, pricing_mode, metadata_only,
            machine_id, agent, collector_event_fingerprint, ingest_batch_id
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7,
            ?8, ?9, ?10, ?11,
            ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, NULL, ?19, ?20,
            ?21, ?22, ?23, ?24, 1,
            ?1, ?2, ?25, ?26
        )
        "#,
        params![
            machine_id,
            event.agent,
            event.project_key,
            event.session_key,
            event.turn_id,
            event.provider,
            event.model,
            event.reasoning_effort,
            event.prompt_tokens,
            event.completion_tokens,
            event.cache_read_tokens,
            event.cache_write_tokens,
            event.reasoning_tokens,
            event.total_tokens,
            event.estimated_cost_usd,
            event.confidence,
            event.occurred_at,
            event.source_key,
            event.parser_name,
            event.parser_version,
            raw_event_hash,
            imported_at,
            event.pricing_version,
            event.pricing_mode.as_str(),
            event.collector_event_fingerprint,
            batch_id,
        ],
    )
    .map_err(HubError::internal)?;
    Ok(UsageEventWrite::Inserted)
}
