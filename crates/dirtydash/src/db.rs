use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::importers::{self, DetectedSource, SourceKind, UsageEvent};
use crate::pricing::PricingRecord;

#[derive(Debug, Clone)]
pub struct Database {
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceFileRecord {
    pub source: SourceKind,
    pub path: PathBuf,
    pub machine: String,
    pub file_count_hint: u64,
    pub parse_error: Option<String>,
    pub last_imported_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub event_count: u64,
    pub pricing_count: u64,
    pub detected_sources: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardSummary {
    pub totals: UsageTotals,
    pub cache: CacheStats,
    pub daily: Vec<NamedUsagePoint>,
    pub by_source: Vec<NamedUsagePoint>,
    pub by_model: Vec<NamedUsagePoint>,
    pub by_project: Vec<NamedUsagePoint>,
    pub expensive_sessions: Vec<SessionSummary>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageTotals {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CacheStats {
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub hit_ratio: f64,
    pub estimated_savings_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NamedUsagePoint {
    pub name: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub machine: String,
    pub source: String,
    pub session_id: String,
    pub project_path: String,
    pub provider: String,
    pub model: String,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
    pub confidence: f64,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub raw_path: String,
    pub parser_name: String,
    pub pricing_version: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceSummary {
    pub source: String,
    pub machine: String,
    pub files: u64,
    pub parse_errors: u64,
    pub last_imported_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteRow {
    pub name: String,
    pub ssh_target: String,
    pub source_roots_json: String,
    pub last_sync_at: Option<String>,
    pub last_error: Option<String>,
    pub last_file_count: u64,
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating database directory {}", parent.display()))?;
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn connection(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path)
            .with_context(|| format!("opening SQLite database {}", self.path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(conn)
    }

    pub fn migrate(&self) -> Result<()> {
        let conn = self.connection()?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS usage_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                machine TEXT NOT NULL,
                source TEXT NOT NULL,
                project_path TEXT NOT NULL,
                session_id TEXT NOT NULL,
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost_usd REAL NOT NULL DEFAULT 0,
                confidence REAL NOT NULL DEFAULT 0,
                event_timestamp TEXT,
                raw_path TEXT NOT NULL,
                raw_span TEXT,
                parser_name TEXT NOT NULL,
                parser_version TEXT NOT NULL,
                raw_event_hash TEXT NOT NULL UNIQUE,
                imported_at TEXT NOT NULL,
                pricing_version TEXT NOT NULL,
                metadata_only INTEGER NOT NULL DEFAULT 1
            );

            CREATE INDEX IF NOT EXISTS idx_usage_events_source
                ON usage_events(source, machine);
            CREATE INDEX IF NOT EXISTS idx_usage_events_project
                ON usage_events(project_path);
            CREATE INDEX IF NOT EXISTS idx_usage_events_model
                ON usage_events(provider, model);
            CREATE INDEX IF NOT EXISTS idx_usage_events_session
                ON usage_events(machine, source, session_id);
            CREATE INDEX IF NOT EXISTS idx_usage_events_time
                ON usage_events(event_timestamp);

            CREATE TABLE IF NOT EXISTS source_files (
                source TEXT NOT NULL,
                path TEXT NOT NULL,
                machine TEXT NOT NULL,
                file_count_hint INTEGER NOT NULL DEFAULT 0,
                parse_error TEXT,
                last_imported_at TEXT NOT NULL,
                PRIMARY KEY(source, path, machine)
            );

            CREATE TABLE IF NOT EXISTS pricing_records (
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                input_rate REAL NOT NULL,
                output_rate REAL NOT NULL,
                cache_read_rate REAL NOT NULL,
                cache_write_rate REAL NOT NULL,
                source_label TEXT NOT NULL,
                snapshot_version TEXT NOT NULL,
                override_flag INTEGER NOT NULL DEFAULT 0,
                local_free_flag INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL,
                PRIMARY KEY(provider, model)
            );

            CREATE TABLE IF NOT EXISTS remotes (
                name TEXT PRIMARY KEY,
                ssh_target TEXT NOT NULL,
                source_roots_json TEXT NOT NULL DEFAULT '[]',
                last_sync_at TEXT,
                last_error TEXT,
                last_file_count INTEGER NOT NULL DEFAULT 0
            );

            INSERT OR IGNORE INTO schema_migrations(version, applied_at)
            VALUES (1, datetime('now'));
            "#,
        )
        .context("applying SQLite migrations")?;
        Ok(())
    }

    pub fn insert_usage_event(&self, event: &UsageEvent) -> Result<bool> {
        let conn = self.connection()?;
        let changed = conn.execute(
            r#"
            INSERT OR IGNORE INTO usage_events (
                machine, source, project_path, session_id, provider, model,
                prompt_tokens, completion_tokens, cache_read_tokens, cache_write_tokens,
                reasoning_tokens, total_tokens, estimated_cost_usd, confidence,
                event_timestamp, raw_path, raw_span, parser_name, parser_version,
                raw_event_hash, imported_at, pricing_version, metadata_only
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
            "#,
            params![
                event.machine,
                event.source.as_str(),
                event.project_path,
                event.session_id,
                event.provider,
                event.model,
                event.prompt_tokens,
                event.completion_tokens,
                event.cache_read_tokens,
                event.cache_write_tokens,
                event.reasoning_tokens,
                event.total_tokens,
                event.estimated_cost_usd,
                event.confidence,
                event.event_timestamp,
                event.raw_path,
                event.raw_span,
                event.parser_name,
                event.parser_version,
                event.raw_event_hash,
                event.imported_at,
                event.pricing_version,
                if event.metadata_only { 1 } else { 0 },
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn upsert_source_file(&self, record: &SourceFileRecord) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            INSERT INTO source_files (
                source, path, machine, file_count_hint, parse_error, last_imported_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(source, path, machine) DO UPDATE SET
                file_count_hint = excluded.file_count_hint,
                parse_error = excluded.parse_error,
                last_imported_at = excluded.last_imported_at
            "#,
            params![
                record.source.as_str(),
                record.path.display().to_string(),
                record.machine,
                record.file_count_hint,
                record.parse_error,
                record.last_imported_at,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_pricing_record(&self, record: &PricingRecord, replace: bool) -> Result<()> {
        let conn = self.connection()?;
        let override_flag = if record.override_flag { 1 } else { 0 };
        let local_free_flag = if record.local_free_flag { 1 } else { 0 };
        if replace {
            conn.execute(
                r#"
                INSERT INTO pricing_records (
                    provider, model, input_rate, output_rate, cache_read_rate, cache_write_rate,
                    source_label, snapshot_version, override_flag, local_free_flag, updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT(provider, model) DO UPDATE SET
                    input_rate = excluded.input_rate,
                    output_rate = excluded.output_rate,
                    cache_read_rate = excluded.cache_read_rate,
                    cache_write_rate = excluded.cache_write_rate,
                    source_label = excluded.source_label,
                    snapshot_version = excluded.snapshot_version,
                    override_flag = excluded.override_flag,
                    local_free_flag = excluded.local_free_flag,
                    updated_at = excluded.updated_at
                "#,
                params![
                    &record.provider,
                    &record.model,
                    record.input_rate,
                    record.output_rate,
                    record.cache_read_rate,
                    record.cache_write_rate,
                    &record.source_label,
                    &record.snapshot_version,
                    override_flag,
                    local_free_flag,
                    &record.updated_at,
                ],
            )?;
        } else {
            conn.execute(
                r#"
                INSERT OR IGNORE INTO pricing_records (
                    provider, model, input_rate, output_rate, cache_read_rate, cache_write_rate,
                    source_label, snapshot_version, override_flag, local_free_flag, updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                "#,
                params![
                    &record.provider,
                    &record.model,
                    record.input_rate,
                    record.output_rate,
                    record.cache_read_rate,
                    record.cache_write_rate,
                    &record.source_label,
                    &record.snapshot_version,
                    override_flag,
                    local_free_flag,
                    &record.updated_at,
                ],
            )?;
        }
        Ok(())
    }

    pub fn pricing_record(&self, provider: &str, model: &str) -> Result<Option<PricingRecord>> {
        let conn = self.connection()?;
        let mut candidates = vec![model.to_string()];
        if let Some(stripped) = strip_version_suffix(model) {
            candidates.push(stripped);
        }

        for candidate in candidates {
            let record = conn
                .query_row(
                    r#"
                    SELECT provider, model, input_rate, output_rate, cache_read_rate,
                        cache_write_rate, source_label, snapshot_version, override_flag,
                        local_free_flag, updated_at
                    FROM pricing_records
                    WHERE provider = ?1 AND model = ?2
                    "#,
                    params![provider, candidate],
                    pricing_from_row,
                )
                .optional()?;
            if record.is_some() {
                return Ok(record);
            }
        }
        Ok(None)
    }

    pub fn list_pricing(&self, provider: Option<&str>) -> Result<Vec<PricingRecord>> {
        let conn = self.connection()?;
        let sql = if provider.is_some() {
            r#"
            SELECT provider, model, input_rate, output_rate, cache_read_rate,
                cache_write_rate, source_label, snapshot_version, override_flag,
                local_free_flag, updated_at
            FROM pricing_records
            WHERE provider = ?1
            ORDER BY provider, model
            "#
        } else {
            r#"
            SELECT provider, model, input_rate, output_rate, cache_read_rate,
                cache_write_rate, source_label, snapshot_version, override_flag,
                local_free_flag, updated_at
            FROM pricing_records
            ORDER BY provider, model
            "#
        };

        let mut rows = if let Some(provider) = provider {
            conn.prepare(sql)?
                .query_map(params![provider], pricing_from_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            conn.prepare(sql)?
                .query_map([], pricing_from_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        rows.sort_by(|a, b| (&a.provider, &a.model).cmp(&(&b.provider, &b.model)));
        Ok(rows)
    }

    pub fn doctor(&self, config: &Config) -> Result<DoctorReport> {
        let conn = self.connection()?;
        let event_count = count_row(&conn, "SELECT COUNT(*) FROM usage_events")?;
        let pricing_count = count_row(&conn, "SELECT COUNT(*) FROM pricing_records")?;
        let detected = importers::scan_sources(config)?;
        let mut warnings = Vec::new();

        if pricing_count == 0 {
            warnings.push("no pricing records are available".to_string());
        }
        if detected.iter().all(|source| source.file_count == 0) {
            warnings.push("no local usage source files were detected".to_string());
        }

        Ok(DoctorReport {
            event_count,
            pricing_count,
            detected_sources: detected
                .iter()
                .filter(|source| source.path.exists() && source.file_count > 0)
                .count(),
            warnings,
        })
    }

    pub fn dashboard_summary(&self) -> Result<DashboardSummary> {
        Ok(DashboardSummary {
            totals: self.usage_totals()?,
            cache: self.cache_stats()?,
            daily: self.grouped_usage("date(COALESCE(event_timestamp, imported_at))", 30)?,
            by_source: self.grouped_usage("source", 20)?,
            by_model: self.grouped_usage("provider || '/' || model", 20)?,
            by_project: self.grouped_usage("project_path", 20)?,
            expensive_sessions: self.sessions(12)?,
        })
    }

    pub fn source_summaries(&self) -> Result<Vec<SourceSummary>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT source, machine, COUNT(*) AS files,
                SUM(CASE WHEN parse_error IS NULL THEN 0 ELSE 1 END) AS parse_errors,
                MAX(last_imported_at) AS last_imported_at
            FROM source_files
            GROUP BY source, machine
            ORDER BY source, machine
            "#,
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SourceSummary {
                    source: row.get(0)?,
                    machine: row.get(1)?,
                    files: row.get::<_, i64>(2)? as u64,
                    parse_errors: row.get::<_, i64>(3)? as u64,
                    last_imported_at: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn sessions(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT machine, source, session_id, project_path, provider, model,
                SUM(total_tokens) AS total_tokens,
                SUM(estimated_cost_usd) AS estimated_cost_usd,
                AVG(confidence) AS confidence,
                MIN(event_timestamp) AS first_seen,
                MAX(event_timestamp) AS last_seen,
                MIN(raw_path) AS raw_path,
                MIN(parser_name) AS parser_name,
                MIN(pricing_version) AS pricing_version
            FROM usage_events
            GROUP BY machine, source, session_id, project_path, provider, model
            ORDER BY estimated_cost_usd DESC, total_tokens DESC
            LIMIT ?1
            "#,
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(SessionSummary {
                    machine: row.get(0)?,
                    source: row.get(1)?,
                    session_id: row.get(2)?,
                    project_path: row.get(3)?,
                    provider: row.get(4)?,
                    model: row.get(5)?,
                    total_tokens: row.get::<_, i64>(6)? as u64,
                    estimated_cost_usd: row.get(7)?,
                    confidence: row.get(8)?,
                    first_seen: row.get(9)?,
                    last_seen: row.get(10)?,
                    raw_path: row.get(11)?,
                    parser_name: row.get(12)?,
                    pricing_version: row.get(13)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn add_remote(&self, name: &str, ssh_target: &str, source_roots_json: &str) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            INSERT INTO remotes(name, ssh_target, source_roots_json)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(name) DO UPDATE SET
                ssh_target = excluded.ssh_target,
                source_roots_json = excluded.source_roots_json
            "#,
            params![name, ssh_target, source_roots_json],
        )?;
        Ok(())
    }

    pub fn remove_remote(&self, name: &str) -> Result<()> {
        let conn = self.connection()?;
        conn.execute("DELETE FROM remotes WHERE name = ?1", params![name])?;
        Ok(())
    }

    pub fn update_remote_sync(
        &self,
        name: &str,
        file_count: u64,
        error: Option<&str>,
    ) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            UPDATE remotes
            SET last_sync_at = ?2, last_error = ?3, last_file_count = ?4
            WHERE name = ?1
            "#,
            params![name, Utc::now().to_rfc3339(), error, file_count],
        )?;
        Ok(())
    }

    pub fn list_remotes(&self) -> Result<Vec<RemoteRow>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT name, ssh_target, source_roots_json, last_sync_at, last_error, last_file_count
            FROM remotes
            ORDER BY name
            "#,
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(RemoteRow {
                    name: row.get(0)?,
                    ssh_target: row.get(1)?,
                    source_roots_json: row.get(2)?,
                    last_sync_at: row.get(3)?,
                    last_error: row.get(4)?,
                    last_file_count: row.get::<_, i64>(5)? as u64,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn detected_to_source_files(&self, sources: &[DetectedSource]) -> Result<()> {
        let machine = local_machine();
        let imported_at = Utc::now().to_rfc3339();
        for source in sources {
            self.upsert_source_file(&SourceFileRecord {
                source: source.kind,
                path: source.path.clone(),
                machine: machine.clone(),
                file_count_hint: source.file_count,
                parse_error: None,
                last_imported_at: imported_at.clone(),
            })?;
        }
        Ok(())
    }

    fn usage_totals(&self) -> Result<UsageTotals> {
        let conn = self.connection()?;
        conn.query_row(
            r#"
            SELECT COALESCE(SUM(prompt_tokens), 0),
                COALESCE(SUM(completion_tokens), 0),
                COALESCE(SUM(cache_read_tokens), 0),
                COALESCE(SUM(cache_write_tokens), 0),
                COALESCE(SUM(reasoning_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(estimated_cost_usd), 0)
            FROM usage_events
            "#,
            [],
            |row| {
                Ok(UsageTotals {
                    prompt_tokens: row.get::<_, i64>(0)? as u64,
                    completion_tokens: row.get::<_, i64>(1)? as u64,
                    cache_read_tokens: row.get::<_, i64>(2)? as u64,
                    cache_write_tokens: row.get::<_, i64>(3)? as u64,
                    reasoning_tokens: row.get::<_, i64>(4)? as u64,
                    total_tokens: row.get::<_, i64>(5)? as u64,
                    estimated_cost_usd: row.get(6)?,
                })
            },
        )
        .context("querying usage totals")
    }

    fn cache_stats(&self) -> Result<CacheStats> {
        let totals = self.usage_totals()?;
        let cache_input = totals.cache_read_tokens + totals.cache_write_tokens;
        let denominator = totals.prompt_tokens + cache_input;
        let hit_ratio = if denominator == 0 {
            0.0
        } else {
            totals.cache_read_tokens as f64 / denominator as f64
        };
        Ok(CacheStats {
            cache_read_tokens: totals.cache_read_tokens,
            cache_write_tokens: totals.cache_write_tokens,
            hit_ratio,
            estimated_savings_usd: 0.0,
        })
    }

    fn grouped_usage(&self, expression: &str, limit: usize) -> Result<Vec<NamedUsagePoint>> {
        let conn = self.connection()?;
        let sql = format!(
            r#"
            SELECT COALESCE({expression}, 'unknown') AS name,
                COALESCE(SUM(prompt_tokens), 0),
                COALESCE(SUM(completion_tokens), 0),
                COALESCE(SUM(cache_read_tokens), 0),
                COALESCE(SUM(cache_write_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(estimated_cost_usd), 0)
            FROM usage_events
            GROUP BY name
            ORDER BY estimated_cost_usd DESC, total_tokens DESC
            LIMIT ?1
            "#
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(NamedUsagePoint {
                    name: row.get(0)?,
                    prompt_tokens: row.get::<_, i64>(1)? as u64,
                    completion_tokens: row.get::<_, i64>(2)? as u64,
                    cache_read_tokens: row.get::<_, i64>(3)? as u64,
                    cache_write_tokens: row.get::<_, i64>(4)? as u64,
                    total_tokens: row.get::<_, i64>(5)? as u64,
                    estimated_cost_usd: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn pricing_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PricingRecord> {
    Ok(PricingRecord {
        provider: row.get(0)?,
        model: row.get(1)?,
        input_rate: row.get(2)?,
        output_rate: row.get(3)?,
        cache_read_rate: row.get(4)?,
        cache_write_rate: row.get(5)?,
        source_label: row.get(6)?,
        snapshot_version: row.get(7)?,
        override_flag: row.get::<_, i64>(8)? != 0,
        local_free_flag: row.get::<_, i64>(9)? != 0,
        updated_at: row.get(10)?,
    })
}

fn count_row(conn: &Connection, sql: &str) -> Result<u64> {
    Ok(conn.query_row(sql, [], |row| row.get::<_, i64>(0))? as u64)
}

pub fn local_machine() -> String {
    hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "local".to_string())
}

fn strip_version_suffix(model: &str) -> Option<String> {
    let parts: Vec<&str> = model.split('-').collect();
    if parts.len() < 2 {
        return None;
    }
    let last = parts.last()?;
    if last.len() == 8 && last.chars().all(|c| c.is_ascii_digit()) {
        Some(parts[..parts.len() - 1].join("-"))
    } else if parts.len() > 4 {
        let tail = &parts[parts.len() - 3..];
        let looks_like_date = tail[0].len() == 4
            && tail[1].len() == 2
            && tail[2].len() == 2
            && tail
                .iter()
                .all(|part| part.chars().all(|c| c.is_ascii_digit()));
        if looks_like_date {
            Some(parts[..parts.len() - 3].join("-"))
        } else {
            None
        }
    } else {
        None
    }
}
