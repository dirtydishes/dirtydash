use super::fleet::fleet_update_command_id;
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
            command_notify: Arc::new(Notify::new()),
            #[cfg(test)]
            final_insert_failure: Arc::new(Mutex::new(false)),
        }
    }

    pub(crate) fn db_path(&self) -> std::path::PathBuf {
        self.db.path().to_path_buf()
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
        let existing: Option<(Option<String>, Option<String>, i64)> = tx
            .query_row(
                "SELECT revoked_at, archived_at, state_revision FROM machines WHERE machine_id = ?1",
                params![machine_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        if let Some((revoked_at, archived_at, revision)) = existing {
            if revoked_at.is_some() || archived_at.is_some() {
                return Err(HubError::conflict(
                    "machine-archived",
                    "credential rotation cannot resurrect an archived Machine",
                ));
            }
            tx.execute(
                "UPDATE machines SET display_name = ?2, state_revision = ?3 + 1 WHERE machine_id = ?1 AND revoked_at IS NULL AND archived_at IS NULL",
                params![machine_id, display_name, revision],
            )
            .map_err(HubError::internal)?;
        } else {
            tx.execute(
                "INSERT INTO machines(machine_id, display_name, enrolled_at, last_seen_at, state_revision) VALUES (?1, ?2, ?3, ?3, 1)",
                params![machine_id, display_name, now],
            )
            .map_err(HubError::internal)?;
        }
        // Rotation is an overlap window, not an immediate cutover. The old
        // credential remains valid until the Collector proves the replacement
        // by authenticating a successful request.
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
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let changed = tx
            .execute(
                r#"
                UPDATE collector_credentials
                SET revoked_at = ?3
                WHERE machine_id = ?1
                    AND credential_id = ?2
                    AND revoked_at IS NULL
                "#,
                params![machine_id, credential_id, now],
            )
            .map_err(HubError::internal)?;
        if changed == 0 {
            return Err(HubError::not_found(
                "collector-credential-not-found",
                "collector credential was not found or is already revoked",
            ));
        }
        let machine_changed = tx
            .execute(
                "UPDATE machines SET state_revision = state_revision + 1 WHERE machine_id = ?1 AND archived_at IS NULL",
                params![machine_id],
            )
            .map_err(HubError::internal)?;
        if machine_changed == 0 {
            return Err(HubError::not_found(
                "machine-not-found",
                "collector credential target Machine was not found",
            ));
        }
        tx.commit().map_err(HubError::internal)
    }

    pub(crate) fn authenticate_collector_bearer(
        &self,
        bearer_token: &str,
    ) -> Result<AuthenticatedCollector, HubError> {
        let bearer_token = validate_non_empty(bearer_token, "collector bearer token")?;
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let token = bearer_token.strip_prefix("ddcol_").ok_or_else(|| {
            HubError::unauthorized(
                "collector-auth-required",
                "collector bearer token is invalid",
            )
        })?;
        let (credential_id, secret) = token.rsplit_once('.').ok_or_else(|| {
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

    pub(crate) fn activate_collector_credential_rotation(
        &self,
        auth: &AuthenticatedCollector,
        request: CollectorCredentialRotationActivationRequest,
    ) -> Result<CollectorCredentialRotationResponse, HubError> {
        let machine_id = validate_identifier(&request.machine_id, "machine_id")?;
        if machine_id != auth.machine_id {
            return Err(HubError::unauthorized(
                "collector-auth-required",
                "collector machine identity does not match the credential",
            ));
        }
        let rotation_id = validate_identifier(&request.rotation_id, "rotation_id")?;
        let replacement_secret =
            validate_non_empty(&request.replacement_secret, "replacement credential secret")?;
        if replacement_secret.len() > 512 {
            return Err(HubError::unprocessable(
                "invalid-credential-secret",
                "replacement credential secret is too long",
            ));
        }
        let secret_hash = sha256_hex(&replacement_secret);
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let label: String = tx
            .query_row(
                "SELECT credential_label FROM collector_credentials WHERE credential_id = ?1 AND machine_id = ?2 AND revoked_at IS NULL",
                params![auth.credential_id, auth.machine_id],
                |row| row.get(0),
            )
            .map_err(HubError::internal)?;
        let existing = tx
            .query_row(
                r#"
                SELECT machine_id, credential_id, status
                FROM collector_credential_rotations
                WHERE rotation_id = ?1
                "#,
                params![rotation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(HubError::internal)?;
        if let Some((existing_machine, credential_id, status)) = existing {
            if existing_machine != auth.machine_id {
                return Err(HubError::conflict(
                    "collector-rotation-conflict",
                    "rotation_id is already bound to another Machine",
                ));
            }
            let existing_hash: String = tx
                .query_row(
                    "SELECT secret_hash FROM collector_credentials WHERE credential_id = ?1",
                    params![credential_id],
                    |row| row.get(0),
                )
                .map_err(HubError::internal)?;
            if existing_hash != secret_hash {
                return Err(HubError::conflict(
                    "collector-rotation-conflict",
                    "rotation_id is already bound to a different replacement",
                ));
            }
            tx.commit().map_err(HubError::internal)?;
            return Ok(CollectorCredentialRotationResponse {
                machine_id,
                rotation_id,
                status,
            });
        }

        let credential_collision: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM collector_credentials WHERE credential_id = ?1)",
                params![rotation_id],
                |row| row.get(0),
            )
            .map_err(HubError::internal)?;
        if credential_collision {
            return Err(HubError::conflict(
                "collector-rotation-conflict",
                "rotation_id is already bound to a credential",
            ));
        }
        let now = now_utc();
        tx.execute(
            r#"
            INSERT INTO collector_credentials(
                credential_id, machine_id, credential_label, secret_hash, created_at, rotated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            "#,
            params![rotation_id, auth.machine_id, label, secret_hash, now],
        )
        .map_err(HubError::internal)?;
        tx.execute(
            r#"
            INSERT INTO collector_credential_rotations(
                rotation_id, machine_id, credential_id, previous_credential_id,
                credential_label, status, created_at, activated_at
            ) VALUES (?1, ?2, ?1, ?3, ?4, 'activated', ?5, ?5)
            "#,
            params![rotation_id, auth.machine_id, auth.credential_id, label, now],
        )
        .map_err(HubError::internal)?;
        tx.execute(
            "UPDATE machines SET state_revision = state_revision + 1 WHERE machine_id = ?1 AND revoked_at IS NULL AND archived_at IS NULL",
            params![auth.machine_id],
        )
        .map_err(HubError::internal)?;
        tx.commit().map_err(HubError::internal)?;
        Ok(CollectorCredentialRotationResponse {
            machine_id,
            rotation_id,
            status: "activated".to_string(),
        })
    }

    pub(crate) fn prove_collector_credential_rotation(
        &self,
        auth: &AuthenticatedCollector,
        request: CollectorCredentialRotationProofRequest,
    ) -> Result<CollectorCredentialRotationResponse, HubError> {
        let machine_id = validate_identifier(&request.machine_id, "machine_id")?;
        if machine_id != auth.machine_id {
            return Err(HubError::unauthorized(
                "collector-auth-required",
                "collector machine identity does not match the credential",
            ));
        }
        let rotation_id = validate_identifier(&request.rotation_id, "rotation_id")?;
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let rotation = tx
            .query_row(
                r#"
                SELECT machine_id, credential_id, credential_label, status
                FROM collector_credential_rotations
                WHERE rotation_id = ?1
                "#,
                params![rotation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(HubError::internal)?
            .ok_or_else(|| {
                HubError::not_found(
                    "collector-rotation-not-found",
                    "collector credential rotation was not found",
                )
            })?;
        if rotation.0 != auth.machine_id || rotation.1 != auth.credential_id {
            return Err(HubError::unauthorized(
                "collector-auth-required",
                "replacement credential proof is invalid",
            ));
        }
        if rotation.3 != "proved" {
            let now = now_utc();
            tx.execute(
                r#"
                UPDATE collector_credentials
                SET revoked_at = ?3
                WHERE machine_id = ?1
                    AND credential_label = ?2
                    AND credential_id <> ?4
                    AND revoked_at IS NULL
                "#,
                params![rotation.0, rotation.2, now, rotation.1],
            )
            .map_err(HubError::internal)?;
            tx.execute(
                r#"
                UPDATE collector_credential_rotations
                SET status = 'proved', proved_at = ?2
                WHERE rotation_id = ?1
                "#,
                params![rotation_id, now],
            )
            .map_err(HubError::internal)?;
            tx.execute(
                "UPDATE machines SET state_revision = state_revision + 1 WHERE machine_id = ?1 AND revoked_at IS NULL AND archived_at IS NULL",
                params![auth.machine_id],
            )
            .map_err(HubError::internal)?;
        }
        tx.commit().map_err(HubError::internal)?;
        Ok(CollectorCredentialRotationResponse {
            machine_id,
            rotation_id,
            status: "proved".to_string(),
        })
    }

    fn prove_collector_credential(&self, auth: &AuthenticatedCollector) -> Result<(), HubError> {
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        self.prove_collector_credential_tx(&tx, auth)?;
        tx.commit().map_err(HubError::internal)
    }

    /// A successful request made with the newest overlapping credential is
    /// proof that rotation completed. Only then are older credentials retired.
    fn prove_collector_credential_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        auth: &AuthenticatedCollector,
    ) -> Result<(), HubError> {
        let (machine_id, label, row_id): (String, String, i64) = tx
            .query_row(
                "SELECT machine_id, credential_label, rowid FROM collector_credentials WHERE credential_id = ?1 AND revoked_at IS NULL",
                params![auth.credential_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(HubError::internal)?;
        // Explicit rotation rows are retired only by the authenticated proof
        // endpoint. This keeps the old credential valid through activation,
        // proof retries, and Collector crash recovery.
        let explicit_rotation: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM collector_credential_rotations WHERE credential_id = ?1)",
                params![auth.credential_id],
                |row| row.get(0),
            )
            .map_err(HubError::internal)?;
        if explicit_rotation {
            return Ok(());
        }
        // A newer active row means this token is an older overlap token. It
        // remains valid so a Collector can safely fall back while staging the
        // newest token.
        let newer_exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM collector_credentials WHERE machine_id = ?1 AND credential_label = ?2 AND revoked_at IS NULL AND rowid > ?3)",
                params![machine_id, label, row_id],
                |row| row.get(0),
            )
            .map_err(HubError::internal)?;
        if newer_exists {
            return Ok(());
        }
        let revoked = tx
            .execute(
                "UPDATE collector_credentials SET revoked_at = ?3 WHERE machine_id = ?1 AND credential_label = ?2 AND revoked_at IS NULL AND rowid < ?4",
                params![machine_id, label, now_utc(), row_id],
            )
            .map_err(HubError::internal)?;
        if revoked > 0 {
            tx.execute(
                "UPDATE machines SET state_revision = state_revision + 1 WHERE machine_id = ?1 AND archived_at IS NULL",
                params![machine_id],
            )
            .map_err(HubError::internal)?;
        }
        Ok(())
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
        let active_machine: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM machines WHERE machine_id = ?1 AND revoked_at IS NULL AND archived_at IS NULL)",
                params![validated.machine_id],
                |row| row.get(0),
            )
            .map_err(HubError::internal)?;
        if !active_machine {
            return Err(HubError::unauthorized(
                "collector-revoked",
                "Collector credentials are revoked for this Machine",
            ));
        }

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
            self.prove_collector_credential_tx(&tx, auth)?;
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
            "UPDATE machines SET last_seen_at = ?2, last_sync_at = ?3, collector_version = ?4, collector_protocol_version = ?5, collector_runtime_generation = COALESCE(?6, collector_runtime_generation), state_revision = state_revision + 1 WHERE machine_id = ?1 AND archived_at IS NULL AND revoked_at IS NULL",
            params![
                validated.machine_id,
                committed_at,
                validated.sync_run.finished_at,
                validated.sync_run.collector_version,
                validated.protocol_version,
                validated.sync_run.runtime_generation,
            ],
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
                validated.protocol_version,
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

        self.prove_collector_credential_tx(&tx, auth)?;
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

    pub(crate) fn command_notification(&self) -> Arc<Notify> {
        Arc::clone(&self.command_notify)
    }

    pub(crate) fn issue_collector_command(
        &self,
        request: IssueCollectorCommandRequest,
    ) -> Result<IssueCollectorCommandResponse, HubError> {
        let machine_id = validate_identifier(&request.machine_id, "machine_id")?;
        let command_id = validate_identifier(request.command.command_id(), "command_id")?;
        match &request.command {
            OwnerCommand::RotateCredential { rotation_id, .. } => {
                validate_identifier(rotation_id, "rotation_id")?;
            }
            OwnerCommand::ApprovedUpdate {
                update_id,
                version,
                sha256,
                ..
            }
            | OwnerCommand::RollbackUpdate {
                update_id,
                version,
                sha256,
                ..
            } => {
                validate_identifier(update_id, "update_id")?;
                if version.is_empty()
                    || !version.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
                    })
                    || sha256.len() != 64
                    || !sha256
                        .chars()
                        .all(|character| character.is_ascii_hexdigit())
                {
                    return Err(HubError::unprocessable(
                        "invalid-approved-update",
                        "Collector updates require a safe version and SHA-256 digest",
                    ));
                }
            }
            OwnerCommand::Refresh { .. }
            | OwnerCommand::Repair { .. }
            | OwnerCommand::Diagnostics { .. } => {}
        }
        let command = serde_json::to_value(&request.command).map_err(HubError::internal)?;
        validate_command_has_no_secret(&command)?;
        let command_json = serde_json::to_string(&command).map_err(HubError::internal)?;
        if command_json.len() > 4096 {
            return Err(HubError::unprocessable(
                "collector-command-too-large",
                "Collector commands are limited to the typed allowlist size",
            ));
        }
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let machine: Option<(Option<String>, Option<String>, i64)> = tx
            .query_row(
                "SELECT revoked_at, archived_at, state_revision FROM machines WHERE machine_id = ?1",
                params![machine_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some((revoked_at, archived_at, state_revision)) = machine else {
            return Err(HubError::not_found(
                "machine-not-found",
                "Collector command target Machine was not found",
            ));
        };
        if revoked_at.is_some() || archived_at.is_some() {
            return Err(HubError::conflict(
                "machine-archived",
                "archived Machines cannot receive Collector commands",
            ));
        }
        if let Some(expected) = request.expected_state_revision {
            if expected != state_revision {
                return Err(HubError::conflict(
                    "machine-state-conflict",
                    "Machine state changed; reload before issuing this action",
                ));
            }
        }
        let existing = tx
            .query_row(
                "SELECT machine_id, command_json FROM collector_commands WHERE command_id = ?1",
                params![command_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        if let Some((existing_machine, existing_json)) = existing {
            let same_command = existing_machine == machine_id
                && serde_json::from_str::<serde_json::Value>(&existing_json).ok()
                    == serde_json::from_str::<serde_json::Value>(&command_json).ok();
            if !same_command {
                return Err(HubError::conflict(
                    "collector-command-conflict",
                    "command_id is already bound to a different machine or command",
                ));
            }
            tx.commit().map_err(HubError::internal)?;
            return Ok(IssueCollectorCommandResponse {
                command_id,
                machine_id,
            });
        }
        tx.execute(
            r#"
            INSERT INTO collector_commands(command_id, machine_id, command_json, created_at)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            params![command_id, machine_id, command_json, now],
        )
        .map_err(HubError::internal)?;
        let revision_changed = tx
            .execute(
                "UPDATE machines SET state_revision = state_revision + 1 WHERE machine_id = ?1 AND state_revision = ?2 AND archived_at IS NULL",
                params![machine_id, state_revision],
            )
            .map_err(HubError::internal)?;
        if revision_changed == 0 {
            return Err(HubError::conflict(
                "machine-state-conflict",
                "Machine lifecycle changed while issuing the Collector command",
            ));
        }
        tx.commit().map_err(HubError::internal)?;
        // Keep a permit if the poller is between its immediate DB check and
        // registering the long-poll future; this closes the command wake race.
        self.command_notify.notify_one();
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
        let active_machine: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM machines WHERE machine_id = ?1 AND revoked_at IS NULL AND archived_at IS NULL)",
                params![auth.machine_id],
                |row| row.get(0),
            )
            .map_err(HubError::internal)?;
        if !active_machine {
            return Err(HubError::unauthorized(
                "collector-revoked",
                "Collector credentials are revoked for this Machine",
            ));
        }
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
        request.result.validate()?;
        let result_json = serde_json::to_string(&request.result).map_err(HubError::internal)?;
        let diagnostics_result = match &request.result {
            CollectorCommandResult::Diagnostics { diagnostics } => Some(diagnostics.clone()),
            _ => None,
        };
        let write_guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let conn = self.db.connection().map_err(HubError::internal)?;
        let existing = conn
            .query_row(
                "SELECT machine_id, acknowledged_at, result_json, command_json FROM collector_commands WHERE command_id = ?1",
                params![command_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(HubError::internal)?
            .ok_or_else(|| {
                HubError::not_found(
                    "collector-command-not-found",
                    "collector command was not found or belongs to another Machine",
                )
            })?;
        if existing.0 != auth.machine_id {
            return Err(HubError::not_found(
                "collector-command-not-found",
                "collector command was not found or belongs to another Machine",
            ));
        }
        let command: OwnerCommand =
            serde_json::from_str(&existing.3).map_err(HubError::internal)?;
        if !request.result.matches_command(&command) {
            return Err(HubError::unprocessable(
                "collector-command-result-mismatch",
                "Collector acknowledgement does not match the issued command",
            ));
        }
        let rejected_update = match (&command, &request.result) {
            (
                OwnerCommand::ApprovedUpdate { update_id, .. },
                CollectorCommandResult::Rejected { reason },
            ) => Some((update_id.clone(), reason.clone())),
            _ => None,
        };
        if let Some(existing_result) = existing.2 {
            let same_result = serde_json::from_str::<serde_json::Value>(&existing_result).ok()
                == serde_json::from_str::<serde_json::Value>(&result_json).ok();
            if !same_result {
                return Err(HubError::conflict(
                    "collector-command-ack-conflict",
                    "command acknowledgement conflicts with the previously recorded result",
                ));
            }
            // A byte-identical (or JSON-equivalent) acknowledgement is a
            // successful replay, not a not-found error.
            drop(conn);
            drop(write_guard);
            if let Some((update_id, reason)) = rejected_update.as_ref() {
                self.record_collector_update_rejection(auth, update_id, reason)?;
            }
            self.prove_collector_credential(auth)?;
            return Ok(());
        }
        let changed = conn
            .execute(
                r#"
                UPDATE collector_commands
                SET acknowledged_at = ?3, result_json = ?4
                WHERE command_id = ?1 AND machine_id = ?2 AND acknowledged_at IS NULL
                "#,
                params![command_id, auth.machine_id, now_utc(), result_json],
            )
            .map_err(HubError::internal)?;
        if changed == 0 {
            return Err(HubError::conflict(
                "collector-command-ack-conflict",
                "collector command acknowledgement was changed concurrently",
            ));
        }
        drop(conn);
        drop(write_guard);
        if let Some(diagnostics) = diagnostics_result {
            self.record_collector_diagnostics(&auth.machine_id, &diagnostics)?;
        }
        if let Some((update_id, reason)) = rejected_update.as_ref() {
            self.record_collector_update_rejection(auth, update_id, reason)?;
        }
        self.prove_collector_credential(auth)?;
        Ok(())
    }

    fn record_collector_update_rejection(
        &self,
        auth: &AuthenticatedCollector,
        update_id: &str,
        reason: &str,
    ) -> Result<(), HubError> {
        let update_id = validate_identifier(update_id, "update_id")?;
        let now = now_utc();
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let node: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT status, previous_desired_version FROM fleet_update_nodes WHERE update_id = ?1 AND machine_id = ?2",
                params![update_id, auth.machine_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some((status, previous_desired_version)) = node else {
            return Err(HubError::not_found(
                "update-machine-not-found",
                "Collector rejection is not part of a fleet update",
            ));
        };
        if matches!(status.as_str(), "succeeded" | "rolled-back") {
            tx.commit().map_err(HubError::internal)?;
            return Ok(());
        }
        tx.execute(
            "UPDATE fleet_update_nodes SET status = 'rolled-back', rolled_back_at = ?3, failure_reason = ?4, attempts = attempts + 1, state_revision = state_revision + 1 WHERE update_id = ?1 AND machine_id = ?2 AND status IN ('queued', 'updating')",
            params![update_id, auth.machine_id, now, reason],
        )
        .map_err(HubError::internal)?;
        tx.execute(
            "UPDATE machines SET desired_version = ?2, state_revision = state_revision + 1 WHERE machine_id = ?1 AND archived_at IS NULL",
            params![auth.machine_id, previous_desired_version],
        )
        .map_err(HubError::internal)?;
        tx.commit().map_err(HubError::internal)?;
        drop(_guard);
        self.finish_update_if_terminal(&update_id).map(|_| ())
    }

    pub(crate) fn record_collector_update_receipt(
        &self,
        auth: &AuthenticatedCollector,
        request: CollectorUpdateReceiptRequest,
    ) -> std::result::Result<FleetUpdateRun, HubError> {
        let receipt = request.receipt;
        CollectorCommandResult::UpdateApplied {
            update_id: receipt.update_id.clone(),
            command_id: receipt.command_id.clone(),
            version: receipt.version.clone(),
            sha256: receipt.sha256.clone(),
        }
        .validate()?;
        validate_update_receipt(&receipt)?;
        Self::validate_update_receipt_timestamps(&receipt)?;
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let update: Option<(String, String, String, String, String, String)> = tx
            .query_row(
                "SELECT version, artifact_sha256, status, publisher_key_id, publisher_fingerprint, manifest_sha256 FROM fleet_update_runs WHERE update_id = ?1",
                params![receipt.update_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some((version, sha256, status, _, _, _)) = update else {
            return Err(HubError::not_found(
                "update-not-found",
                "fleet update was not found",
            ));
        };
        if status != "collectors-queued" {
            return Err(HubError::conflict(
                "collector-update-order",
                "Collector receipts are accepted only after the Hub health gate",
            ));
        }
        if version != receipt.version || !sha256.eq_ignore_ascii_case(&receipt.sha256) {
            return Err(HubError::unprocessable(
                "collector-update-receipt-mismatch",
                "Collector receipt does not match the signed fleet update",
            ));
        }
        let expected_command_id = fleet_update_command_id(&receipt.update_id, &auth.machine_id);
        if expected_command_id != receipt.command_id {
            return Err(HubError::unprocessable(
                "collector-update-receipt-mismatch",
                "Collector receipt is not bound to the durable update command",
            ));
        }
        let command_record: Option<(String, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT machine_id, acknowledged_at, result_json FROM collector_commands WHERE command_id = ?1",
                params![receipt.command_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some((command_machine_id, acknowledged_at, result_json)) = command_record else {
            return Err(HubError::conflict(
                "collector-update-command-missing",
                "Collector update receipt requires the durable command acknowledgement",
            ));
        };
        if command_machine_id != auth.machine_id || acknowledged_at.is_none() {
            return Err(HubError::conflict(
                "collector-update-command-unacknowledged",
                "Collector update receipt requires an acknowledged command for this Machine",
            ));
        }
        let Some(result_json) = result_json else {
            return Err(HubError::conflict(
                "collector-update-command-unacknowledged",
                "Collector update command has no authenticated result",
            ));
        };
        let result: CollectorCommandResult =
            serde_json::from_str(&result_json).map_err(HubError::internal)?;
        let command_result_matches = matches!(
            result,
            CollectorCommandResult::UpdateApplied { ref update_id, ref command_id, ref version, ref sha256 }
                if update_id == &receipt.update_id
                    && command_id == &receipt.command_id
                    && version == &receipt.version
                    && sha256.eq_ignore_ascii_case(&receipt.sha256)
        );
        if !command_result_matches {
            return Err(HubError::conflict(
                "collector-update-command-result-mismatch",
                "Collector update receipt does not match the authenticated command result",
            ));
        }
        let node: Option<(String, i64, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT status, state_revision, previous_runtime_generation, evidence_json FROM fleet_update_nodes WHERE update_id = ?1 AND machine_id = ?2",
                params![receipt.update_id, auth.machine_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some((node_status, node_revision, previous_runtime_generation, previous_receipt)) =
            node
        else {
            return Err(HubError::not_found(
                "update-machine-not-found",
                "Collector is not in this fleet update",
            ));
        };
        if node_status == "succeeded" {
            let same = previous_receipt
                .as_deref()
                .and_then(|value| serde_json::from_str::<CollectorUpdateReceipt>(value).ok())
                .is_some_and(|existing| existing == receipt);
            if same {
                tx.commit().map_err(HubError::internal)?;
                return self.fleet_update(&receipt.update_id);
            }
            return Err(HubError::conflict(
                "collector-update-receipt-conflict",
                "Collector update receipt conflicts with the durable receipt",
            ));
        }
        if previous_runtime_generation
            .as_deref()
            .is_some_and(|previous| previous == receipt.runtime_generation)
        {
            return Err(HubError::unprocessable(
                "collector-update-receipt-mismatch",
                "Collector receipt did not prove a new runtime generation",
            ));
        }
        if node_status != "updating" {
            return Err(HubError::conflict(
                "collector-update-state",
                "Collector update is not awaiting an authenticated receipt",
            ));
        }
        let evidence_json = serde_json::to_string(&receipt).map_err(HubError::internal)?;
        let now = now_utc();
        let node_changed = tx
            .execute(
                "UPDATE fleet_update_nodes SET status = 'succeeded', restarted_at = ?3, health_checked_at = ?4, collector_protocol_version = ?5, evidence_json = ?6, attempts = attempts + 1, state_revision = state_revision + 1 WHERE update_id = ?1 AND machine_id = ?2 AND status = 'updating' AND state_revision = ?7",
                params![receipt.update_id, auth.machine_id, receipt.restarted_at, receipt.health_checked_at, receipt.protocol_version, evidence_json, node_revision],
            )
            .map_err(HubError::internal)?;
        if node_changed == 0 {
            return Err(HubError::conflict(
                "collector-update-state",
                "Collector update changed while committing its receipt",
            ));
        }
        let machine_changed = tx
            .execute(
                "UPDATE machines SET collector_version = ?2, collector_protocol_version = ?3, collector_runtime_generation = ?4, desired_version = NULL, last_seen_at = ?5, state_revision = state_revision + 1 WHERE machine_id = ?1 AND archived_at IS NULL AND revoked_at IS NULL",
                params![auth.machine_id, receipt.collector_version, receipt.protocol_version, receipt.runtime_generation, now],
            )
            .map_err(HubError::internal)?;
        if machine_changed == 0 {
            return Err(HubError::conflict(
                "machine-state-conflict",
                "Machine was archived while committing its Collector receipt",
            ));
        }
        tx.commit().map_err(HubError::internal)?;
        self.finish_update_if_terminal(&receipt.update_id)
    }

    fn validate_update_receipt_timestamps(
        receipt: &CollectorUpdateReceipt,
    ) -> std::result::Result<(), HubError> {
        let restarted_at = parse_utc_timestamp(&receipt.restarted_at)?;
        let health_checked_at = parse_utc_timestamp(&receipt.health_checked_at)?;
        if health_checked_at < restarted_at {
            return Err(HubError::unprocessable(
                "collector-update-receipt-mismatch",
                "Collector health proof must follow its restart proof",
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
