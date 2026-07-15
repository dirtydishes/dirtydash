use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use directories::BaseDirs;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::config::Config;
use crate::db::{local_machine, Database, SourceFileRecord};
use crate::pricing::{self, PricingMode};

/// Compatibility label retained for the local import report. Collector
/// payloads use the per-parser versions returned by [`SourceKind::parser_version`].
pub const PARSER_VERSION: &str = "dirtydash-v1.0.0";
pub const CLAUDE_CODE_PARSER_VERSION: &str = "claude-code-v1";
pub const CODEX_PARSER_VERSION: &str = "codex-v1";
pub const OPENCODE_PARSER_VERSION: &str = "opencode-v1";
pub const PI_AGENT_PARSER_VERSION: &str = "pi-agent-v1";
pub const HERMES_AGENT_PARSER_VERSION: &str = "hermes-agent-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    ClaudeCode,
    Codex,
    OpenCode,
    PiAgent,
    HermesAgent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedSource {
    pub kind: SourceKind,
    pub path: PathBuf,
    pub confidence: String,
    pub file_count: u64,
    pub harness_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ImportOptions {
    pub metadata_only: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportReport {
    pub files_seen: u64,
    pub inserted_events: u64,
    pub updated_existing_events: u64,
    pub skipped_existing_events: u64,
    pub parse_errors: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParserDescriptor {
    pub source: SourceKind,
    pub parser_name: String,
    pub parser_version: String,
}

#[derive(Debug, Clone)]
pub struct CollectorParsedFile {
    pub source: DetectedSource,
    pub file: PathBuf,
    pub file_fingerprint: String,
    pub events: Vec<UsageEvent>,
    pub parse_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CollectorParserRegistry;

impl CollectorParserRegistry {
    pub fn descriptors(self) -> Vec<ParserDescriptor> {
        SourceKind::all()
            .into_iter()
            .map(|source| ParserDescriptor {
                source,
                parser_name: source.parser_name().to_string(),
                parser_version: source.parser_version().to_string(),
            })
            .collect()
    }

    /// Parse one local source artifact without writing usage rows or retaining
    /// its body. Pricing lookup is read-only; the caller owns persistence.
    pub fn parse_file(
        self,
        db: &Database,
        source: &DetectedSource,
        file: &Path,
        machine: &str,
        imported_at: &str,
    ) -> Result<CollectorParsedFile> {
        parse_source_file_for_collector(db, source, file, machine, imported_at)
    }
}

pub type ParserRegistry = CollectorParserRegistry;

pub fn parser_registry() -> CollectorParserRegistry {
    CollectorParserRegistry
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageNumbers {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub machine: String,
    pub source: SourceKind,
    pub project_path: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
    pub confidence: f64,
    pub event_timestamp: Option<String>,
    pub raw_path: String,
    pub raw_span: Option<String>,
    pub parser_name: String,
    pub parser_version: String,
    pub raw_event_hash: String,
    pub imported_at: String,
    pub pricing_version: String,
    pub pricing_mode: PricingMode,
    pub metadata_only: bool,
}

#[derive(Debug, Clone)]
struct ParsedFile {
    events: Vec<UsageEvent>,
    parse_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct CodexPriorityEvidence {
    turns: HashMap<String, CodexPriorityTurnEvidence>,
}

#[derive(Debug, Clone, Default)]
struct CodexPriorityTurnEvidence {
    model: Option<String>,
}

impl CodexPriorityEvidence {
    fn load(codex_source_path: &Path) -> Self {
        for candidate in codex_trace_db_candidates(codex_source_path) {
            if !candidate.exists() {
                continue;
            }
            if let Ok(turns) = load_priority_turns_from_trace_db(&candidate) {
                return CodexPriorityEvidence { turns };
            }
        }
        CodexPriorityEvidence::default()
    }

    fn is_priority(&self, turn_id: &str) -> bool {
        self.turns.contains_key(turn_id)
    }

    fn pricing_model(&self, turn_id: &str) -> Option<&str> {
        self.turns.get(turn_id)?.model.as_deref()
    }
}

fn codex_trace_db_candidates(codex_source_path: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(paths) = env_paths("CODEX_HOME") {
        for path in paths {
            candidates.push(path.join("logs_2.sqlite"));
        }
    }
    if codex_source_path.ends_with("sessions") || codex_source_path.ends_with("archived_sessions") {
        if let Some(parent) = codex_source_path.parent() {
            candidates.push(parent.join("logs_2.sqlite"));
        }
    }
    if let Some(base_dirs) = BaseDirs::new() {
        candidates.push(base_dirs.home_dir().join(".codex/logs_2.sqlite"));
    }

    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|path| seen.insert(path.display().to_string()))
        .collect()
}

fn load_priority_turns_from_trace_db(
    path: &Path,
) -> Result<HashMap<String, CodexPriorityTurnEvidence>> {
    let conn = Connection::open(path)
        .with_context(|| format!("opening Codex trace DB {}", path.display()))?;
    let mut stmt = conn.prepare(
        r#"
        SELECT feedback_log_body
        FROM logs
        WHERE feedback_log_body LIKE '%websocket request:%'
            OR feedback_log_body LIKE '%response.completed%'
        "#,
    )?;
    let rows = stmt
        .query_map([], |row| row.get::<_, Option<String>>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut turns = HashMap::<String, CodexPriorityTurnEvidence>::new();
    let mut completed_models = HashMap::<String, String>::new();
    for body in rows.into_iter().flatten() {
        if let Some((turn_id, model)) = priority_turn_from_feedback_body(&body) {
            turns.insert(turn_id, CodexPriorityTurnEvidence { model });
            continue;
        }
        if let Some((turn_id, model)) = completed_model_from_feedback_body(&body) {
            completed_models.insert(turn_id, model);
        }
    }
    for (turn_id, model) in completed_models {
        if let Some(turn) = turns.get_mut(&turn_id) {
            turn.model = Some(model);
        }
    }
    Ok(turns)
}

fn priority_turn_from_feedback_body(body: &str) -> Option<(String, Option<String>)> {
    let marker = "websocket request:";
    let request_json = body.get(body.find(marker)? + marker.len()..)?.trim_start();
    let mut stream = serde_json::Deserializer::from_str(request_json).into_iter::<Value>();
    let request = stream.next()?.ok()?;
    let service_tier = request.get("service_tier").and_then(Value::as_str)?;
    if service_tier != "priority" {
        return None;
    }

    let turn_id = turn_id_from_codex_request(&request)
        .or_else(|| span_value(body, "turn.id="))
        .or_else(|| span_value(body, "turn_id="))?;
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned);
    Some((turn_id, model))
}

fn completed_model_from_feedback_body(body: &str) -> Option<(String, String)> {
    let marker = "websocket event:";
    let event_json = body.get(body.find(marker)? + marker.len()..)?.trim_start();
    let mut stream = serde_json::Deserializer::from_str(event_json).into_iter::<Value>();
    let event = stream.next()?.ok()?;
    if event.get("type").and_then(Value::as_str)? != "response.completed" {
        return None;
    }
    let model = event
        .pointer("/response/model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())?
        .to_string();
    let turn_id = span_value(body, "turn.id=").or_else(|| span_value(body, "turn_id="))?;
    Some((turn_id, model))
}

fn turn_id_from_codex_request(request: &Value) -> Option<String> {
    if let Some(turn_id) = extract_string(request, TURN_ID_KEYS) {
        return Some(turn_id);
    }
    let metadata = request
        .pointer("/client_metadata/x-codex-turn-metadata")
        .and_then(Value::as_str)?;
    serde_json::from_str::<Value>(metadata)
        .ok()
        .and_then(|value| extract_string(&value, TURN_ID_KEYS))
}

fn span_value(body: &str, key: &str) -> Option<String> {
    let start = body.find(key)? + key.len();
    let rest = body.get(start..)?;
    let value: String = rest
        .chars()
        .take_while(|ch| !ch.is_whitespace() && *ch != ',' && *ch != ']' && *ch != ')')
        .collect();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

impl SourceKind {
    pub const fn all() -> [Self; 5] {
        [
            SourceKind::ClaudeCode,
            SourceKind::Codex,
            SourceKind::OpenCode,
            SourceKind::PiAgent,
            SourceKind::HermesAgent,
        ]
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::ClaudeCode => "claude-code",
            SourceKind::Codex => "codex",
            SourceKind::OpenCode => "opencode",
            SourceKind::PiAgent => "pi-agent",
            SourceKind::HermesAgent => "hermes-agent",
        }
    }

    pub fn parser_name(self) -> &'static str {
        match self {
            SourceKind::ClaudeCode => "claude-code-jsonl",
            SourceKind::Codex => "codex-token-count-jsonl",
            SourceKind::OpenCode => "opencode-storage-json",
            SourceKind::PiAgent => "pi-agent-jsonl",
            SourceKind::HermesAgent => "hermes-agent-metering",
        }
    }

    pub fn parser_version(self) -> &'static str {
        match self {
            SourceKind::ClaudeCode => CLAUDE_CODE_PARSER_VERSION,
            SourceKind::Codex => CODEX_PARSER_VERSION,
            SourceKind::OpenCode => OPENCODE_PARSER_VERSION,
            SourceKind::PiAgent => PI_AGENT_PARSER_VERSION,
            SourceKind::HermesAgent => HERMES_AGENT_PARSER_VERSION,
        }
    }

    fn default_provider(self) -> &'static str {
        match self {
            SourceKind::ClaudeCode | SourceKind::PiAgent => "anthropic",
            SourceKind::Codex => "openai",
            SourceKind::OpenCode | SourceKind::HermesAgent => "unknown",
        }
    }

    fn harness_names(self) -> Vec<String> {
        match self {
            SourceKind::ClaudeCode => vec!["Claude Code".to_string(), "claude-code".to_string()],
            SourceKind::Codex => vec!["Codex CLI".to_string(), "codex".to_string()],
            SourceKind::OpenCode => vec!["OpenCode".to_string(), "opencode".to_string()],
            SourceKind::PiAgent => vec!["Pi".to_string(), "pi-agent".to_string()],
            SourceKind::HermesAgent => vec!["Hermes".to_string(), "hermes-agent".to_string()],
        }
    }
}

impl std::str::FromStr for SourceKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claudecode" => Ok(SourceKind::ClaudeCode),
            "codex" | "codex-cli" => Ok(SourceKind::Codex),
            "opencode" | "open-code" => Ok(SourceKind::OpenCode),
            "pi" | "pi-agent" | "pi_agent" => Ok(SourceKind::PiAgent),
            "hermes" | "hermes-agent" | "hermes_agent" => Ok(SourceKind::HermesAgent),
            other => anyhow::bail!("unknown source kind {other}"),
        }
    }
}

