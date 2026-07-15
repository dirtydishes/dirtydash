use super::*;
use chrono::NaiveDate;
use chrono_tz::Tz;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use std::collections::BTreeMap;
impl HubRepository {
    pub fn new(db: Database) -> Self {
        Self {
            db,
            write_guard: Arc::new(Mutex::new(())),
            #[cfg(test)]
            final_insert_failure: Arc::new(Mutex::new(false)),
        }
    }
    pub(crate) fn rotate_collector_credential(
        &self,
        request: RotateCollectorCredentialRequest,
    ) -> Result<IssuedCollectorCredential, HubError> {
        let machine_id = validate_identifier(&request.machine_id, "machine_id")?;
        let display_name = validate_non_empty(&request.display_name, "display_name")?;
        let credential_label = validate_identifier(&request.credential_label, "credential_label")?;
        let credential_id = random_token(12);
        let secret = random_token(24);
        let secret_hash = sha256_hex(&secret);
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        tx.execute(
            r#"
            INSERT INTO machines(machine_id, display_name, enrolled_at, last_seen_at)
            VALUES (?1, ?2, ?3, ?3)
            ON CONFLICT(machine_id) DO UPDATE SET
                display_name = excluded.display_name,
                revoked_at = NULL
            "#,
            params![machine_id, display_name, now],
        )
        .map_err(HubError::internal)?;
        tx.execute(
            r#"
            UPDATE collector_credentials
            SET revoked_at = ?3
            WHERE machine_id = ?1
                AND credential_label = ?2
                AND revoked_at IS NULL
            "#,
            params![machine_id, credential_label, now],
        )
        .map_err(HubError::internal)?;
        tx.execute(
            r#"
            INSERT INTO collector_credentials(
                credential_id, machine_id, credential_label, secret_hash, created_at, rotated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            "#,
            params![
                credential_id,
                machine_id,
                credential_label,
                secret_hash,
                now
            ],
        )
        .map_err(HubError::internal)?;
        tx.commit().map_err(HubError::internal)?;
        Ok(IssuedCollectorCredential {
            machine_id,
            credential_id: credential_id.clone(),
            token: format!("ddcol_{credential_id}.{secret}"),
        })
    }

    pub(crate) fn revoke_collector_credential(
        &self,
        request: RevokeCollectorCredentialRequest,
    ) -> Result<(), HubError> {
        let machine_id = validate_identifier(&request.machine_id, "machine_id")?;
        let credential_id = validate_identifier(&request.credential_id, "credential_id")?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        let changed = conn
            .execute(
                r#"
                UPDATE collector_credentials
                SET revoked_at = ?3
                WHERE machine_id = ?1
                    AND credential_id = ?2
                    AND revoked_at IS NULL
                "#,
                params![machine_id, credential_id, now_utc()],
            )
            .map_err(HubError::internal)?;
        if changed == 0 {
            return Err(HubError::not_found(
                "collector-credential-not-found",
                "collector credential was not found or is already revoked",
            ));
        }
        Ok(())
    }

    pub(crate) fn authenticate_collector_bearer(
        &self,
        bearer_token: &str,
    ) -> Result<AuthenticatedCollector, HubError> {
        let bearer_token = validate_non_empty(bearer_token, "collector bearer token")?;
        let token = bearer_token.strip_prefix("ddcol_").ok_or_else(|| {
            HubError::unauthorized(
                "collector-auth-required",
                "collector bearer token is invalid",
            )
        })?;
        let (credential_id, secret) = token.split_once('.').ok_or_else(|| {
            HubError::unauthorized(
                "collector-auth-required",
                "collector bearer token is invalid",
            )
        })?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        let record = conn
            .query_row(
                r#"
                SELECT c.machine_id, c.secret_hash
                FROM collector_credentials c
                JOIN machines m ON m.machine_id = c.machine_id
                WHERE c.credential_id = ?1
                    AND c.revoked_at IS NULL
                    AND m.revoked_at IS NULL
                "#,
                params![credential_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(HubError::internal)?
            .ok_or_else(|| {
                HubError::unauthorized(
                    "collector-auth-required",
                    "collector bearer token is invalid",
                )
            })?;
        if sha256_hex(secret) != record.1 {
            return Err(HubError::unauthorized(
                "collector-auth-required",
                "collector bearer token is invalid",
            ));
        }
        conn.execute(
            "UPDATE collector_credentials SET last_used_at = ?2 WHERE credential_id = ?1",
            params![credential_id, now_utc()],
        )
        .map_err(HubError::internal)?;
        Ok(AuthenticatedCollector {
            machine_id: record.0,
            credential_id: credential_id.to_string(),
        })
    }

    pub(crate) fn ingest_batch(
        &self,
        auth: &AuthenticatedCollector,
        request: IngestBatchRequest,
    ) -> Result<IngestBatchResponse, HubError> {
        let validated = validate_ingest_batch(request, &auth.machine_id)?;
        let committed_at = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;

        let existing_batch = tx
            .query_row(
                r#"
                SELECT request_fingerprint, event_count, committed_at
                FROM ingest_batches
                WHERE machine_id = ?1 AND batch_id = ?2
                "#,
                params![validated.machine_id, validated.batch_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)? as u64,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(HubError::internal)?;
        if let Some((request_fingerprint, event_count, committed_at)) = existing_batch {
            if request_fingerprint != validated.request_fingerprint {
                return Err(HubError::conflict(
                    "ingest-batch-conflict",
                    "batch_id was already committed with a different payload",
                ));
            }
            tx.commit().map_err(HubError::internal)?;
            return Ok(IngestBatchResponse {
                batch_id: validated.batch_id,
                inserted_events: 0,
                updated_events: 0,
                skipped_events: event_count,
                idempotent_replay: true,
                committed_at,
            });
        }

        tx.execute(
            "UPDATE machines SET last_seen_at = ?2 WHERE machine_id = ?1",
            params![validated.machine_id, committed_at],
        )
        .map_err(HubError::internal)?;

        tx.execute(
            r#"
            INSERT INTO sync_runs(machine_id, sync_run_id, collector_version, started_at, finished_at, status, event_count, batch_id)
            VALUES (?1, ?2, ?3, ?4, ?5, 'committed', ?6, ?7)
            ON CONFLICT(machine_id, sync_run_id) DO UPDATE SET
                collector_version = excluded.collector_version,
                started_at = excluded.started_at,
                finished_at = excluded.finished_at,
                status = excluded.status,
                event_count = excluded.event_count,
                batch_id = excluded.batch_id
            "#,
            params![
                validated.machine_id,
                validated.sync_run.sync_run_id,
                validated.sync_run.collector_version,
                validated.sync_run.started_at,
                validated.sync_run.finished_at,
                validated.events.len() as u64,
                validated.batch_id,
            ],
        )
        .map_err(HubError::internal)?;

        for manifest in &validated.source_manifests {
            tx.execute(
                r#"
                INSERT INTO source_manifests(
                    machine_id, sync_run_id, source_key, agent, display_path, item_count, cursor, manifest_fingerprint, recorded_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(machine_id, sync_run_id, source_key) DO UPDATE SET
                    agent = excluded.agent,
                    display_path = excluded.display_path,
                    item_count = excluded.item_count,
                    cursor = excluded.cursor,
                    manifest_fingerprint = excluded.manifest_fingerprint,
                    recorded_at = excluded.recorded_at
                "#,
                params![
                    validated.machine_id,
                    validated.sync_run.sync_run_id,
                    manifest.source_key,
                    manifest.agent,
                    manifest.display_path,
                    manifest.item_count,
                    manifest.cursor,
                    manifest.manifest_fingerprint,
                    committed_at,
                ],
            )
            .map_err(HubError::internal)?;
        }

        for checkpoint in &validated.checkpoints {
            tx.execute(
                r#"
                INSERT INTO ingest_checkpoints(machine_id, agent, checkpoint_key, checkpoint_value, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(machine_id, agent, checkpoint_key) DO UPDATE SET
                    checkpoint_value = excluded.checkpoint_value,
                    updated_at = excluded.updated_at
                "#,
                params![
                    validated.machine_id,
                    checkpoint.agent,
                    checkpoint.checkpoint_key,
                    checkpoint.checkpoint_value,
                    committed_at,
                ],
            )
            .map_err(HubError::internal)?;
        }

        let mut inserted_events = 0_u64;
        let mut updated_events = 0_u64;
        let mut skipped_events = 0_u64;
        for event in &validated.events {
            match upsert_usage_event_tx(
                &tx,
                &validated.machine_id,
                &validated.batch_id,
                event,
                &committed_at,
            )? {
                UsageEventWrite::Inserted => inserted_events += 1,
                UsageEventWrite::Updated => updated_events += 1,
                UsageEventWrite::Skipped => skipped_events += 1,
            }
        }

        #[cfg(test)]
        if self.take_final_insert_failure() {
            tx.execute(
                "INSERT INTO hub_test_missing_final_insert(table_column) VALUES (1)",
                [],
            )
            .map_err(HubError::internal)?;
        }
        tx.execute(
            r#"
            INSERT INTO ingest_batches(
                machine_id, batch_id, protocol_version, credential_id, request_fingerprint,
                event_count, source_manifest_count, checkpoint_count, sync_run_id, committed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                validated.machine_id,
                validated.batch_id,
                SUPPORTED_PROTOCOL_VERSION,
                auth.credential_id,
                validated.request_fingerprint,
                validated.events.len() as u64,
                validated.source_manifests.len() as u64,
                validated.checkpoints.len() as u64,
                validated.sync_run.sync_run_id,
                committed_at,
            ],
        )
        .map_err(HubError::internal)?;

        tx.commit().map_err(HubError::internal)?;
        Ok(IngestBatchResponse {
            batch_id: validated.batch_id,
            inserted_events,
            updated_events,
            skipped_events,
            idempotent_replay: false,
            committed_at,
        })
    }

    pub(crate) fn issue_collector_command(
        &self,
        request: IssueCollectorCommandRequest,
    ) -> Result<IssueCollectorCommandResponse, HubError> {
        let machine_id = validate_identifier(&request.machine_id, "machine_id")?;
        let command_id = validate_identifier(request.command.command_id(), "command_id")?;
        let command_json = serde_json::to_string(&request.command).map_err(HubError::internal)?;
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let conn = self.db.connection().map_err(HubError::internal)?;
        conn.execute(
            r#"
            INSERT INTO collector_commands(command_id, machine_id, command_json, created_at)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            params![command_id, machine_id, command_json, now],
        )
        .map_err(HubError::internal)?;
        Ok(IssueCollectorCommandResponse {
            command_id,
            machine_id,
        })
    }

    pub(crate) fn poll_collector_command(
        &self,
        auth: &AuthenticatedCollector,
    ) -> Result<Option<OwnerCommand>, HubError> {
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let pending = tx
            .query_row(
                r#"
                SELECT command_id, command_json
                FROM collector_commands
                WHERE machine_id = ?1
                    AND acknowledged_at IS NULL
                    AND (claimed_at IS NULL OR julianday(claimed_at) <= julianday(?2) - (60.0 / 86400.0))
                ORDER BY created_at, command_id
                LIMIT 1
                "#,
                params![auth.machine_id, now_utc()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some((command_id, command_json)) = pending else {
            tx.commit().map_err(HubError::internal)?;
            return Ok(None);
        };
        tx.execute(
            "UPDATE collector_commands SET claimed_at = ?2 WHERE command_id = ?1 AND acknowledged_at IS NULL",
            params![command_id, now_utc()],
        )
        .map_err(HubError::internal)?;
        tx.commit().map_err(HubError::internal)?;
        let command = serde_json::from_str(&command_json).map_err(HubError::internal)?;
        Ok(Some(command))
    }

    pub(crate) fn acknowledge_collector_command(
        &self,
        auth: &AuthenticatedCollector,
        request: CollectorCommandAckRequest,
    ) -> Result<(), HubError> {
        let command_id = validate_identifier(&request.command_id, "command_id")?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        let changed = conn
            .execute(
                r#"
                UPDATE collector_commands
                SET acknowledged_at = ?3, result_json = ?4
                WHERE command_id = ?1 AND machine_id = ?2 AND acknowledged_at IS NULL
                "#,
                params![
                    command_id,
                    auth.machine_id,
                    now_utc(),
                    serde_json::to_string(&request.result).map_err(HubError::internal)?
                ],
            )
            .map_err(HubError::internal)?;
        if changed == 0 {
            return Err(HubError::not_found(
                "collector-command-not-found",
                "collector command was not found, belongs to another Machine, or was already acknowledged",
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn inject_final_insert_failure(&self) {
        *self
            .final_insert_failure
            .lock()
            .expect("hub failure mutex poisoned") = true;
    }

    #[cfg(test)]
    fn take_final_insert_failure(&self) -> bool {
        let mut failure = self
            .final_insert_failure
            .lock()
            .expect("hub failure mutex poisoned");
        let should_fail = *failure;
        *failure = false;
        should_fail
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn usage_by_day_in_owner_time_zone(
        &self,
        time_zone: &str,
    ) -> Result<Vec<UsageDayBucket>, HubError> {
        let time_zone = validate_time_zone(time_zone)?;
        let tz: Tz = time_zone.parse().map_err(|_| {
            HubError::unprocessable(
                "invalid-time-zone",
                "time_zone must be a valid IANA time zone",
            )
        })?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT COALESCE(event_timestamp, imported_at), total_tokens, estimated_cost_usd
                FROM usage_events
                ORDER BY COALESCE(event_timestamp, imported_at)
                "#,
            )
            .map_err(HubError::internal)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, f64>(2)?,
                ))
            })
            .map_err(HubError::internal)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(HubError::internal)?;
        let mut buckets = BTreeMap::<NaiveDate, (u64, f64)>::new();
        for (timestamp, total_tokens, estimated_cost_usd) in rows {
            let utc = parse_utc_timestamp(&timestamp)?;
            let day = utc.with_timezone(&tz).date_naive();
            let entry = buckets.entry(day).or_insert((0, 0.0));
            entry.0 += total_tokens;
            entry.1 += estimated_cost_usd;
        }
        Ok(buckets
            .into_iter()
            .map(|(day, (total_tokens, estimated_cost_usd))| UsageDayBucket {
                day: day.to_string(),
                total_tokens,
                estimated_cost_usd,
            })
            .collect())
    }
}
