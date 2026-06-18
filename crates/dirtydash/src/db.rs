use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::importers::{
    self, DetectedSource, ReasoningEffort, ReasoningTurn, SourceKind, UsageEvent,
};
use crate::pricing::{PricingMode, PricingRecord};

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
    pub standard_tokens: u64,
    pub priority_tokens: u64,
    pub priority_estimated_cost_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CacheStats {
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_share: f64,
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
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
    pub standard_tokens: u64,
    pub priority_tokens: u64,
    pub priority_estimated_cost_usd: f64,
    pub reasoning: Vec<ReasoningBucket>,
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
    pub reasoning: Vec<ReasoningBucket>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReasoningBucket {
    pub effort: String,
    pub tokens: u64,
    pub estimated_cost_usd: f64,
    pub share: f64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageEventWrite {
    Inserted,
    Updated,
    Skipped,
}

#[derive(Debug)]
struct UsageEventPricingState {
    provider: String,
    model: String,
    turn_id: Option<String>,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    estimated_cost_usd: f64,
    confidence: f64,
    pricing_version: String,
    pricing_mode: String,
    reasoning_effort: String,
    raw_reasoning_effort: Option<String>,
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
                turn_id TEXT,
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_effort TEXT NOT NULL DEFAULT 'unknown',
                raw_reasoning_effort TEXT,
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
                pricing_mode TEXT NOT NULL DEFAULT 'unpriced',
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

            CREATE TABLE IF NOT EXISTS reasoning_turns (
                machine TEXT NOT NULL,
                source TEXT NOT NULL,
                session_id TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                project_path TEXT NOT NULL,
                model TEXT NOT NULL,
                reasoning_effort TEXT NOT NULL DEFAULT 'unknown',
                raw_reasoning_effort TEXT,
                event_timestamp TEXT,
                raw_path TEXT NOT NULL,
                raw_span TEXT,
                imported_at TEXT NOT NULL,
                PRIMARY KEY(machine, source, session_id, turn_id)
            );

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
        self.ensure_usage_event_columns(&conn)?;
        conn.execute_batch(
            r#"
            CREATE INDEX IF NOT EXISTS idx_usage_events_turn
                ON usage_events(turn_id);
            CREATE INDEX IF NOT EXISTS idx_usage_events_pricing_mode
                ON usage_events(pricing_mode);
            CREATE INDEX IF NOT EXISTS idx_usage_events_reasoning_effort
                ON usage_events(reasoning_effort);
            CREATE INDEX IF NOT EXISTS idx_usage_events_time_reasoning_effort
                ON usage_events(event_timestamp, reasoning_effort);
            CREATE INDEX IF NOT EXISTS idx_usage_events_session_reasoning_effort
                ON usage_events(machine, source, session_id, reasoning_effort);
            CREATE TABLE IF NOT EXISTS reasoning_turns (
                machine TEXT NOT NULL,
                source TEXT NOT NULL,
                session_id TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                project_path TEXT NOT NULL,
                model TEXT NOT NULL,
                reasoning_effort TEXT NOT NULL DEFAULT 'unknown',
                raw_reasoning_effort TEXT,
                event_timestamp TEXT,
                raw_path TEXT NOT NULL,
                raw_span TEXT,
                imported_at TEXT NOT NULL,
                PRIMARY KEY(machine, source, session_id, turn_id)
            );
            "#,
        )?;
        Ok(())
    }

    fn ensure_usage_event_columns(&self, conn: &Connection) -> Result<()> {
        let columns = table_columns(conn, "usage_events")?;
        if !columns.iter().any(|column| column == "turn_id") {
            conn.execute("ALTER TABLE usage_events ADD COLUMN turn_id TEXT", [])?;
        }
        if !columns.iter().any(|column| column == "pricing_mode") {
            conn.execute(
                "ALTER TABLE usage_events ADD COLUMN pricing_mode TEXT NOT NULL DEFAULT 'unpriced'",
                [],
            )?;
        }
        if !columns.iter().any(|column| column == "reasoning_effort") {
            conn.execute(
                "ALTER TABLE usage_events ADD COLUMN reasoning_effort TEXT NOT NULL DEFAULT 'unknown'",
                [],
            )?;
        }
        if !columns
            .iter()
            .any(|column| column == "raw_reasoning_effort")
        {
            conn.execute(
                "ALTER TABLE usage_events ADD COLUMN raw_reasoning_effort TEXT",
                [],
            )?;
        }
        conn.execute(
            "UPDATE usage_events SET reasoning_effort = 'unknown' WHERE reasoning_effort IS NULL OR reasoning_effort = ''",
            [],
        )?;
        Ok(())
    }

    pub fn upsert_usage_event(&self, event: &UsageEvent) -> Result<UsageEventWrite> {
        let conn = self.connection()?;
        let existing = conn
            .query_row(
                r#"
                SELECT provider, model, turn_id, prompt_tokens, completion_tokens, cache_read_tokens,
                    cache_write_tokens, reasoning_tokens, total_tokens, estimated_cost_usd,
                    confidence, pricing_version, pricing_mode,
                    COALESCE(reasoning_effort, 'unknown'),
                    raw_reasoning_effort
                FROM usage_events
                WHERE raw_event_hash = ?1
                "#,
                params![event.raw_event_hash],
                |row| {
                    Ok(UsageEventPricingState {
                        provider: row.get(0)?,
                        model: row.get(1)?,
                        turn_id: row.get(2)?,
                        prompt_tokens: row.get::<_, i64>(3)? as u64,
                        completion_tokens: row.get::<_, i64>(4)? as u64,
                        cache_read_tokens: row.get::<_, i64>(5)? as u64,
                        cache_write_tokens: row.get::<_, i64>(6)? as u64,
                        reasoning_tokens: row.get::<_, i64>(7)? as u64,
                        total_tokens: row.get::<_, i64>(8)? as u64,
                        estimated_cost_usd: row.get(9)?,
                        confidence: row.get(10)?,
                        pricing_version: row.get(11)?,
                        pricing_mode: row.get(12)?,
                        reasoning_effort: row.get(13)?,
                        raw_reasoning_effort: row.get(14)?,
                    })
                },
            )
            .optional()?;

        if let Some(existing) = existing {
            if existing.matches(event) {
                return Ok(UsageEventWrite::Skipped);
            }
            conn.execute(
                r#"
                UPDATE usage_events
                SET provider = ?1,
                    model = ?2,
                    turn_id = ?3,
                    prompt_tokens = ?4,
                    completion_tokens = ?5,
                    cache_read_tokens = ?6,
                    cache_write_tokens = ?7,
                    reasoning_tokens = ?8,
                    total_tokens = ?9,
                    estimated_cost_usd = ?10,
                    confidence = ?11,
                    parser_version = ?12,
                    imported_at = ?13,
                    pricing_version = ?14,
                    pricing_mode = ?15,
                    metadata_only = ?16,
                    reasoning_effort = ?17,
                    raw_reasoning_effort = ?18
                WHERE raw_event_hash = ?19
                "#,
                params![
                    event.provider,
                    event.model,
                    event.turn_id,
                    event.prompt_tokens,
                    event.completion_tokens,
                    event.cache_read_tokens,
                    event.cache_write_tokens,
                    event.reasoning_tokens,
                    event.total_tokens,
                    event.estimated_cost_usd,
                    event.confidence,
                    event.parser_version,
                    event.imported_at,
                    event.pricing_version,
                    event.pricing_mode.as_str(),
                    if event.metadata_only { 1 } else { 0 },
                    event.reasoning_effort.as_str(),
                    event.raw_reasoning_effort,
                    event.raw_event_hash,
                ],
            )?;
            return Ok(UsageEventWrite::Updated);
        }

        let changed = conn.execute(
            r#"
            INSERT INTO usage_events (
                machine, source, project_path, session_id, turn_id, provider, model,
                prompt_tokens, completion_tokens, cache_read_tokens, cache_write_tokens,
                reasoning_tokens, total_tokens, reasoning_effort, raw_reasoning_effort,
                estimated_cost_usd, confidence,
                event_timestamp, raw_path, raw_span, parser_name, parser_version,
                raw_event_hash, imported_at, pricing_version, pricing_mode, metadata_only
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)
            "#,
            params![
                event.machine,
                event.source.as_str(),
                event.project_path,
                event.session_id,
                event.turn_id,
                event.provider,
                event.model,
                event.prompt_tokens,
                event.completion_tokens,
                event.cache_read_tokens,
                event.cache_write_tokens,
                event.reasoning_tokens,
                event.total_tokens,
                event.reasoning_effort.as_str(),
                event.raw_reasoning_effort,
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
                event.pricing_mode.as_str(),
                if event.metadata_only { 1 } else { 0 },
            ],
        )?;
        Ok(if changed > 0 {
            UsageEventWrite::Inserted
        } else {
            UsageEventWrite::Skipped
        })
    }

    pub fn upsert_reasoning_turn(&self, turn: &ReasoningTurn) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            INSERT INTO reasoning_turns (
                machine, source, session_id, turn_id, project_path, model,
                reasoning_effort, raw_reasoning_effort, event_timestamp, raw_path,
                raw_span, imported_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(machine, source, session_id, turn_id) DO UPDATE SET
                project_path = excluded.project_path,
                model = excluded.model,
                reasoning_effort = CASE
                    WHEN excluded.reasoning_effort <> 'unknown' THEN excluded.reasoning_effort
                    ELSE reasoning_turns.reasoning_effort
                END,
                raw_reasoning_effort = CASE
                    WHEN excluded.reasoning_effort <> 'unknown' THEN excluded.raw_reasoning_effort
                    WHEN reasoning_turns.raw_reasoning_effort IS NULL THEN excluded.raw_reasoning_effort
                    ELSE reasoning_turns.raw_reasoning_effort
                END,
                event_timestamp = COALESCE(excluded.event_timestamp, reasoning_turns.event_timestamp),
                raw_path = excluded.raw_path,
                raw_span = excluded.raw_span,
                imported_at = excluded.imported_at
            "#,
            params![
                turn.machine,
                turn.source.as_str(),
                turn.session_id,
                turn.turn_id,
                turn.project_path,
                turn.model,
                turn.reasoning_effort.as_str(),
                turn.raw_reasoning_effort,
                turn.event_timestamp,
                turn.raw_path,
                turn.raw_span,
                turn.imported_at,
            ],
        )?;
        Ok(())
    }

    pub fn delete_non_overridden_pricing_records(&self, records: &[(&str, &str)]) -> Result<()> {
        let conn = self.connection()?;
        for (provider, model) in records {
            conn.execute(
                r#"
                DELETE FROM pricing_records
                WHERE provider = ?1
                    AND model = ?2
                    AND override_flag = 0
                    AND local_free_flag = 0
                "#,
                params![provider, model],
            )?;
        }
        Ok(())
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
                    updated_at = excluded.updated_at
                WHERE pricing_records.override_flag = 0
                    AND pricing_records.local_free_flag = 0
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
        let provider_candidates = pricing_provider_candidates(provider);
        let model_candidates = pricing_model_candidates(model);

        for provider_candidate in provider_candidates {
            for model_candidate in &model_candidates {
                let record = conn
                    .query_row(
                        r#"
                        SELECT provider, model, input_rate, output_rate, cache_read_rate,
                            cache_write_rate, source_label, snapshot_version, override_flag,
                            local_free_flag, updated_at
                        FROM pricing_records
                        WHERE provider = ?1 AND model = ?2
                        "#,
                        params![provider_candidate, model_candidate],
                        pricing_from_row,
                    )
                    .optional()?;
                if record.is_some() {
                    return Ok(record);
                }
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
            by_model: self.grouped_model_usage(20)?,
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
        let mut rows = stmt
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
                    reasoning: Vec::new(),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let buckets = self.session_reasoning_buckets()?;
        for row in &mut rows {
            let key = session_reasoning_key(
                &row.machine,
                &row.source,
                &row.session_id,
                &row.project_path,
                &row.provider,
                &row.model,
            );
            row.reasoning = buckets.get(&key).cloned().unwrap_or_default();
        }
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
                COALESCE(SUM(estimated_cost_usd), 0),
                COALESCE(SUM(CASE WHEN pricing_mode = 'priority' THEN 0 ELSE total_tokens END), 0),
                COALESCE(SUM(CASE WHEN pricing_mode = 'priority' THEN total_tokens ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN pricing_mode = 'priority' THEN estimated_cost_usd ELSE 0 END), 0)
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
                    standard_tokens: row.get::<_, i64>(7)? as u64,
                    priority_tokens: row.get::<_, i64>(8)? as u64,
                    priority_estimated_cost_usd: row.get(9)?,
                })
            },
        )
        .context("querying usage totals")
    }

    fn cache_stats(&self) -> Result<CacheStats> {
        let totals = self.usage_totals()?;
        let cache_input = totals.cache_read_tokens + totals.cache_write_tokens;
        let denominator = totals.prompt_tokens + cache_input;
        let cache_read_share = if denominator == 0 {
            0.0
        } else {
            totals.cache_read_tokens as f64 / denominator as f64
        };
        Ok(CacheStats {
            cache_read_tokens: totals.cache_read_tokens,
            cache_write_tokens: totals.cache_write_tokens,
            cache_read_share,
            hit_ratio: cache_read_share,
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
                COALESCE(SUM(reasoning_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(estimated_cost_usd), 0),
                COALESCE(SUM(CASE WHEN pricing_mode = 'priority' THEN 0 ELSE total_tokens END), 0),
                COALESCE(SUM(CASE WHEN pricing_mode = 'priority' THEN total_tokens ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN pricing_mode = 'priority' THEN estimated_cost_usd ELSE 0 END), 0)
            FROM usage_events
            GROUP BY name
            ORDER BY estimated_cost_usd DESC, total_tokens DESC
            LIMIT ?1
            "#
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(NamedUsagePoint {
                    name: row.get(0)?,
                    prompt_tokens: row.get::<_, i64>(1)? as u64,
                    completion_tokens: row.get::<_, i64>(2)? as u64,
                    cache_read_tokens: row.get::<_, i64>(3)? as u64,
                    cache_write_tokens: row.get::<_, i64>(4)? as u64,
                    reasoning_tokens: row.get::<_, i64>(5)? as u64,
                    total_tokens: row.get::<_, i64>(6)? as u64,
                    estimated_cost_usd: row.get(7)?,
                    standard_tokens: row.get::<_, i64>(8)? as u64,
                    priority_tokens: row.get::<_, i64>(9)? as u64,
                    priority_estimated_cost_usd: row.get(10)?,
                    reasoning: Vec::new(),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let buckets = self.named_reasoning_buckets(expression)?;
        for row in &mut rows {
            row.reasoning = buckets.get(&row.name).cloned().unwrap_or_default();
        }
        Ok(rows)
    }

    fn grouped_model_usage(&self, limit: usize) -> Result<Vec<NamedUsagePoint>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT provider,
                model,
                COALESCE(SUM(prompt_tokens), 0),
                COALESCE(SUM(completion_tokens), 0),
                COALESCE(SUM(cache_read_tokens), 0),
                COALESCE(SUM(cache_write_tokens), 0),
                COALESCE(SUM(reasoning_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(estimated_cost_usd), 0),
                COALESCE(SUM(CASE WHEN pricing_mode = 'priority' THEN 0 ELSE total_tokens END), 0),
                COALESCE(SUM(CASE WHEN pricing_mode = 'priority' THEN total_tokens ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN pricing_mode = 'priority' THEN estimated_cost_usd ELSE 0 END), 0)
            FROM usage_events
            GROUP BY provider, model
            "#,
        )?;
        let mut rows = stmt
            .query_map([], |row| {
                Ok(NamedUsagePoint {
                    name: canonical_model_label(row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                    prompt_tokens: row.get::<_, i64>(2)? as u64,
                    completion_tokens: row.get::<_, i64>(3)? as u64,
                    cache_read_tokens: row.get::<_, i64>(4)? as u64,
                    cache_write_tokens: row.get::<_, i64>(5)? as u64,
                    reasoning_tokens: row.get::<_, i64>(6)? as u64,
                    total_tokens: row.get::<_, i64>(7)? as u64,
                    estimated_cost_usd: row.get(8)?,
                    standard_tokens: row.get::<_, i64>(9)? as u64,
                    priority_tokens: row.get::<_, i64>(10)? as u64,
                    priority_estimated_cost_usd: row.get(11)?,
                    reasoning: Vec::new(),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut merged = Vec::<NamedUsagePoint>::new();
        for row in rows.drain(..) {
            if let Some(existing) = merged.iter_mut().find(|existing| existing.name == row.name) {
                existing.prompt_tokens += row.prompt_tokens;
                existing.completion_tokens += row.completion_tokens;
                existing.cache_read_tokens += row.cache_read_tokens;
                existing.cache_write_tokens += row.cache_write_tokens;
                existing.reasoning_tokens += row.reasoning_tokens;
                existing.total_tokens += row.total_tokens;
                existing.estimated_cost_usd += row.estimated_cost_usd;
                existing.standard_tokens += row.standard_tokens;
                existing.priority_tokens += row.priority_tokens;
                existing.priority_estimated_cost_usd += row.priority_estimated_cost_usd;
                existing.reasoning = merge_reasoning_buckets(&existing.reasoning, &row.reasoning);
            } else {
                merged.push(row);
            }
        }
        let buckets = self.model_reasoning_buckets()?;
        for row in &mut merged {
            row.reasoning = buckets.get(&row.name).cloned().unwrap_or_default();
        }

        merged.sort_by(|a, b| {
            b.estimated_cost_usd
                .partial_cmp(&a.estimated_cost_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.total_tokens.cmp(&a.total_tokens))
                .then_with(|| a.name.cmp(&b.name))
        });
        merged.truncate(limit);
        Ok(merged)
    }

    fn named_reasoning_buckets(
        &self,
        expression: &str,
    ) -> Result<HashMap<String, Vec<ReasoningBucket>>> {
        let conn = self.connection()?;
        let sql = format!(
            r#"
            SELECT COALESCE({expression}, 'unknown') AS name,
                COALESCE(reasoning_effort, 'unknown') AS reasoning_effort,
                COALESCE(SUM(total_tokens), 0) AS tokens,
                COALESCE(SUM(estimated_cost_usd), 0) AS estimated_cost_usd
            FROM usage_events
            GROUP BY name, COALESCE(reasoning_effort, 'unknown')
            "#
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    ReasoningEffort::from_db(&row.get::<_, String>(1)?),
                    row.get::<_, i64>(2)? as u64,
                    row.get::<_, f64>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(build_reasoning_bucket_map(rows))
    }

    fn model_reasoning_buckets(&self) -> Result<HashMap<String, Vec<ReasoningBucket>>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT provider, model, COALESCE(reasoning_effort, 'unknown') AS reasoning_effort,
                COALESCE(SUM(total_tokens), 0) AS tokens,
                COALESCE(SUM(estimated_cost_usd), 0) AS estimated_cost_usd
            FROM usage_events
            GROUP BY provider, model, COALESCE(reasoning_effort, 'unknown')
            "#,
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    canonical_model_label(row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                    ReasoningEffort::from_db(&row.get::<_, String>(2)?),
                    row.get::<_, i64>(3)? as u64,
                    row.get::<_, f64>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(build_reasoning_bucket_map(rows))
    }

    fn session_reasoning_buckets(&self) -> Result<HashMap<String, Vec<ReasoningBucket>>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT machine, source, session_id, project_path, provider, model,
                COALESCE(reasoning_effort, 'unknown') AS reasoning_effort,
                COALESCE(SUM(total_tokens), 0) AS tokens,
                COALESCE(SUM(estimated_cost_usd), 0) AS estimated_cost_usd
            FROM usage_events
            GROUP BY machine, source, session_id, project_path, provider, model,
                COALESCE(reasoning_effort, 'unknown')
            "#,
        )?;
        let rows = stmt
            .query_map([], |row| {
                let key = session_reasoning_key(
                    &row.get::<_, String>(0)?,
                    &row.get::<_, String>(1)?,
                    &row.get::<_, String>(2)?,
                    &row.get::<_, String>(3)?,
                    &row.get::<_, String>(4)?,
                    &row.get::<_, String>(5)?,
                );
                Ok((
                    key,
                    ReasoningEffort::from_db(&row.get::<_, String>(6)?),
                    row.get::<_, i64>(7)? as u64,
                    row.get::<_, f64>(8)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(build_reasoning_bucket_map(rows))
    }
}

impl UsageEventPricingState {
    fn matches(&self, event: &UsageEvent) -> bool {
        self.provider == event.provider
            && self.model == event.model
            && self.turn_id == event.turn_id
            && self.prompt_tokens == event.prompt_tokens
            && self.completion_tokens == event.completion_tokens
            && self.cache_read_tokens == event.cache_read_tokens
            && self.cache_write_tokens == event.cache_write_tokens
            && self.reasoning_tokens == event.reasoning_tokens
            && self.total_tokens == event.total_tokens
            && (self.estimated_cost_usd - event.estimated_cost_usd).abs() < 0.0000001
            && (self.confidence - event.confidence).abs() < 0.0000001
            && self.pricing_version == event.pricing_version
            && PricingMode::from_db(&self.pricing_mode) == event.pricing_mode
            && ReasoningEffort::from_db(&self.reasoning_effort) == event.reasoning_effort
            && self.raw_reasoning_effort == event.raw_reasoning_effort
    }
}

fn build_reasoning_bucket_map(
    rows: Vec<(String, ReasoningEffort, u64, f64)>,
) -> HashMap<String, Vec<ReasoningBucket>> {
    let mut grouped = HashMap::<String, Vec<(ReasoningEffort, u64, f64)>>::new();
    for (key, effort, tokens, cost) in rows {
        if tokens > 0 {
            grouped.entry(key).or_default().push((effort, tokens, cost));
        }
    }

    grouped
        .into_iter()
        .map(|(key, mut buckets)| {
            buckets.sort_by_key(|(effort, _, _)| reasoning_sort_index(*effort));
            let total_tokens: u64 = buckets.iter().map(|(_, tokens, _)| *tokens).sum();
            let values = if total_tokens == 0 {
                Vec::new()
            } else {
                buckets
                    .into_iter()
                    .map(|(effort, tokens, estimated_cost_usd)| ReasoningBucket {
                        effort: effort.as_str().to_string(),
                        tokens,
                        estimated_cost_usd,
                        share: tokens as f64 / total_tokens as f64,
                    })
                    .collect()
            };
            (key, values)
        })
        .collect()
}

fn merge_reasoning_buckets(
    left: &[ReasoningBucket],
    right: &[ReasoningBucket],
) -> Vec<ReasoningBucket> {
    let mut rows = Vec::new();
    for bucket in left.iter().chain(right) {
        rows.push((
            "merged".to_string(),
            ReasoningEffort::from_db(&bucket.effort),
            bucket.tokens,
            bucket.estimated_cost_usd,
        ));
    }
    build_reasoning_bucket_map(rows)
        .remove("merged")
        .unwrap_or_default()
}

fn reasoning_sort_index(effort: ReasoningEffort) -> usize {
    match effort {
        ReasoningEffort::None => 0,
        ReasoningEffort::Low => 1,
        ReasoningEffort::Medium => 2,
        ReasoningEffort::High => 3,
        ReasoningEffort::XHigh => 4,
        ReasoningEffort::Unknown => 5,
    }
}

fn session_reasoning_key(
    machine: &str,
    source: &str,
    session_id: &str,
    project_path: &str,
    provider: &str,
    model: &str,
) -> String {
    [machine, source, session_id, project_path, provider, model].join("\u{1f}")
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

fn pricing_provider_candidates(provider: &str) -> Vec<String> {
    let normalized = provider.trim().to_ascii_lowercase();
    let mut candidates = vec![normalized.clone()];
    if matches!(
        normalized.as_str(),
        "openai-codex" | "openai-code" | "codex" | "codex-openai"
    ) {
        candidates.push("openai".to_string());
    }
    dedupe(candidates)
}

fn pricing_model_candidates(model: &str) -> Vec<String> {
    let normalized = model.trim().to_string();
    let mut candidates = vec![normalized.clone()];
    if let Some(dot_version) = cursor_doc_slug_to_model(&normalized) {
        candidates.push(dot_version);
    }
    if let Some(stripped) = strip_version_suffix(&normalized) {
        candidates.push(stripped);
    }
    if let Some(stripped) = normalized.strip_suffix("-spark") {
        candidates.push(stripped.to_string());
    }
    dedupe(candidates)
}

fn canonical_model_label(_provider: String, model: String) -> String {
    let model = model.trim();
    if model.is_empty() {
        "unknown".to_string()
    } else {
        model.to_string()
    }
}

fn dedupe(values: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    for value in values {
        if !value.trim().is_empty() && !deduped.contains(&value) {
            deduped.push(value);
        }
    }
    deduped
}

fn count_row(conn: &Connection, sql: &str) -> Result<u64> {
    Ok(conn.query_row(sql, [], |row| row.get::<_, i64>(0))? as u64)
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(columns)
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

fn cursor_doc_slug_to_model(model: &str) -> Option<String> {
    let parts: Vec<&str> = model.split('-').collect();
    if parts.len() < 3 {
        return None;
    }
    let major = parts[0];
    let minor = parts[1];
    let patch = parts[2];
    if !major.chars().all(|c| c.is_ascii_alphabetic())
        || !minor.chars().all(|c| c.is_ascii_digit())
        || !patch.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let suffix = if parts.len() > 3 {
        format!("-{}", parts[3..].join("-"))
    } else {
        String::new()
    };
    Some(format!("{major}-{minor}.{patch}{suffix}"))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn model_summary_hides_provider_and_exposes_priority_split() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("dirtydash.sqlite3")).unwrap();
        db.migrate().unwrap();

        db.upsert_usage_event(&event(
            "openai",
            "gpt-5.5",
            1_000,
            "hash-1",
            PricingMode::Standard,
        ))
        .unwrap();
        db.upsert_usage_event(&event(
            "openai-codex",
            "gpt-5.5",
            2_000,
            "hash-2",
            PricingMode::Standard,
        ))
        .unwrap();
        db.upsert_usage_event(&event(
            "openai",
            "gpt-5.5",
            3_000,
            "hash-3",
            PricingMode::Priority,
        ))
        .unwrap();

        let summary = db.dashboard_summary().unwrap();
        let model = summary
            .by_model
            .iter()
            .find(|row| row.name == "gpt-5.5")
            .expect("fast model row should be present");

        assert_eq!(model.total_tokens, 6_000);
        assert_eq!(model.standard_tokens, 3_000);
        assert_eq!(model.priority_tokens, 3_000);
        assert_eq!(model.priority_estimated_cost_usd, 0.003);
        assert!(summary.by_model.iter().all(|row| !row.name.contains('/')));
    }

    #[test]
    fn migration_adds_reasoning_columns_and_turn_table_to_old_database() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("dirtydash.sqlite3");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE usage_events (
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
                "#,
            )
            .unwrap();
        }

        let db = Database::open(&path).unwrap();
        db.migrate().unwrap();
        let conn = db.connection().unwrap();
        let usage_columns = table_columns(&conn, "usage_events").unwrap();

        assert!(usage_columns.contains(&"turn_id".to_string()));
        assert!(usage_columns.contains(&"pricing_mode".to_string()));
        assert!(usage_columns.contains(&"reasoning_effort".to_string()));
        assert!(usage_columns.contains(&"raw_reasoning_effort".to_string()));
        assert!(table_columns(&conn, "reasoning_turns")
            .unwrap()
            .contains(&"turn_id".to_string()));
    }

    #[test]
    fn daily_and_session_rollups_include_reasoning_buckets() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("dirtydash.sqlite3")).unwrap();
        db.migrate().unwrap();

        let mut high = event(
            "openai",
            "gpt-5.5",
            300,
            "reasoning-high",
            PricingMode::Standard,
        );
        high.session_id = "mixed-session".to_string();
        high.event_timestamp = Some("2026-06-07T12:00:00Z".to_string());
        high.reasoning_effort = ReasoningEffort::High;
        high.raw_reasoning_effort = Some("high".to_string());
        high.estimated_cost_usd = 0.30;
        db.upsert_usage_event(&high).unwrap();

        let mut low = event(
            "openai",
            "gpt-5.5",
            100,
            "reasoning-low",
            PricingMode::Standard,
        );
        low.session_id = "mixed-session".to_string();
        low.event_timestamp = Some("2026-06-07T12:05:00Z".to_string());
        low.reasoning_effort = ReasoningEffort::Low;
        low.raw_reasoning_effort = Some("low".to_string());
        low.estimated_cost_usd = 0.10;
        db.upsert_usage_event(&low).unwrap();

        let summary = db.dashboard_summary().unwrap();
        let day = summary
            .daily
            .iter()
            .find(|row| row.name == "2026-06-07")
            .expect("daily row exists");
        assert_eq!(day.total_tokens, 400);
        assert_eq!(day.reasoning.len(), 2);
        assert_eq!(day.reasoning[0].effort, "low");
        assert_eq!(day.reasoning[0].tokens, 100);
        assert!((day.reasoning[0].share - 0.25).abs() < 0.000001);
        assert_eq!(day.reasoning[1].effort, "high");
        assert_eq!(day.reasoning[1].tokens, 300);
        assert!((day.reasoning[1].share - 0.75).abs() < 0.000001);

        let session = db
            .sessions(10)
            .unwrap()
            .into_iter()
            .find(|row| row.session_id == "mixed-session")
            .expect("session row exists");
        assert_eq!(session.reasoning.len(), 2);
        assert_eq!(session.reasoning[0].effort, "low");
        assert_eq!(session.reasoning[1].effort, "high");
    }

    fn event(
        provider: &str,
        model: &str,
        tokens: u64,
        hash: &str,
        pricing_mode: PricingMode,
    ) -> UsageEvent {
        UsageEvent {
            machine: "test-machine".to_string(),
            source: importers::SourceKind::Codex,
            project_path: "/repo".to_string(),
            session_id: format!("session-{hash}"),
            turn_id: Some(format!("turn-{hash}")),
            provider: provider.to_string(),
            model: model.to_string(),
            prompt_tokens: tokens,
            completion_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: tokens,
            reasoning_effort: ReasoningEffort::Unknown,
            raw_reasoning_effort: None,
            estimated_cost_usd: tokens as f64 / 1_000_000.0,
            confidence: 0.92,
            event_timestamp: None,
            raw_path: "/tmp/session.jsonl".to_string(),
            raw_span: None,
            parser_name: "test-parser".to_string(),
            parser_version: "test".to_string(),
            raw_event_hash: hash.to_string(),
            imported_at: Utc::now().to_rfc3339(),
            pricing_version: "test-pricing".to_string(),
            pricing_mode,
            metadata_only: true,
        }
    }
}