impl UsageNumbers {
    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens
            + self.completion_tokens
            + self.cache_read_tokens
            + self.cache_write_tokens
            + self.reasoning_tokens
    }

    fn has_usage(&self) -> bool {
        self.total_tokens() > 0
    }

    fn saturating_delta(&self, previous: &UsageNumbers) -> UsageNumbers {
        UsageNumbers {
            prompt_tokens: self.prompt_tokens.saturating_sub(previous.prompt_tokens),
            completion_tokens: self
                .completion_tokens
                .saturating_sub(previous.completion_tokens),
            cache_read_tokens: self
                .cache_read_tokens
                .saturating_sub(previous.cache_read_tokens),
            cache_write_tokens: self
                .cache_write_tokens
                .saturating_sub(previous.cache_write_tokens),
            reasoning_tokens: self
                .reasoning_tokens
                .saturating_sub(previous.reasoning_tokens),
        }
    }
}

pub fn scan_sources(config: &Config) -> Result<Vec<DetectedSource>> {
    let mut candidates = default_candidates()?;
    candidates.extend(configured_candidates(config)?);
    detect_candidates(candidates)
}

/// Scan only explicitly configured roots. Collectors use this boundary so a
/// test or enrolled machine cannot accidentally walk unrelated home-directory
/// data while preserving the legacy CLI's default discovery behavior.
pub fn scan_configured_sources(config: &Config) -> Result<Vec<DetectedSource>> {
    detect_candidates(configured_candidates(config)?)
}

fn configured_candidates(config: &Config) -> Result<Vec<(SourceKind, PathBuf)>> {
    let mut candidates = Vec::new();
    for root in &config.source_roots {
        let kind: SourceKind = root.kind.parse()?;
        candidates.extend(normalize_source_paths(kind, root.path.clone()));
    }
    Ok(candidates)
}

fn detect_candidates(candidates: Vec<(SourceKind, PathBuf)>) -> Result<Vec<DetectedSource>> {
    let mut seen = HashSet::new();
    let mut detected = Vec::new();
    for (kind, path) in candidates {
        let key = (kind, path.display().to_string());
        if !seen.insert(key) {
            continue;
        }
        let file_count = matching_files(kind, &path)?.len() as u64;
        let confidence = if file_count > 0 {
            "high"
        } else if path.exists() {
            "medium"
        } else {
            "low"
        }
        .to_string();

        detected.push(DetectedSource {
            kind,
            path,
            confidence,
            file_count,
            harness_names: kind.harness_names(),
        });
    }

    detected.sort_by(|a, b| {
        b.file_count
            .cmp(&a.file_count)
            .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(detected)
}

pub fn import_detected(
    db: &Database,
    config: &Config,
    options: ImportOptions,
) -> Result<ImportReport> {
    let sources = scan_sources(config)?;
    import_sources(db, sources, options)
}

pub fn reclassify_codex_priority_events_from_trace_db(db: &Database) -> Result<usize> {
    let conn = db.connection()?;
    let mut stmt = conn.prepare(
        r#"
        SELECT id, provider, model, turn_id, raw_path, prompt_tokens, completion_tokens,
            cache_read_tokens, cache_write_tokens, reasoning_tokens
        FROM usage_events
        WHERE source = 'codex'
            AND turn_id IS NOT NULL
            AND pricing_mode <> 'priority'
            AND pricing_version NOT LIKE 'manual%'
            AND pricing_version <> 'reported-cost'
        "#,
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                UsageNumbers {
                    prompt_tokens: row.get::<_, i64>(5)? as u64,
                    completion_tokens: row.get::<_, i64>(6)? as u64,
                    cache_read_tokens: row.get::<_, i64>(7)? as u64,
                    cache_write_tokens: row.get::<_, i64>(8)? as u64,
                    reasoning_tokens: row.get::<_, i64>(9)? as u64,
                },
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);

    let mut evidence_by_trace = HashMap::<PathBuf, CodexPriorityEvidence>::new();
    let mut updated = 0;
    for (id, provider, model, turn_id, raw_path, usage) in rows {
        let Some(trace_db) = trace_db_for_imported_codex_path(&raw_path) else {
            continue;
        };
        if !evidence_by_trace.contains_key(&trace_db) {
            let turns = load_priority_turns_from_trace_db(&trace_db).unwrap_or_default();
            let evidence = CodexPriorityEvidence { turns };
            evidence_by_trace.insert(trace_db.clone(), evidence);
        }
        let Some(evidence) = evidence_by_trace.get(&trace_db) else {
            continue;
        };
        if !evidence.is_priority(&turn_id) {
            continue;
        }

        let pricing_model = evidence.pricing_model(&turn_id).unwrap_or(model.as_str());
        let mut estimate = pricing::estimate_cost(
            db,
            &provider,
            pricing_model,
            &usage,
            Some(PricingMode::Priority),
        )?;
        if !estimate.priced && pricing_model != model {
            estimate =
                pricing::estimate_cost(db, &provider, &model, &usage, Some(PricingMode::Priority))?;
        }
        conn.execute(
            r#"
            UPDATE usage_events
            SET total_tokens = ?1,
                estimated_cost_usd = ?2,
                pricing_version = ?3,
                pricing_mode = ?4
            WHERE id = ?5
            "#,
            rusqlite::params![
                usage.total_tokens(),
                estimate.estimated_cost_usd,
                estimate.pricing_version,
                estimate.pricing_mode.as_str(),
                id,
            ],
        )?;
        updated += 1;
    }

    Ok(updated)
}

fn trace_db_for_imported_codex_path(raw_path: &str) -> Option<PathBuf> {
    let path = Path::new(raw_path);
    path.ancestors()
        .map(|ancestor| ancestor.join("logs_2.sqlite"))
        .find(|candidate| candidate.exists())
}

pub fn import_sources(
    db: &Database,
    sources: Vec<DetectedSource>,
    options: ImportOptions,
) -> Result<ImportReport> {
    db.detected_to_source_files(&sources)?;
    let mut report = ImportReport::default();
    let machine = local_machine();
    let imported_at = Utc::now().to_rfc3339();

    for source in sources {
        if source.file_count == 0 {
            continue;
        }
        let codex_priority_evidence = if source.kind == SourceKind::Codex {
            CodexPriorityEvidence::load(&source.path)
        } else {
            CodexPriorityEvidence::default()
        };

        for file in matching_files(source.kind, &source.path)? {
            report.files_seen += 1;
            let parsed = parse_file(
                db,
                &source,
                &file,
                &machine,
                &imported_at,
                options,
                Some(&codex_priority_evidence),
            )
            .unwrap_or_else(|error| ParsedFile {
                events: Vec::new(),
                parse_error: Some(error.to_string()),
            });

            if parsed.parse_error.is_some() {
                report.parse_errors += 1;
            }

            db.upsert_source_file(&SourceFileRecord {
                source: source.kind,
                path: file.clone(),
                machine: machine.clone(),
                file_count_hint: 1,
                parse_error: parsed.parse_error.clone(),
                last_imported_at: imported_at.clone(),
            })?;

            for event in parsed.events {
                match db.upsert_usage_event(&event)? {
                    crate::db::UsageEventWrite::Inserted => report.inserted_events += 1,
                    crate::db::UsageEventWrite::Updated => report.updated_existing_events += 1,
                    crate::db::UsageEventWrite::Skipped => report.skipped_existing_events += 1,
                }
            }
        }
    }

    Ok(report)
}

fn default_candidates() -> Result<Vec<(SourceKind, PathBuf)>> {
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    let home = base_dirs.home_dir();
    let mut candidates = Vec::new();

    let claude_roots = env_paths("CLAUDE_CONFIG_DIR").unwrap_or_else(|| {
        vec![
            home.join(".config/claude/projects"),
            home.join(".claude/projects"),
        ]
    });
    for root in claude_roots {
        candidates.push((
            SourceKind::ClaudeCode,
            normalize_source_path(SourceKind::ClaudeCode, root),
        ));
    }

    let codex_roots = env_paths("CODEX_HOME").unwrap_or_else(|| vec![home.join(".codex")]);
    for root in codex_roots {
        candidates.extend(normalize_source_paths(SourceKind::Codex, root));
    }

    let opencode_roots =
        env_paths("OPENCODE_DATA_DIR").unwrap_or_else(|| vec![home.join(".local/share/opencode")]);
    for root in opencode_roots {
        candidates.push((
            SourceKind::OpenCode,
            normalize_source_path(SourceKind::OpenCode, root),
        ));
    }

    let pi_roots =
        env_paths("PI_AGENT_DIR").unwrap_or_else(|| vec![home.join(".pi/agent/sessions")]);
    for root in pi_roots {
        candidates.push((
            SourceKind::PiAgent,
            normalize_source_path(SourceKind::PiAgent, root),
        ));
    }

    let hermes_roots = env_paths("HERMES_HOME").unwrap_or_else(|| vec![home.join(".hermes")]);
    for root in hermes_roots {
        candidates.extend(normalize_source_paths(SourceKind::HermesAgent, root));
    }

    Ok(candidates)
}

fn env_paths(name: &str) -> Option<Vec<PathBuf>> {
    env::var(name).ok().map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(expand_home)
            .collect()
    })
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Some(base_dirs) = BaseDirs::new() {
            return base_dirs.home_dir().join(stripped);
        }
    }
    PathBuf::from(path)
}

