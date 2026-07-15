use super::*;
use rusqlite::{params, OptionalExtension, TransactionBehavior};

impl HubRepository {
    pub(crate) fn bootstrap_owner(
        &self,
        request: BootstrapOwnerRequest,
    ) -> Result<IssuedOwnerSession, HubError> {
        let username = validate_identifier(&request.username, "username")?;
        let time_zone = validate_time_zone(&request.time_zone)?;
        let tailscale_identity = request
            .tailscale_identity
            .as_deref()
            .map(validate_tailscale_identity)
            .transpose()?;
        let password_hash = hash_password(&request.password)?;
        let issued_at = now_utc();
        let expires_at = plus_seconds(&issued_at, OWNER_SESSION_TTL_SECONDS)?;
        let csrf_token = random_token(24);
        let session_id = random_token(24);
        let csrf_hash = sha256_hex(&csrf_token);
        let _guard = self.write_guard.lock().expect("hub write mutex poisoned");
        let mut conn = self.db.connection().map_err(HubError::internal)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(HubError::internal)?;
        let existing_owner = tx
            .query_row("SELECT owner_id FROM owners LIMIT 1", [], |row| {
                row.get::<_, String>(0)
            })
            .optional()
            .map_err(HubError::internal)?;
        if existing_owner.is_some() {
            return Err(HubError::conflict(
                "owner-already-bootstrapped",
                "owner bootstrap is only allowed before the first owner exists",
            ));
        }
        let owner_id = random_token(12);
        tx.execute(
            r#"
            INSERT INTO owners(owner_id, username, password_hash, time_zone, created_at, updated_at, password_updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?5)
            "#,
            params![owner_id, username, password_hash, time_zone, issued_at],
        )
        .map_err(HubError::internal)?;
        if let Some(tailscale_identity) = &tailscale_identity {
            tx.execute(
                r#"
                INSERT INTO owner_tailscale_identities(owner_id, tailscale_identity, created_at)
                VALUES (?1, ?2, ?3)
                "#,
                params![owner_id, tailscale_identity, issued_at],
            )
            .map_err(HubError::internal)?;
        }
        tx.execute(
            r#"
            INSERT INTO owner_sessions(session_id, owner_id, csrf_token_hash, trusted_tailscale_user, created_at, last_seen_at, expires_at)
            VALUES (?1, ?2, ?3, NULL, ?4, ?4, ?5)
            "#,
            params![session_id, owner_id, csrf_hash, issued_at, expires_at],
        )
        .map_err(HubError::internal)?;
        tx.commit().map_err(HubError::internal)?;
        Ok(IssuedOwnerSession {
            session_id,
            owner_username: username,
            time_zone,
            csrf_token,
            trusted_tailscale_user: None,
        })
    }

    pub(crate) fn login_owner(
        &self,
        request: OwnerLoginRequest,
    ) -> Result<IssuedOwnerSession, HubError> {
        let owner = self.owner_by_username(&request.username)?;
        verify_password(&owner.password_hash, &request.password)?;
        self.issue_owner_session(&owner, None)
    }

    pub(crate) fn login_owner_via_tailscale(
        &self,
        trusted_tailscale_identity: &str,
        configured_mappings: &[TailscaleOwnerMapping],
    ) -> Result<IssuedOwnerSession, HubError> {
        let trusted_tailscale_identity = validate_tailscale_identity(trusted_tailscale_identity)?;
        if let Some(owner) =
            self.owner_by_persisted_tailscale_identity(&trusted_tailscale_identity)?
        {
            return self.issue_owner_session(&owner, Some(trusted_tailscale_identity));
        }
        let Some(mapping) = configured_mappings
            .iter()
            .find(|mapping| mapping.tailscale_identity == trusted_tailscale_identity)
        else {
            return Err(HubError::unauthorized(
                "tailscale-identity-not-authorized",
                "the trusted Tailscale identity is not mapped to an owner",
            ));
        };
        let owner = self.owner_by_username(&mapping.owner_username)?;
        self.issue_owner_session(&owner, Some(trusted_tailscale_identity))
    }