fn normalize_source_path(kind: SourceKind, path: PathBuf) -> PathBuf {
    match kind {
        SourceKind::ClaudeCode => {
            if path.ends_with("projects") {
                path
            } else if path.join("projects").exists() {
                path.join("projects")
            } else {
                path
            }
        }
        SourceKind::Codex => {
            if path.join("sessions").exists() {
                path.join("sessions")
            } else {
                path
            }
        }
        SourceKind::OpenCode => {
            if path.join("storage/message").exists() {
                path.join("storage/message")
            } else {
                path
            }
        }
        SourceKind::PiAgent => path,
        SourceKind::HermesAgent => {
            if path.ends_with("state.db") || path.ends_with("sessions") {
                path
            } else if path.join("state.db").exists() {
                path.join("state.db")
            } else if path.join("sessions").exists() {
                path.join("sessions")
            } else {
                path
            }
        }
    }
}

fn normalize_source_paths(kind: SourceKind, path: PathBuf) -> Vec<(SourceKind, PathBuf)> {
    if kind == SourceKind::HermesAgent {
        let mut paths = Vec::new();
        if path.ends_with("state.db") {
            paths.push(path.clone());
        } else if path.join("state.db").exists() {
            paths.push(path.join("state.db"));
        }
        for relative in [
            "sessions",
            "webui/sessions/_run_journal",
            "webui/sessions/_turn_journal",
        ] {
            let candidate = if path.ends_with("state.db") {
                path.parent()
                    .map(|parent| parent.join(relative))
                    .unwrap_or_else(|| path.join(relative))
            } else {
                path.join(relative)
            };
            if candidate.exists() {
                paths.push(candidate);
            }
        }
        if paths.is_empty() {
            paths.push(path);
        }
        return paths.into_iter().map(|path| (kind, path)).collect();
    }

    if kind != SourceKind::Codex {
        return vec![(kind, normalize_source_path(kind, path))];
    }

    let mut paths = Vec::new();
    if path.ends_with("sessions") {
        paths.push(path.clone());
        if let Some(parent) = path.parent() {
            let archived = parent.join("archived_sessions");
            if archived.exists() {
                paths.push(archived);
            }
        }
    } else if path.ends_with("archived_sessions") {
        paths.push(path);
    } else if path.join("sessions").exists() {
        paths.push(path.join("sessions"));
        let archived = path.join("archived_sessions");
        if archived.exists() {
            paths.push(archived);
        }
    } else {
        paths.push(path);
    }

    paths
        .into_iter()
        .map(|path| (SourceKind::Codex, path))
        .collect()
}

fn matching_files(kind: SourceKind, path: &Path) -> Result<Vec<PathBuf>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(path)
        .follow_links(false)
        .max_depth(match kind {
            SourceKind::OpenCode => 5,
            _ => 8,
        })
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let file = entry.path();
        if format_evidence(kind, file)? {
            files.push(file.to_path_buf());
        }
    }
    files.sort();
    Ok(files)
}

/// Detection is deliberately based on parseable format evidence rather than
/// a filename extension. A malformed line does not hide a sibling valid line,
/// while arbitrary `.jsonl`/`.json` files are not claimed as agent sources.
fn format_evidence(kind: SourceKind, file: &Path) -> Result<bool> {
    if kind == SourceKind::HermesAgent
        && file
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "state.db")
    {
        let mut header = [0_u8; 16];
        let mut handle = fs::File::open(file)?;
        use std::io::Read;
        let read = handle.read(&mut header)?;
        return Ok(read >= 15 && &header[..15] == b"SQLite format 3");
    }

    let extension = file.extension().and_then(|extension| extension.to_str());
    let is_jsonl = extension == Some("jsonl");
    let is_json = extension == Some("json");
    if !is_jsonl && !is_json {
        return Ok(false);
    }
    let raw = fs::read_to_string(file)?;
    if is_json {
        let value = match serde_json::from_str::<Value>(&raw) {
            Ok(value) => value,
            Err(_) => return Ok(false),
        };
        return Ok(value_is_source_evidence(kind, &value));
    }

    Ok(raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .any(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .is_some_and(|value| value_is_source_evidence(kind, &value))
        }))
}

fn value_is_source_evidence(kind: SourceKind, value: &Value) -> bool {
    if !extract_usage_numbers(value).has_usage() {
        let payload_type = value
            .pointer("/payload/type")
            .and_then(Value::as_str)
            .or_else(|| value.get("type").and_then(Value::as_str));
        if kind == SourceKind::Codex
            && !matches!(payload_type, Some("token_count" | "turn_context"))
        {
            return false;
        }
        if kind == SourceKind::HermesAgent
            && !matches!(
                payload_type,
                Some("metering" | "usage" | "session" | "turn")
            )
        {
            return false;
        }
    }
    true
}

pub fn parse_source_file_for_collector(
    db: &Database,
    source: &DetectedSource,
    file: &Path,
    machine: &str,
    imported_at: &str,
) -> Result<CollectorParsedFile> {
    let codex_priority_evidence = if source.kind == SourceKind::Codex {
        CodexPriorityEvidence::load(&source.path)
    } else {
        CodexPriorityEvidence::default()
    };
    let parsed = parse_file(
        db,
        source,
        file,
        machine,
        imported_at,
        ImportOptions {
            metadata_only: true,
        },
        Some(&codex_priority_evidence),
    )?;
    Ok(CollectorParsedFile {
        source: source.clone(),
        file: file.to_path_buf(),
        file_fingerprint: source_file_fingerprint(file)?,
        events: parsed.events,
        parse_error: parsed.parse_error,
    })
}

pub fn parse_sources_for_collector(
    db: &Database,
    sources: &[DetectedSource],
    machine: &str,
    imported_at: &str,
) -> Result<Vec<CollectorParsedFile>> {
    let mut parsed = Vec::new();
    for source in sources {
        for file in matching_files(source.kind, &source.path)? {
            parsed.push(parse_source_file_for_collector(
                db,
                source,
                &file,
                machine,
                imported_at,
            )?);
        }
    }
    Ok(parsed)
}

pub fn source_file_fingerprint(file: &Path) -> Result<String> {
    let bytes =
        fs::read(file).with_context(|| format!("reading source fingerprint {}", file.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn parse_file(
    db: &Database,
    source: &DetectedSource,
    file: &Path,
    machine: &str,
    imported_at: &str,
    options: ImportOptions,
    codex_priority_evidence: Option<&CodexPriorityEvidence>,
) -> Result<ParsedFile> {
    match source.kind {
        SourceKind::Codex => parse_codex_jsonl(
            db,
            source,
            file,
            machine,
            imported_at,
            options,
            codex_priority_evidence,
        ),
        SourceKind::ClaudeCode | SourceKind::PiAgent | SourceKind::HermesAgent => {
            if source.kind == SourceKind::HermesAgent
                && file.extension().and_then(|extension| extension.to_str()) == Some("db")
            {
                parse_hermes_state_db(db, source, file, machine, imported_at, options)
            } else {
                parse_generic_jsonl(db, source, file, machine, imported_at, options)
            }
        }
        SourceKind::OpenCode => parse_generic_json(db, source, file, machine, imported_at, options),
    }
}

fn parse_hermes_state_db(
    db: &Database,
    source: &DetectedSource,
    file: &Path,
    machine: &str,
    imported_at: &str,
    options: ImportOptions,
) -> Result<ParsedFile> {
    use rusqlite::OpenFlags;

    let connection = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening Hermes state database {}", file.display()))?;
    let has_sessions = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'sessions'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if !has_sessions {
        anyhow::bail!("Hermes state database has no sessions table");
    }

    let mut statement = connection.prepare("SELECT * FROM sessions")?;
    let columns = statement
        .column_names()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let mut events = Vec::new();
    let mut parse_error = None;
    let rows = statement.query_map([], |row| {
        let mut object = serde_json::Map::new();
        for (index, column) in columns.iter().enumerate() {
            let value = row.get_ref(index)?;
            let json = match value {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(value) => Value::from(value),
                rusqlite::types::ValueRef::Real(value) => Value::from(value),
                rusqlite::types::ValueRef::Text(value) => {
                    Value::String(String::from_utf8_lossy(value).into_owned())
                }
                rusqlite::types::ValueRef::Blob(_) => Value::Null,
            };
            object.insert(column.clone(), json);
        }
        Ok(Value::Object(object))
    })?;

    for (index, row) in rows.enumerate() {
        match row {
            Ok(value) => {
                if let Some(event) = event_from_value(
                    db,
                    source,
                    file,
                    Some(format!("row {}", index + 1)),
                    &value,
                    machine,
                    imported_at,
                    options,
                    None,
                    None,
                    None,
                    None,
                )? {
                    events.push(event);
                }
            }
            Err(error) => {
                if parse_error.is_none() {
                    parse_error = Some(error.to_string());
                }
            }
        }
    }

    Ok(ParsedFile {
        events,
        parse_error,
    })
}

fn parse_generic_jsonl(
    db: &Database,
    source: &DetectedSource,
    file: &Path,
    machine: &str,
    imported_at: &str,
    options: ImportOptions,
) -> Result<ParsedFile> {
    let raw = fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let mut parse_errors = Vec::new();
    let mut events = Vec::new();

    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(value) => {
                if let Some(event) = event_from_value(
                    db,
                    source,
                    file,
                    Some(format!("line {}", index + 1)),
                    &value,
                    machine,
                    imported_at,
                    options,
                    None,
                    None,
                    None,
                    None,
                )? {
                    events.push(event);
                }
            }
            Err(error) => parse_errors.push(format!("line {}: {error}", index + 1)),
        }
    }

    Ok(ParsedFile {
        events,
        parse_error: parse_errors.first().cloned(),
    })
}

fn parse_generic_json(
    db: &Database,
    source: &DetectedSource,
    file: &Path,
    machine: &str,
    imported_at: &str,
    options: ImportOptions,
) -> Result<ParsedFile> {
    let raw = fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let value = serde_json::from_str::<Value>(&raw)
        .with_context(|| format!("parsing JSON {}", file.display()))?;
    let event = event_from_value(
        db,
        source,
        file,
        Some("$.root".to_string()),
        &value,
        machine,
        imported_at,
        options,
        None,
        None,
        None,
        None,
    )?;

    Ok(ParsedFile {
        events: event.into_iter().collect(),
        parse_error: None,
    })
}

fn parse_codex_jsonl(
    db: &Database,
    source: &DetectedSource,
    file: &Path,
    machine: &str,
    imported_at: &str,
    options: ImportOptions,
    priority_evidence: Option<&CodexPriorityEvidence>,
) -> Result<ParsedFile> {
    let raw = fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let mut parse_errors = Vec::new();
    let mut events = Vec::new();
    let mut previous = UsageNumbers::default();
    let mut current_model: Option<String> = None;
    let mut current_provider: Option<String> = None;
    let mut current_turn_id: Option<String> = None;
    let mut current_reasoning_effort: Option<String> = None;

    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        let value = match serde_json::from_str::<Value>(line) {
            Ok(value) => value,
            Err(error) => {
                parse_errors.push(format!("line {}: {error}", index + 1));
                continue;
            }
        };

        if let Some(model) = extract_string(&value, MODEL_KEYS) {
            current_model = Some(model);
        }
        if let Some(provider) = extract_string(&value, PROVIDER_KEYS) {
            current_provider = Some(provider);
        }
        if let Some(turn_id) = extract_string(&value, TURN_ID_KEYS) {
            current_turn_id = Some(turn_id);
        }
        if let Some(reasoning_effort) = extract_reasoning_effort(&value) {
            current_reasoning_effort = Some(reasoning_effort);
        }

        let payload_type = value
            .pointer("/payload/type")
            .and_then(Value::as_str)
            .or_else(|| value.get("type").and_then(Value::as_str));

        if payload_type == Some("turn_context") {
            continue;
        }

        if payload_type == Some("token_count") {
            let usage_value = value.pointer("/payload").unwrap_or(&value);
            let (delta, current) = extract_codex_token_count_usage(usage_value, &previous);
            if let Some(current) = current {
                previous = current;
            }
            if !delta.has_usage() {
                continue;
            }

            if let Some(mut event) = event_from_usage(
                db,
                source,
                file,
                Some(format!("line {}", index + 1)),
                &value,
                delta,
                machine,
                imported_at,
                options,
                Some(current_model.as_deref().unwrap_or("gpt-5.5")),
                current_turn_id.as_deref(),
                current_reasoning_effort.as_deref(),
                priority_evidence,
            )? {
                event.provider = current_provider
                    .clone()
                    .unwrap_or_else(|| SourceKind::Codex.default_provider().to_string());
                events.push(event);
            }
            continue;
        }

        if let Some(event) = event_from_value(
            db,
            source,
            file,
            Some(format!("line {}", index + 1)),
            &value,
            machine,
            imported_at,
            options,
            current_model.as_deref(),
            current_turn_id.as_deref(),
            current_reasoning_effort.as_deref(),
            priority_evidence,
        )? {
            events.push(event);
        }
    }

    Ok(ParsedFile {
        events,
        parse_error: parse_errors.first().cloned(),
    })
}