    pub(crate) fn authenticate_owner_session(
        &self,
        session_id: &str,
    ) -> Result<OwnerSessionRecord, HubError> {
        let session_id = validate_non_empty(session_id, "owner session")?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        let now = now_utc();
        let session = conn
            .query_row(
                r#"
                SELECT s.session_id, s.owner_id, o.username, o.time_zone, s.trusted_tailscale_user
                FROM owner_sessions s
                JOIN owners o ON o.owner_id = s.owner_id
                WHERE s.session_id = ?1
                    AND s.revoked_at IS NULL
                    AND s.expires_at > ?2
                "#,
                params![session_id, now],
                |row| {
                    Ok(OwnerSessionRecord {
                        session_id: row.get(0)?,
                        owner_username: row.get(2)?,
                        time_zone: row.get(3)?,
                        trusted_tailscale_user: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(HubError::internal)?;
        let Some(session) = session else {
            return Err(HubError::unauthorized(
                "owner-session-required",
                "a valid owner session is required",
            ));
        };
        conn.execute(
            "UPDATE owner_sessions SET last_seen_at = ?2 WHERE session_id = ?1",
            params![session.session_id, now_utc()],
        )
        .map_err(HubError::internal)?;
        Ok(session)
    }

    pub(crate) fn verify_owner_csrf(
        &self,
        session_id: &str,
        csrf_token: &str,
    ) -> Result<(), HubError> {
        let csrf_token = validate_non_empty(csrf_token, "csrf token")?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        let expected_hash = conn
            .query_row(
                r#"
                SELECT csrf_token_hash
                FROM owner_sessions
                WHERE session_id = ?1
                    AND revoked_at IS NULL
                "#,
                params![session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(HubError::internal)?
            .ok_or_else(|| {
                HubError::unauthorized(
                    "owner-session-required",
                    "a valid owner session is required",
                )
            })?;
        if sha256_hex(&csrf_token) != expected_hash {
            return Err(HubError::forbidden(
                "csrf-mismatch",
                "state-changing admin requests require a matching CSRF token",
            ));
        }
        Ok(())
    }

    pub(crate) fn issue_owner_csrf(&self, session_id: &str) -> Result<String, HubError> {
        let session = self.authenticate_owner_session(session_id)?;
        let token = random_token(24);
        let conn = self.db.connection().map_err(HubError::internal)?;
        let changed = conn
            .execute(
                "UPDATE owner_sessions SET csrf_token_hash = ?2, last_seen_at = ?3 WHERE session_id = ?1 AND revoked_at IS NULL",
                params![session.session_id, sha256_hex(&token), now_utc()],
            )
            .map_err(HubError::internal)?;
        if changed == 0 {
            return Err(HubError::unauthorized(
                "owner-session-required",
                "a valid owner session is required",
            ));
        }
        Ok(token)
    }

    pub(crate) fn logout_owner(&self, session_id: &str) -> Result<(), HubError> {
        let conn = self.db.connection().map_err(HubError::internal)?;
        conn.execute(
            "UPDATE owner_sessions SET revoked_at = ?2 WHERE session_id = ?1",
            params![session_id, now_utc()],
        )
        .map_err(HubError::internal)?;
        Ok(())
    }
    fn owner_by_persisted_tailscale_identity(
        &self,
        tailscale_identity: &str,
    ) -> Result<Option<OwnerRecord>, HubError> {
        let conn = self.db.connection().map_err(HubError::internal)?;
        conn.query_row(
            r#"
            SELECT o.owner_id, o.username, o.password_hash, o.time_zone
            FROM owner_tailscale_identities i
            JOIN owners o ON o.owner_id = i.owner_id
            WHERE i.tailscale_identity = ?1
            "#,
            params![tailscale_identity],
            |row| {
                Ok(OwnerRecord {
                    owner_id: row.get(0)?,
                    username: row.get(1)?,
                    password_hash: row.get(2)?,
                    time_zone: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(HubError::internal)
    }

    fn owner_by_username(&self, username: &str) -> Result<OwnerRecord, HubError> {
        let username = validate_non_empty(username, "username")?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        conn.query_row(
            r#"
            SELECT owner_id, username, password_hash, time_zone
            FROM owners
            WHERE username = ?1
            "#,
            params![username],
            |row| {
                Ok(OwnerRecord {
                    owner_id: row.get(0)?,
                    username: row.get(1)?,
                    password_hash: row.get(2)?,
                    time_zone: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(HubError::internal)?
        .ok_or_else(|| {
            HubError::unauthorized("owner-auth-required", "owner credentials are invalid")
        })
    }

    fn issue_owner_session(
        &self,
        owner: &OwnerRecord,
        trusted_tailscale_user: Option<String>,
    ) -> Result<IssuedOwnerSession, HubError> {
        let session_id = random_token(24);
        let csrf_token = random_token(24);
        let csrf_hash = sha256_hex(&csrf_token);
        let now = now_utc();
        let expires_at = plus_seconds(&now, OWNER_SESSION_TTL_SECONDS)?;
        let conn = self.db.connection().map_err(HubError::internal)?;
        conn.execute(
            r#"
            INSERT INTO owner_sessions(session_id, owner_id, csrf_token_hash, trusted_tailscale_user, created_at, last_seen_at, expires_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)
            "#,
            params![
                session_id,
                owner.owner_id,
                csrf_hash,
                trusted_tailscale_user,
                now,
                expires_at,
            ],
        )
        .map_err(HubError::internal)?;
        Ok(IssuedOwnerSession {
            session_id,
            owner_username: owner.username.clone(),
            time_zone: owner.time_zone.clone(),
            csrf_token,
            trusted_tailscale_user,
        })
    }
}