#[allow(clippy::too_many_arguments)]
fn event_from_value(
    db: &Database,
    source: &DetectedSource,
    file: &Path,
    raw_span: Option<String>,
    value: &Value,
    machine: &str,
    imported_at: &str,
    options: ImportOptions,
    fallback_model: Option<&str>,
    fallback_turn_id: Option<&str>,
    fallback_reasoning_effort: Option<&str>,
    priority_evidence: Option<&CodexPriorityEvidence>,
) -> Result<Option<UsageEvent>> {
    let usage = extract_usage_numbers(value);
    if !usage.has_usage() {
        return Ok(None);
    }
    event_from_usage(
        db,
        source,
        file,
        raw_span,
        value,
        usage,
        machine,
        imported_at,
        options,
        fallback_model,
        fallback_turn_id,
        fallback_reasoning_effort,
        priority_evidence,
    )
}

#[allow(clippy::too_many_arguments)]
fn event_from_usage(
    db: &Database,
    source: &DetectedSource,
    file: &Path,
    raw_span: Option<String>,
    value: &Value,
    usage: UsageNumbers,
    machine: &str,
    imported_at: &str,
    options: ImportOptions,
    fallback_model: Option<&str>,
    fallback_turn_id: Option<&str>,
    fallback_reasoning_effort: Option<&str>,
    priority_evidence: Option<&CodexPriorityEvidence>,
) -> Result<Option<UsageEvent>> {
    if !usage.has_usage() {
        return Ok(None);
    }

    let provider = extract_string(value, PROVIDER_KEYS)
        .unwrap_or_else(|| source.kind.default_provider().to_string());
    let model = extract_string(value, MODEL_KEYS)
        .map(|model| model.trim().to_string())
        .or_else(|| fallback_model.map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_string());
    let session_id = extract_string(value, SESSION_KEYS).unwrap_or_else(|| {
        file.file_stem_string()
            .unwrap_or_else(|| "unknown-session".to_string())
    });
    let project_path =
        extract_string(value, PROJECT_KEYS).unwrap_or_else(|| infer_project_path(source, file));
    let turn_id =
        extract_string(value, TURN_ID_KEYS).or_else(|| fallback_turn_id.map(ToOwned::to_owned));
    let reasoning_effort = extract_reasoning_effort(value)
        .or_else(|| fallback_reasoning_effort.map(ToOwned::to_owned));
    let event_timestamp = extract_timestamp(value).or_else(|| file_modified_at(file));
    let reported_cost = extract_reported_cost(value);
    let requested_pricing_mode = if source.kind == SourceKind::Codex
        && turn_id.as_deref().is_some_and(|turn_id| {
            priority_evidence.is_some_and(|evidence| evidence.is_priority(turn_id))
        }) {
        Some(PricingMode::Priority)
    } else {
        None
    };
    let pricing_model = if requested_pricing_mode == Some(PricingMode::Priority) {
        turn_id
            .as_deref()
            .and_then(|turn_id| {
                priority_evidence.and_then(|evidence| evidence.pricing_model(turn_id))
            })
            .unwrap_or(model.as_str())
    } else {
        model.as_str()
    };
    let cost = if reported_cost.is_some() {
        None
    } else {
        Some(pricing::estimate_cost(
            db,
            &provider,
            pricing_model,
            &usage,
            requested_pricing_mode,
        )?)
    };
    let confidence = if reported_cost.is_some() {
        0.98
    } else if cost.as_ref().is_some_and(|estimate| estimate.priced) {
        0.92
    } else {
        0.62
    };
    let hash = raw_hash(source.kind, file, raw_span.as_deref(), value)?;

    Ok(Some(UsageEvent {
        machine: machine.to_string(),
        source: source.kind,
        project_path,
        session_id,
        turn_id,
        provider,
        model,
        reasoning_effort,
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        total_tokens: usage.total_tokens(),
        estimated_cost_usd: reported_cost
            .or_else(|| cost.as_ref().map(|estimate| estimate.estimated_cost_usd))
            .unwrap_or(0.0),
        confidence,
        event_timestamp,
        raw_path: file.display().to_string(),
        raw_span,
        parser_name: source.kind.parser_name().to_string(),
        parser_version: source.kind.parser_version().to_string(),
        raw_event_hash: hash,
        imported_at: imported_at.to_string(),
        pricing_version: reported_cost
            .map(|_| "reported-cost".to_string())
            .or_else(|| {
                cost.as_ref()
                    .map(|estimate| estimate.pricing_version.clone())
            })
            .unwrap_or_else(|| "unpriced".to_string()),
        pricing_mode: if reported_cost.is_some() {
            PricingMode::Reported
        } else {
            cost.as_ref()
                .map(|estimate| estimate.pricing_mode)
                .unwrap_or(PricingMode::Unpriced)
        },
        metadata_only: options.metadata_only,
    }))
}

fn extract_usage_numbers(value: &Value) -> UsageNumbers {
    if let Some(usage) = extract_direct_usage_numbers(value) {
        return usage;
    }

    UsageNumbers {
        prompt_tokens: extract_u64(value, PROMPT_KEYS).unwrap_or(0),
        completion_tokens: extract_u64(value, COMPLETION_KEYS).unwrap_or(0),
        cache_read_tokens: extract_u64(value, CACHE_READ_KEYS).unwrap_or(0),
        cache_write_tokens: extract_u64(value, CACHE_WRITE_KEYS).unwrap_or(0),
        reasoning_tokens: extract_u64(value, REASONING_KEYS).unwrap_or(0),
    }
}

fn extract_codex_token_count_usage(
    payload: &Value,
    previous: &UsageNumbers,
) -> (UsageNumbers, Option<UsageNumbers>) {
    let info = payload.get("info").unwrap_or(payload);
    let last_usage = info
        .get("last_token_usage")
        .map(extract_usage_numbers)
        .map(split_codex_cached_input);
    let total_usage = info
        .get("total_token_usage")
        .map(extract_usage_numbers)
        .map(split_codex_cached_input);

    match (last_usage, total_usage) {
        (Some(last), Some(total)) => (last, Some(total)),
        (Some(last), None) => (last, None),
        (None, Some(total)) => {
            let delta = total.saturating_delta(previous);
            (delta, Some(total))
        }
        (None, None) => {
            let current = split_codex_cached_input(extract_usage_numbers(payload));
            let delta = current.saturating_delta(previous);
            (delta, Some(current))
        }
    }
}

fn split_codex_cached_input(mut usage: UsageNumbers) -> UsageNumbers {
    let cached = usage.cache_read_tokens.min(usage.prompt_tokens);
    usage.prompt_tokens -= cached;
    usage.cache_read_tokens = cached;
    usage
}

fn extract_direct_usage_numbers(value: &Value) -> Option<UsageNumbers> {
    match value {
        Value::Object(map) => {
            if let Some(usage) = map.get("usage") {
                let usage = UsageNumbers {
                    prompt_tokens: extract_direct_u64(usage, DIRECT_PROMPT_KEYS).unwrap_or(0),
                    completion_tokens: extract_direct_u64(usage, DIRECT_COMPLETION_KEYS)
                        .unwrap_or(0),
                    cache_read_tokens: extract_direct_u64(usage, DIRECT_CACHE_READ_KEYS)
                        .unwrap_or(0),
                    cache_write_tokens: extract_direct_u64(usage, DIRECT_CACHE_WRITE_KEYS)
                        .unwrap_or(0),
                    reasoning_tokens: extract_direct_u64(usage, DIRECT_REASONING_KEYS).unwrap_or(0),
                };
                if usage.has_usage() {
                    return Some(usage);
                }
            }
            map.values().find_map(extract_direct_usage_numbers)
        }
        Value::Array(items) => items.iter().find_map(extract_direct_usage_numbers),
        _ => None,
    }
}

const DIRECT_PROMPT_KEYS: &[&str] = &[
    "input",
    "inputTokens",
    "input_tokens",
    "prompt_tokens",
    "promptTokens",
    "total_input_tokens",
    "totalInputTokens",
];
const DIRECT_COMPLETION_KEYS: &[&str] = &[
    "output",
    "outputTokens",
    "output_tokens",
    "completion_tokens",
    "completionTokens",
    "total_output_tokens",
    "totalOutputTokens",
];
const DIRECT_CACHE_READ_KEYS: &[&str] = &[
    "cacheRead",
    "cache_read_input_tokens",
    "cacheReadInputTokens",
    "cache_read_tokens",
    "cacheReadTokens",
    "cached_input_tokens",
    "cachedInputTokens",
];
const DIRECT_CACHE_WRITE_KEYS: &[&str] = &[
    "cacheWrite",
    "cache_creation_input_tokens",
    "cacheCreationInputTokens",
    "cache_write_tokens",
    "cacheWriteTokens",
    "cacheCreationTokens",
];
const DIRECT_REASONING_KEYS: &[&str] = &[
    "reasoning",
    "reasoning_tokens",
    "reasoningTokens",
    "reasoning_output_tokens",
    "reasoningOutputTokens",
];
const PROMPT_KEYS: &[&str] = &[
    "input_tokens",
    "inputTokens",
    "prompt_tokens",
    "promptTokens",
    "total_input_tokens",
    "totalInputTokens",
];
const COMPLETION_KEYS: &[&str] = &[
    "output_tokens",
    "outputTokens",
    "completion_tokens",
    "completionTokens",
    "total_output_tokens",
    "totalOutputTokens",
];
const CACHE_READ_KEYS: &[&str] = &[
    "cache_read_input_tokens",
    "cacheReadInputTokens",
    "cache_read_tokens",
    "cacheReadTokens",
    "cached_input_tokens",
    "cachedInputTokens",
    "cacheRead",
];
const CACHE_WRITE_KEYS: &[&str] = &[
    "cache_creation_input_tokens",
    "cacheCreationInputTokens",
    "cache_write_tokens",
    "cacheWriteTokens",
    "cacheCreationTokens",
    "cacheWrite",
];
const REASONING_KEYS: &[&str] = &[
    "reasoning_tokens",
    "reasoningTokens",
    "reasoning_output_tokens",
    "reasoningOutputTokens",
];
const MODEL_KEYS: &[&str] = &["model", "model_id", "modelID", "modelId", "active_model"];
const PROVIDER_KEYS: &[&str] = &["provider", "provider_id", "providerID", "providerId"];
const REASONING_EFFORT_KEYS: &[&str] =
    &["reasoning_effort", "reasoningEffort", "reasoning", "effort"];
const SESSION_KEYS: &[&str] = &[
    "session_id",
    "sessionId",
    "conversation_id",
    "conversationId",
    "id",
    "thread_id",
    "threadId",
];
const TURN_ID_KEYS: &[&str] = &["turn_id", "turnId"];
const PROJECT_KEYS: &[&str] = &[
    "project_path",
    "projectPath",
    "cwd",
    "working_dir",
    "workingDirectory",
];
const TIMESTAMP_KEYS: &[&str] = &[
    "timestamp",
    "created_at",
    "createdAt",
    "time",
    "lastActivity",
    "updated_at",
    "updatedAt",
];

fn extract_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(found) = map.get(*key).and_then(value_to_u64) {
                    return Some(found);
                }
            }
            for child in map.values() {
                if let Some(found) = extract_u64(child, keys) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(|item| extract_u64(item, keys)),
        _ => None,
    }
}

fn extract_direct_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    let Value::Object(map) = value else {
        return None;
    };
    keys.iter()
        .find_map(|key| map.get(*key).and_then(value_to_u64))
}

fn value_to_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .or_else(|| number.as_f64().map(|n| n.max(0.0) as u64)),
        Value::String(raw) => raw.parse::<u64>().ok(),
        _ => None,
    }
}

fn value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(raw) => raw.parse::<f64>().ok(),
        _ => None,
    }
}

fn extract_reported_cost(value: &Value) -> Option<f64> {
    match value {
        Value::Object(map) => {
            if let Some(cost) = map
                .get("usage")
                .and_then(|usage| usage.pointer("/cost/total"))
                .and_then(value_to_f64)
                .filter(|cost| cost.is_finite() && *cost >= 0.0)
            {
                return Some(cost);
            }
            for key in ["total_cost", "totalCost", "cost_usd", "costUsd", "cost"] {
                if let Some(cost) = map
                    .get(key)
                    .and_then(|value| match value {
                        Value::Object(object) => object
                            .get("total")
                            .or_else(|| object.get("value"))
                            .and_then(value_to_f64),
                        scalar => value_to_f64(scalar),
                    })
                    .filter(|cost| cost.is_finite() && *cost >= 0.0)
                {
                    return Some(cost);
                }
            }
            map.values().find_map(extract_reported_cost)
        }
        Value::Array(items) => items.iter().find_map(extract_reported_cost),
        _ => None,
    }
}

fn extract_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(found) = map.get(*key).and_then(Value::as_str) {
                    if !found.trim().is_empty() {
                        return Some(found.to_string());
                    }
                }
            }
            for child in map.values() {
                if let Some(found) = extract_string(child, keys) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(|item| extract_string(item, keys)),
        _ => None,
    }
}

fn extract_timestamp(value: &Value) -> Option<String> {
    extract_string(value, TIMESTAMP_KEYS).and_then(|raw| normalize_timestamp(&raw))
}

fn extract_reasoning_effort(value: &Value) -> Option<String> {
    extract_string(value, REASONING_EFFORT_KEYS).and_then(|raw| {
        let normalized = raw.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "minimal" | "low" | "medium" | "high" => Some(normalized),
            _ => None,
        }
    })
}

fn normalize_timestamp(raw: &str) -> Option<String> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        return Some(parsed.with_timezone(&Utc).to_rfc3339());
    }
    if let Ok(epoch) = raw.parse::<i64>() {
        let seconds = if epoch > 10_000_000_000 {
            epoch / 1000
        } else {
            epoch
        };
        return DateTime::from_timestamp(seconds, 0).map(|dt| dt.to_rfc3339());
    }
    None
}

fn file_modified_at(path: &Path) -> Option<String> {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(DateTime::<Utc>::from)
        .map(|dt| dt.to_rfc3339())
}

fn infer_project_path(source: &DetectedSource, file: &Path) -> String {
    if let Ok(relative) = file.strip_prefix(&source.path) {
        if let Some(first) = relative.components().next() {
            let candidate = first.as_os_str().to_string_lossy();
            if !candidate.is_empty() {
                return candidate.to_string();
            }
        }
    }
    file.parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown-project".to_string())
}

fn raw_hash(
    source: SourceKind,
    _file: &Path,
    raw_span: Option<&str>,
    value: &Value,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(source.as_str().as_bytes());
    hasher.update(b"\n");
    // Exclude the local absolute path: this hash seeds the Collector
    // fingerprint and must survive source relocation.
    if let Some(raw_span) = raw_span {
        hasher.update(raw_span.as_bytes());
    }
    hasher.update(b"\n");
    hasher.update(serde_json::to_vec(value)?);
    Ok(hex::encode(hasher.finalize()))
}

/// Stable identity material intentionally excludes local paths, project salts,
/// pricing, parser version, and import time. Parser upgrades can therefore
/// update an existing Hub event instead of creating a second event.
pub fn stable_event_fingerprint(event: &UsageEvent) -> String {
    let mut hasher = Sha256::new();
    hasher.update(event.source.as_str().as_bytes());
    hasher.update(b"\n");
    // raw_event_hash is canonical source-record content plus parser span; it
    // deliberately contains no local path. Do not include fallback session
    // names, model/pricing fields, or parser-version labels here.
    hasher.update(event.raw_span.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"\n");
    hasher.update(event.raw_event_hash.as_bytes());
    hex::encode(hasher.finalize())
}

trait FileStemString {
    fn file_stem_string(&self) -> Option<String>;
}

impl FileStemString for Path {
    fn file_stem_string(&self) -> Option<String> {
        self.file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .filter(|stem| !stem.trim().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::pricing::seed_bundled_pricing;

    #[test]
    fn imports_claude_codex_opencode_and_pi_fixtures_idempotently() {
        let dir = tempdir().unwrap();
        let data = dir.path().join("data");
        let claude = data.join("claude/projects/my-project");
        let codex = data.join("codex/sessions");
        let opencode = data.join("opencode/storage/message/session-a");
        let pi = data.join("pi/agent/sessions/project-pi");
        fs::create_dir_all(&claude).unwrap();
        fs::create_dir_all(&codex).unwrap();
        fs::create_dir_all(&opencode).unwrap();
        fs::create_dir_all(&pi).unwrap();

        fs::write(
            claude.join("session.jsonl"),
            r#"{"sessionId":"claude-1","cwd":"/repo/dirtydash","timestamp":"2026-06-06T12:00:00Z","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":1000,"output_tokens":200,"cache_creation_input_tokens":50,"cache_read_input_tokens":500}}}"#,
        )
        .unwrap();
        fs::write(
            codex.join("session.jsonl"),
            r#"{"type":"event_msg","payload":{"type":"turn_context","model":"gpt-5.3-codex","cwd":"/repo/codex"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":100,"output_tokens":50,"reasoning_output_tokens":25},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":100,"output_tokens":50,"reasoning_output_tokens":25}}}}
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":3000,"cached_input_tokens":400,"output_tokens":110,"reasoning_output_tokens":55},"last_token_usage":{"input_tokens":2000,"cached_input_tokens":300,"output_tokens":60,"reasoning_output_tokens":30}}}}"#,
        )
        .unwrap();
        fs::write(
            opencode.join("msg_1.json"),
            r#"{"sessionID":"open-1","projectPath":"/repo/open","providerID":"anthropic","modelID":"claude-haiku-4-5","usage":{"inputTokens":300,"outputTokens":100,"cacheReadTokens":25}}"#,
        )
        .unwrap();
        fs::write(
            pi.join("session.jsonl"),
            r#"{"session_id":"pi-1","projectPath":"project-pi","model":"claude-opus-4-6","usage":{"inputTokens":100,"outputTokens":25,"cacheWriteTokens":10}}"#,
        )
        .unwrap();

        let db = Database::open(dir.path().join("dirtydash.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();
        let sources = vec![
            detected(SourceKind::ClaudeCode, data.join("claude/projects")),
            detected(SourceKind::Codex, data.join("codex/sessions")),
            detected(SourceKind::OpenCode, data.join("opencode/storage/message")),
            detected(SourceKind::PiAgent, data.join("pi/agent/sessions")),
        ];

        let first = import_sources(
            &db,
            sources.clone(),
            ImportOptions {
                metadata_only: true,
            },
        )
        .unwrap();
        assert_eq!(first.inserted_events, 5);
        assert_eq!(first.parse_errors, 0);

        let codex_session = db
            .sessions(10)
            .unwrap()
            .into_iter()
            .find(|session| session.source == "codex")
            .expect("codex session should be imported");
        assert_eq!(codex_session.total_tokens, 3_165);

        let second = import_sources(
            &db,
            sources,
            ImportOptions {
                metadata_only: true,
            },
        )
        .unwrap();
        assert_eq!(second.inserted_events, 0);
        assert_eq!(second.skipped_existing_events, 5);
    }

    #[test]
    fn records_malformed_jsonl_without_stopping_import() {
        let dir = tempdir().unwrap();
        let source_root = dir.path().join("claude/projects/broken");
        fs::create_dir_all(&source_root).unwrap();
        fs::write(
            source_root.join("session.jsonl"),
            "not json\n{\"sessionId\":\"ok\",\"model\":\"claude-sonnet-4-6\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}\n",
        )
        .unwrap();

        let db = Database::open(dir.path().join("dirtydash.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();
        let report = import_sources(
            &db,
            vec![detected(
                SourceKind::ClaudeCode,
                dir.path().join("claude/projects"),
            )],
            ImportOptions {
                metadata_only: true,
            },
        )
        .unwrap();
        assert_eq!(report.inserted_events, 1);
        assert_eq!(report.parse_errors, 1);
    }

    #[test]
    fn codex_scan_includes_archived_sessions_next_to_sessions() {
        let dir = tempdir().unwrap();
        let codex_home = dir.path().join("codex");
        let sessions = codex_home.join("sessions/2026/06/07");
        let archived = codex_home.join("archived_sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir_all(&archived).unwrap();
        fs::write(sessions.join("live.jsonl"), "{}\n").unwrap();
        fs::write(archived.join("rollout-2026-06-07T12-00-00.jsonl"), "{}\n").unwrap();

        let mut config = Config::default();
        config.source_roots.push(crate::config::SourceRoot {
            kind: "codex".to_string(),
            path: codex_home,
        });

        let sources = scan_sources(&config).unwrap();
        let codex_paths = sources
            .into_iter()
            .filter(|source| source.kind == SourceKind::Codex)
            .map(|source| source.path)
            .collect::<Vec<_>>();

        assert!(codex_paths.iter().any(|path| path.ends_with("sessions")));
        assert!(codex_paths
            .iter()
            .any(|path| path.ends_with("archived_sessions")));
    }

    #[test]
    fn codex_reasoning_effort_does_not_create_fast_model_slugs_without_trace_evidence() {
        let dir = tempdir().unwrap();
        let source_root = dir.path().join("codex/sessions");
        fs::create_dir_all(&source_root).unwrap();
        fs::write(
            source_root.join("session-55.jsonl"),
            r#"{"timestamp":"2026-06-07T12:00:00Z","type":"turn_context","payload":{"turn_id":"turn-fast-55","cwd":"/repo/fast","model":"gpt-5.5","collaboration_mode":{"settings":{"model":"gpt-5.5","reasoning_effort":"low"}},"effort":"low"}}
{"timestamp":"2026-06-07T12:00:10Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":100,"output_tokens":50},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":100,"output_tokens":50}}}}"#,
        )
        .unwrap();
        fs::write(
            source_root.join("session-54.jsonl"),
            r#"{"timestamp":"2026-06-07T12:01:00Z","type":"turn_context","payload":{"turn_id":"turn-fast-54","cwd":"/repo/fast","model":"gpt-5.4","collaboration_mode":{"settings":{"model":"gpt-5.4","reasoning_effort":"minimal"}},"effort":"minimal"}}
{"timestamp":"2026-06-07T12:01:10Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":100,"output_tokens":50},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":100,"output_tokens":50}}}}"#,
        )
        .unwrap();

        let db = Database::open(dir.path().join("dirtydash.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();
        let source = detected(SourceKind::Codex, source_root);

        let report = import_sources(
            &db,
            vec![source],
            ImportOptions {
                metadata_only: true,
            },
        )
        .unwrap();

        assert_eq!(report.inserted_events, 2);
        let sessions = db.sessions(10).unwrap();
        let gpt55 = sessions
            .iter()
            .find(|session| session.model == "gpt-5.5")
            .expect("gpt-5.5 low effort should stay on the base model");
        let gpt54 = sessions
            .iter()
            .find(|session| session.model == "gpt-5.4")
            .expect("gpt-5.4 minimal effort should stay on the base model");
        assert_eq!(
            gpt55.pricing_version,
            crate::pricing::BUNDLED_PRICING_VERSION
        );
        assert_eq!(
            gpt54.pricing_version,
            crate::pricing::BUNDLED_PRICING_VERSION
        );
        assert!((gpt55.estimated_cost_usd - 0.00605).abs() < 0.000001);
        assert!((gpt54.estimated_cost_usd - 0.003025).abs() < 0.000001);

        let conn = db.connection().unwrap();
        let modes = conn
            .prepare(
                "SELECT model, pricing_mode, reasoning_effort FROM usage_events ORDER BY model",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            modes,
            vec![
                (
                    "gpt-5.4".to_string(),
                    "standard".to_string(),
                    "minimal".to_string()
                ),
                (
                    "gpt-5.5".to_string(),
                    "standard".to_string(),
                    "low".to_string()
                )
            ]
        );
    }

    #[test]
    fn codex_logs_2_priority_rows_mark_matching_turn_events() {
        let dir = tempdir().unwrap();
        let codex_home = dir.path().join("codex");
        let source_root = codex_home.join("sessions");
        fs::create_dir_all(&source_root).unwrap();
        fs::write(
            source_root.join("session-priority.jsonl"),
            r#"{"timestamp":"2026-06-07T12:00:00Z","type":"turn_context","payload":{"turn_id":"turn-priority","cwd":"/repo/fast","model":"gpt-5.5"}}
{"timestamp":"2026-06-07T12:00:10Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":100,"output_tokens":50},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":100,"output_tokens":50}}}}"#,
        )
        .unwrap();

        let trace = rusqlite::Connection::open(codex_home.join("logs_2.sqlite")).unwrap();
        trace
            .execute("CREATE TABLE logs (feedback_log_body TEXT)", [])
            .unwrap();
        let metadata = serde_json::json!({"turn_id": "turn-priority"}).to_string();
        let request = serde_json::json!({
            "type": "response.create",
            "model": "codex-auto-review",
            "service_tier": "priority",
            "client_metadata": {
                "x-codex-turn-metadata": metadata
            }
        })
        .to_string();
        trace
            .execute(
                "INSERT INTO logs (feedback_log_body) VALUES (?1)",
                rusqlite::params![format!("span:websocket request: {request}")],
            )
            .unwrap();
        let completed = serde_json::json!({
            "type": "response.completed",
            "response": {
                "model": "gpt-5.4"
            }
        })
        .to_string();
        trace
            .execute(
                "INSERT INTO logs (feedback_log_body) VALUES (?1)",
                rusqlite::params![format!(
                    "span turn.id=turn-priority websocket event: {completed}"
                )],
            )
            .unwrap();
        drop(trace);

        let db = Database::open(dir.path().join("dirtydash.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();
        let report = import_sources(
            &db,
            vec![detected(SourceKind::Codex, source_root)],
            ImportOptions {
                metadata_only: true,
            },
        )
        .unwrap();

        assert_eq!(report.inserted_events, 1);
        let row = db
            .connection()
            .unwrap()
            .query_row(
                "SELECT model, turn_id, pricing_mode, estimated_cost_usd FROM usage_events",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, f64>(3)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, "gpt-5.5");
        assert_eq!(row.1.as_deref(), Some("turn-priority"));
        assert_eq!(row.2, "priority");
        assert!((row.3 - 0.00605).abs() < 0.000001);
    }

    #[test]
    fn seed_reclassifies_existing_codex_rows_from_trace_evidence() {
        let dir = tempdir().unwrap();
        let codex_home = dir.path().join("codex");
        let source_root = codex_home.join("sessions");
        fs::create_dir_all(&source_root).unwrap();
        let raw_path = source_root.join("session-priority.jsonl");
        fs::write(&raw_path, "{}").unwrap();

        let trace = rusqlite::Connection::open(codex_home.join("logs_2.sqlite")).unwrap();
        trace
            .execute("CREATE TABLE logs (feedback_log_body TEXT)", [])
            .unwrap();
        let metadata = serde_json::json!({"turn_id": "turn-priority"}).to_string();
        let request = serde_json::json!({
            "type": "response.create",
            "model": "codex-auto-review",
            "service_tier": "priority",
            "client_metadata": {
                "x-codex-turn-metadata": metadata
            }
        })
        .to_string();
        trace
            .execute(
                "INSERT INTO logs (feedback_log_body) VALUES (?1)",
                rusqlite::params![format!("span:websocket request: {request}")],
            )
            .unwrap();
        drop(trace);

        let db = Database::open(dir.path().join("dirtydash.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();
        db.upsert_usage_event(&UsageEvent {
            machine: "test-machine".to_string(),
            source: SourceKind::Codex,
            project_path: "/repo/fast".to_string(),
            session_id: "session-priority".to_string(),
            turn_id: Some("turn-priority".to_string()),
            provider: "openai-codex".to_string(),
            model: "gpt-5.5".to_string(),
            reasoning_effort: None,
            prompt_tokens: 1_000,
            completion_tokens: 50,
            cache_read_tokens: 100,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: 1_150,
            estimated_cost_usd: 0.001,
            confidence: 0.92,
            event_timestamp: None,
            raw_path: raw_path.display().to_string(),
            raw_span: None,
            parser_name: SourceKind::Codex.parser_name().to_string(),
            parser_version: PARSER_VERSION.to_string(),
            raw_event_hash: "priority-repair-hash".to_string(),
            imported_at: Utc::now().to_rfc3339(),
            pricing_version: crate::pricing::BUNDLED_PRICING_VERSION.to_string(),
            pricing_mode: PricingMode::Standard,
            metadata_only: true,
        })
        .unwrap();

        seed_bundled_pricing(&db).unwrap();

        let row = db
            .connection()
            .unwrap()
            .query_row(
                "SELECT pricing_mode, estimated_cost_usd FROM usage_events",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )
            .unwrap();
        assert_eq!(row.0, "priority");
        assert!((row.1 - 0.016375).abs() < 0.000001);
    }

    #[test]
    fn pi_agent_reported_cost_updates_stale_imported_rows() {
        let dir = tempdir().unwrap();
        let data = dir.path().join("data");
        let pi = data.join("pi/agent/sessions/project-pi");
        fs::create_dir_all(&pi).unwrap();
        let file = pi.join("session.jsonl");
        fs::write(
            &file,
            r#"{"type":"session","id":"pi-1","timestamp":"2026-06-06T12:00:00Z","cwd":"/repo/pi"}
{"type":"model_change","provider":"openai-codex","modelId":"gpt-5.4"}
{"type":"message","id":"msg-1","timestamp":"2026-06-06T12:01:00Z","message":{"role":"assistant","provider":"openai-codex","model":"gpt-5.4","usage":{"input":1782,"output":169,"cacheRead":1536,"cacheWrite":0,"totalTokens":3487,"cost":{"input":0.004455,"output":0.002535,"cacheRead":0.000384,"cacheWrite":0,"total":0.00699}}}}"#,
        )
        .unwrap();

        let db = Database::open(dir.path().join("dirtydash.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();
        let source = detected(SourceKind::PiAgent, data.join("pi/agent/sessions"));
        let parsed = parse_file(
            &db,
            &source,
            &file,
            &crate::db::local_machine(),
            "2026-06-06T12:02:00Z",
            ImportOptions {
                metadata_only: true,
            },
            None,
        )
        .unwrap();
        let mut stale = parsed.events[0].clone();
        stale.prompt_tokens = 0;
        stale.cache_read_tokens = stale.total_tokens;
        stale.completion_tokens = 0;
        stale.estimated_cost_usd = 0.0;
        stale.confidence = 0.62;
        stale.pricing_version = "unpriced".to_string();
        let hash = stale.raw_event_hash.clone();
        assert!(matches!(
            db.upsert_usage_event(&stale).unwrap(),
            crate::db::UsageEventWrite::Inserted
        ));

        let report = import_sources(
            &db,
            vec![source],
            ImportOptions {
                metadata_only: true,
            },
        )
        .unwrap();
        assert_eq!(report.inserted_events, 0);
        assert_eq!(report.updated_existing_events, 1);
        assert_eq!(report.skipped_existing_events, 0);

        let conn = db.connection().unwrap();
        let repaired = conn
            .query_row(
                r#"
                SELECT provider, model, prompt_tokens, completion_tokens, cache_read_tokens,
                    total_tokens, estimated_cost_usd, pricing_version
                FROM usage_events
                WHERE raw_event_hash = ?1
                "#,
                rusqlite::params![hash],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? as u64,
                        row.get::<_, i64>(3)? as u64,
                        row.get::<_, i64>(4)? as u64,
                        row.get::<_, i64>(5)? as u64,
                        row.get::<_, f64>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(repaired.0, "openai-codex");
        assert_eq!(repaired.1, "gpt-5.4");
        assert_eq!(repaired.2, 1782);
        assert_eq!(repaired.3, 169);
        assert_eq!(repaired.4, 1536);
        assert_eq!(repaired.5, 3487);
        assert!((repaired.6 - 0.00699).abs() < 0.00001);
        assert_eq!(repaired.7, "reported-cost");
    }

    fn detected(kind: SourceKind, path: PathBuf) -> DetectedSource {
        DetectedSource {
            kind,
            file_count: matching_files(kind, &path).unwrap().len() as u64,
            path,
            confidence: "high".to_string(),
            harness_names: kind.harness_names(),
        }
    }
}
