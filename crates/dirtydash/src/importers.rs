use std::collections::HashSet;
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
use crate::pricing;

pub const PARSER_VERSION: &str = "dirtydash-v1.1.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    ClaudeCode,
    Codex,
    OpenCode,
    PiAgent,
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
    pub skipped_existing_events: u64,
    pub parse_errors: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageNumbers {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CodexRawUsage {
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub machine: String,
    pub source: SourceKind,
    pub project_path: String,
    pub session_id: String,
    pub provider: String,
    pub model: String,
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
    pub metadata_only: bool,
}

#[derive(Debug, Clone)]
struct ParsedFile {
    events: Vec<UsageEvent>,
    parse_error: Option<String>,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::ClaudeCode => "claude-code",
            SourceKind::Codex => "codex",
            SourceKind::OpenCode => "opencode",
            SourceKind::PiAgent => "pi-agent",
        }
    }

    pub fn parser_name(self) -> &'static str {
        match self {
            SourceKind::ClaudeCode => "claude-code-jsonl",
            SourceKind::Codex => "codex-token-count-jsonl",
            SourceKind::OpenCode => "opencode-storage-json",
            SourceKind::PiAgent => "pi-agent-jsonl",
        }
    }

    fn default_provider(self) -> &'static str {
        match self {
            SourceKind::ClaudeCode | SourceKind::PiAgent => "anthropic",
            SourceKind::Codex => "openai",
            SourceKind::OpenCode => "unknown",
        }
    }

    fn harness_names(self) -> Vec<String> {
        match self {
            SourceKind::ClaudeCode => vec!["Claude Code".to_string(), "claude-code".to_string()],
            SourceKind::Codex => vec!["Codex CLI".to_string(), "codex".to_string()],
            SourceKind::OpenCode => vec!["OpenCode".to_string(), "opencode".to_string()],
            SourceKind::PiAgent => vec!["pi-agent".to_string()],
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
}

impl CodexRawUsage {
    fn from_value(value: &Value) -> Self {
        let input_tokens = extract_u64_direct(value, CODEX_INPUT_KEYS).unwrap_or(0);
        let output_tokens = extract_u64_direct(value, CODEX_OUTPUT_KEYS).unwrap_or(0);
        let cached_input_tokens = extract_u64_direct(value, CODEX_CACHE_READ_KEYS).unwrap_or(0);
        let reasoning_output_tokens = extract_u64_direct(value, CODEX_REASONING_KEYS).unwrap_or(0);

        Self {
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_output_tokens,
        }
    }

    fn has_usage(self) -> bool {
        self.input_tokens > 0
            || self.output_tokens > 0
            || self.cached_input_tokens > 0
            || self.reasoning_output_tokens > 0
    }

    fn delta_from(self, previous: Self) -> Option<Self> {
        if self.input_tokens < previous.input_tokens
            || self.output_tokens < previous.output_tokens
            || self.cached_input_tokens < previous.cached_input_tokens
            || self.reasoning_output_tokens < previous.reasoning_output_tokens
        {
            return None;
        }

        Some(Self {
            input_tokens: self.input_tokens - previous.input_tokens,
            output_tokens: self.output_tokens - previous.output_tokens,
            cached_input_tokens: self.cached_input_tokens - previous.cached_input_tokens,
            reasoning_output_tokens: self.reasoning_output_tokens
                - previous.reasoning_output_tokens,
        })
    }

    fn saturating_add(self, other: Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
            cached_input_tokens: self
                .cached_input_tokens
                .saturating_add(other.cached_input_tokens),
            reasoning_output_tokens: self
                .reasoning_output_tokens
                .saturating_add(other.reasoning_output_tokens),
        }
    }

    fn total(self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cached_input_tokens)
            .saturating_add(self.reasoning_output_tokens)
    }

    fn looks_like_stale_regression(self, previous: Self, last: Self) -> bool {
        let previous_total = previous.total();
        let current_total = self.total();
        let last_total = last.total();

        if previous_total == 0 || current_total == 0 || last_total == 0 {
            return false;
        }

        current_total.saturating_mul(100) >= previous_total.saturating_mul(98)
            || current_total.saturating_add(last_total.saturating_mul(2)) >= previous_total
    }

    fn into_usage_numbers(self) -> UsageNumbers {
        let cache_read_tokens = self.cached_input_tokens.min(self.input_tokens);
        UsageNumbers {
            prompt_tokens: self.input_tokens.saturating_sub(cache_read_tokens),
            completion_tokens: self.output_tokens,
            cache_read_tokens,
            cache_write_tokens: 0,
            reasoning_tokens: self.reasoning_output_tokens,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct CodexTokenSnapshots {
    total: Option<CodexRawUsage>,
    last: Option<CodexRawUsage>,
}

pub fn scan_sources(config: &Config) -> Result<Vec<DetectedSource>> {
    let mut candidates = default_candidates()?;
    for root in &config.source_roots {
        let kind: SourceKind = root.kind.parse()?;
        candidates.push((kind, normalize_source_path(kind, root.path.clone())));
    }

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

        for file in matching_files(source.kind, &source.path)? {
            report.files_seen += 1;
            let parsed = parse_file(db, &source, &file, &machine, &imported_at, options)
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

            if parsed.parse_error.is_none() || !parsed.events.is_empty() {
                let keep_hashes = parsed
                    .events
                    .iter()
                    .map(|event| event.raw_event_hash.clone())
                    .collect::<Vec<_>>();
                db.delete_usage_events_for_file_except(source.kind, &machine, &file, &keep_hashes)?;
            }

            for event in parsed.events {
                if db.insert_usage_event(&event)? {
                    report.inserted_events += 1;
                } else {
                    report.skipped_existing_events += 1;
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
        let root = expand_candidate_root(root);
        candidates.push((
            SourceKind::Codex,
            normalize_source_path(SourceKind::Codex, root.clone()),
        ));
        candidates.push((SourceKind::Codex, root.join("archived_sessions")));
    }

    let opencode_roots =
        env_paths("OPENCODE_DATA_DIR").unwrap_or_else(|| vec![home.join(".local/share/opencode")]);
    for root in opencode_roots {
        let root = expand_candidate_root(root);
        candidates.push((SourceKind::OpenCode, root.join("opencode.db")));
        candidates.push((
            SourceKind::OpenCode,
            normalize_source_path(SourceKind::OpenCode, root.join("storage/message")),
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

    Ok(candidates)
}

fn expand_candidate_root(path: PathBuf) -> PathBuf {
    if let Some(raw) = path.to_str() {
        return expand_home(raw);
    }
    path
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
            if path.is_file() {
                path
            } else if path.join("opencode.db").exists() {
                path.join("opencode.db")
            } else if path.join("storage/message").exists() {
                path.join("storage/message")
            } else {
                path
            }
        }
        SourceKind::PiAgent => path,
    }
}

fn matching_files(kind: SourceKind, path: &Path) -> Result<Vec<PathBuf>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    if path.is_file() {
        return Ok(match kind {
            SourceKind::OpenCode if is_opencode_sqlite(path) => vec![path.to_path_buf()],
            _ => Vec::new(),
        });
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
        let matches = match kind {
            SourceKind::ClaudeCode | SourceKind::Codex | SourceKind::PiAgent => {
                file.extension().is_some_and(|ext| ext == "jsonl")
            }
            SourceKind::OpenCode => file.extension().is_some_and(|ext| ext == "json"),
        };
        if matches {
            files.push(file.to_path_buf());
        }
    }
    files.sort();
    Ok(files)
}

fn is_opencode_sqlite(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "opencode.db")
}

fn parse_file(
    db: &Database,
    source: &DetectedSource,
    file: &Path,
    machine: &str,
    imported_at: &str,
    options: ImportOptions,
) -> Result<ParsedFile> {
    match source.kind {
        SourceKind::Codex => parse_codex_jsonl(db, source, file, machine, imported_at, options),
        SourceKind::ClaudeCode | SourceKind::PiAgent => {
            parse_generic_jsonl(db, source, file, machine, imported_at, options)
        }
        SourceKind::OpenCode => {
            parse_opencode_file(db, source, file, machine, imported_at, options)
        }
    }
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

fn parse_opencode_file(
    db: &Database,
    source: &DetectedSource,
    file: &Path,
    machine: &str,
    imported_at: &str,
    options: ImportOptions,
) -> Result<ParsedFile> {
    if is_opencode_sqlite(file) {
        return parse_opencode_sqlite(db, source, file, machine, imported_at, options);
    }

    let raw = fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let value = serde_json::from_str::<Value>(&raw)
        .with_context(|| format!("parsing JSON {}", file.display()))?;
    let event = event_from_opencode_value(
        db,
        source,
        file,
        Some("$.root".to_string()),
        &value,
        machine,
        imported_at,
        options,
    )?;

    Ok(ParsedFile {
        events: event.into_iter().collect(),
        parse_error: None,
    })
}

fn parse_opencode_sqlite(
    db: &Database,
    source: &DetectedSource,
    file: &Path,
    machine: &str,
    imported_at: &str,
    options: ImportOptions,
) -> Result<ParsedFile> {
    let conn = Connection::open_with_flags(
        file,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening OpenCode database {}", file.display()))?;

    let mut stmt = conn.prepare(
        r#"
        SELECT m.id, m.session_id, m.data
        FROM message m
        WHERE json_extract(m.data, '$.role') = 'assistant'
          AND json_extract(m.data, '$.tokens') IS NOT NULL
        "#,
    )?;
    let mut rows = stmt.query([])?;
    let mut events = Vec::new();
    let mut parse_errors = Vec::new();

    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let session_id: String = row.get(1)?;
        let data: String = row.get(2)?;
        let mut value = match serde_json::from_str::<Value>(&data) {
            Ok(value) => value,
            Err(error) => {
                parse_errors.push(format!("message {id}: {error}"));
                continue;
            }
        };

        if let Value::Object(map) = &mut value {
            map.entry("id".to_string())
                .or_insert_with(|| Value::String(id.clone()));
            map.entry("sessionID".to_string())
                .or_insert_with(|| Value::String(session_id));
        }

        if let Some(event) = event_from_opencode_value(
            db,
            source,
            file,
            Some(format!("message {id}")),
            &value,
            machine,
            imported_at,
            options,
        )? {
            events.push(event);
        }
    }

    Ok(ParsedFile {
        events,
        parse_error: parse_errors.first().cloned(),
    })
}

fn parse_codex_jsonl(
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
    let mut previous_total: Option<CodexRawUsage> = None;
    let mut current_model: Option<String> = None;
    let mut current_provider: Option<String> = None;

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

        let payload_type = value
            .pointer("/payload/type")
            .and_then(Value::as_str)
            .or_else(|| value.get("type").and_then(Value::as_str));

        if payload_type == Some("turn_context") {
            continue;
        }

        if payload_type == Some("token_count") {
            let usage_value = value.pointer("/payload").unwrap_or(&value);
            let snapshots = extract_codex_token_snapshots(usage_value);
            let (usage, next_total) =
                match codex_incremental_usage(snapshots.total, snapshots.last, previous_total) {
                    Some(result) => result,
                    None => continue,
                };
            if !usage.has_usage() {
                continue;
            }
            previous_total = next_total;

            if let Some(mut event) = event_from_usage(
                db,
                source,
                file,
                Some(format!("line {}", index + 1)),
                &value,
                usage,
                machine,
                imported_at,
                options,
                Some(current_model.as_deref().unwrap_or("gpt-5.5")),
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
    )
}

#[allow(clippy::too_many_arguments)]
fn event_from_opencode_value(
    db: &Database,
    source: &DetectedSource,
    file: &Path,
    raw_span: Option<String>,
    value: &Value,
    machine: &str,
    imported_at: &str,
    options: ImportOptions,
) -> Result<Option<UsageEvent>> {
    let usage =
        extract_opencode_usage_numbers(value).unwrap_or_else(|| extract_usage_numbers(value));
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
        None,
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
) -> Result<Option<UsageEvent>> {
    if !usage.has_usage() {
        return Ok(None);
    }

    let provider = extract_string(value, PROVIDER_KEYS)
        .unwrap_or_else(|| source.kind.default_provider().to_string());
    let model = extract_string(value, MODEL_KEYS)
        .or_else(|| fallback_model.map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_string());
    let session_id = extract_string(value, SESSION_KEYS).unwrap_or_else(|| {
        file.file_stem_string()
            .unwrap_or_else(|| "unknown-session".to_string())
    });
    let project_path =
        extract_string(value, PROJECT_KEYS).unwrap_or_else(|| infer_project_path(source, file));
    let event_timestamp = extract_timestamp(value).or_else(|| file_modified_at(file));
    let cost = if let Some(reported_cost) = extract_reported_cost(value) {
        pricing::CostEstimate {
            estimated_cost_usd: reported_cost,
            pricing_version: "reported-cost".to_string(),
            priced: true,
        }
    } else {
        pricing::estimate_cost(db, &provider, &model, &usage)?
    };
    let confidence = if cost.pricing_version == "reported-cost" {
        0.98
    } else if cost.priced {
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
        provider,
        model,
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        total_tokens: usage.total_tokens(),
        estimated_cost_usd: cost.estimated_cost_usd,
        confidence,
        event_timestamp,
        raw_path: file.display().to_string(),
        raw_span,
        parser_name: source.kind.parser_name().to_string(),
        parser_version: PARSER_VERSION.to_string(),
        raw_event_hash: hash,
        imported_at: imported_at.to_string(),
        pricing_version: cost.pricing_version,
        metadata_only: options.metadata_only,
    }))
}

fn extract_usage_numbers(value: &Value) -> UsageNumbers {
    UsageNumbers {
        prompt_tokens: extract_u64(value, PROMPT_KEYS).unwrap_or(0),
        completion_tokens: extract_u64(value, COMPLETION_KEYS).unwrap_or(0),
        cache_read_tokens: extract_u64(value, CACHE_READ_KEYS).unwrap_or(0),
        cache_write_tokens: extract_u64(value, CACHE_WRITE_KEYS).unwrap_or(0),
        reasoning_tokens: extract_u64(value, REASONING_KEYS).unwrap_or(0),
    }
}

fn extract_opencode_usage_numbers(value: &Value) -> Option<UsageNumbers> {
    let tokens = value.get("tokens")?;
    let cache = tokens.get("cache");
    Some(UsageNumbers {
        prompt_tokens: extract_u64_direct(tokens, &["input"]).unwrap_or(0),
        completion_tokens: extract_u64_direct(tokens, &["output"]).unwrap_or(0),
        cache_read_tokens: cache
            .and_then(|cache| extract_u64_direct(cache, &["read"]))
            .unwrap_or(0),
        cache_write_tokens: cache
            .and_then(|cache| extract_u64_direct(cache, &["write"]))
            .unwrap_or(0),
        reasoning_tokens: extract_u64_direct(tokens, &["reasoning"]).unwrap_or(0),
    })
}

fn extract_codex_token_snapshots(value: &Value) -> CodexTokenSnapshots {
    let info = value.get("info").unwrap_or(value);
    let total = info
        .get("total_token_usage")
        .map(CodexRawUsage::from_value)
        .filter(|usage| usage.has_usage());
    let last = info
        .get("last_token_usage")
        .map(CodexRawUsage::from_value)
        .filter(|usage| usage.has_usage());

    if total.is_none() && last.is_none() {
        let direct = CodexRawUsage::from_value(value);
        return CodexTokenSnapshots {
            total: direct.has_usage().then_some(direct),
            last: None,
        };
    }

    CodexTokenSnapshots { total, last }
}

fn codex_incremental_usage(
    total: Option<CodexRawUsage>,
    last: Option<CodexRawUsage>,
    previous: Option<CodexRawUsage>,
) -> Option<(UsageNumbers, Option<CodexRawUsage>)> {
    match (total, last, previous) {
        (Some(total), Some(last), Some(previous)) => {
            if total == previous {
                return None;
            }
            if total.delta_from(previous).is_none()
                && total.looks_like_stale_regression(previous, last)
            {
                return None;
            }
            Some((last.into_usage_numbers(), Some(total)))
        }
        (Some(total), Some(last), None) => Some((last.into_usage_numbers(), Some(total))),
        (Some(total), None, Some(previous)) => {
            if total == previous {
                return None;
            }
            if let Some(delta) = total.delta_from(previous) {
                Some((delta.into_usage_numbers(), Some(total)))
            } else {
                Some((UsageNumbers::default(), Some(total)))
            }
        }
        (Some(total), None, None) => Some((total.into_usage_numbers(), Some(total))),
        (None, Some(last), Some(previous)) => Some((
            last.into_usage_numbers(),
            Some(previous.saturating_add(last)),
        )),
        (None, Some(last), None) => Some((last.into_usage_numbers(), None)),
        (None, None, _) => None,
    }
}

const PROMPT_KEYS: &[&str] = &[
    "input_tokens",
    "input",
    "inputTokens",
    "prompt_tokens",
    "promptTokens",
    "total_input_tokens",
    "totalInputTokens",
];
const COMPLETION_KEYS: &[&str] = &[
    "output_tokens",
    "output",
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
    "reasoning",
    "reasoningTokens",
    "reasoning_output_tokens",
    "reasoningOutputTokens",
];
const CODEX_INPUT_KEYS: &[&str] = &["input_tokens", "prompt_tokens", "input"];
const CODEX_OUTPUT_KEYS: &[&str] = &["output_tokens", "completion_tokens", "output"];
const CODEX_CACHE_READ_KEYS: &[&str] = &["cached_input_tokens", "cache_read_input_tokens"];
const CODEX_REASONING_KEYS: &[&str] = &["reasoning_output_tokens", "reasoning_tokens"];
const MODEL_KEYS: &[&str] = &["model", "model_id", "modelID", "modelId", "active_model"];
const PROVIDER_KEYS: &[&str] = &["provider", "provider_id", "providerID", "providerId"];
const SESSION_KEYS: &[&str] = &[
    "session_id",
    "sessionId",
    "conversation_id",
    "conversationId",
    "thread_id",
    "threadId",
];
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
    "created",
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

fn extract_u64_direct(value: &Value, keys: &[&str]) -> Option<u64> {
    let map = value.as_object()?;
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
    .filter(|number| number.is_finite() && *number >= 0.0)
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
    extract_timestamp_value(value, TIMESTAMP_KEYS).and_then(normalize_timestamp_value)
}

fn extract_timestamp_value(value: &Value, keys: &[&str]) -> Option<Value> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(found) = map.get(*key) {
                    if found.is_string() || found.is_number() {
                        return Some(found.clone());
                    }
                }
            }
            for child in map.values() {
                if let Some(found) = extract_timestamp_value(child, keys) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .find_map(|item| extract_timestamp_value(item, keys)),
        _ => None,
    }
}

fn normalize_timestamp_value(value: Value) -> Option<String> {
    match value {
        Value::String(raw) => normalize_timestamp(&raw),
        Value::Number(number) => {
            if let Some(epoch) = number.as_i64() {
                return normalize_epoch(epoch);
            }
            number.as_f64().and_then(normalize_epoch_f64)
        }
        _ => None,
    }
}

fn normalize_timestamp(raw: &str) -> Option<String> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        return Some(parsed.with_timezone(&Utc).to_rfc3339());
    }
    if let Ok(epoch) = raw.parse::<i64>() {
        return normalize_epoch(epoch);
    }
    if let Ok(epoch) = raw.parse::<f64>() {
        return normalize_epoch_f64(epoch);
    }
    None
}

fn normalize_epoch(epoch: i64) -> Option<String> {
    let seconds = if epoch > 10_000_000_000 {
        epoch / 1000
    } else {
        epoch
    };
    DateTime::from_timestamp(seconds, 0).map(|dt| dt.to_rfc3339())
}

fn normalize_epoch_f64(epoch: f64) -> Option<String> {
    if !epoch.is_finite() || epoch < 0.0 {
        return None;
    }
    normalize_epoch(epoch as i64)
}

fn extract_reported_cost(value: &Value) -> Option<f64> {
    match value {
        Value::Object(map) => {
            if let Some(cost) = map.get("cost") {
                match cost {
                    Value::Object(cost_map) => {
                        if let Some(total) = cost_map.get("total").and_then(value_to_f64) {
                            if total > 0.0 {
                                return Some(total);
                            }
                        }
                    }
                    _ => {
                        if let Some(total) = value_to_f64(cost) {
                            if total > 0.0 {
                                return Some(total);
                            }
                        }
                    }
                }
            }

            map.values().find_map(extract_reported_cost)
        }
        Value::Array(items) => items.iter().find_map(extract_reported_cost),
        _ => None,
    }
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
    file: &Path,
    raw_span: Option<&str>,
    value: &Value,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(source.as_str().as_bytes());
    hasher.update(b"\n");
    hasher.update(file.display().to_string().as_bytes());
    hasher.update(b"\n");
    if let Some(raw_span) = raw_span {
        hasher.update(raw_span.as_bytes());
    }
    hasher.update(b"\n");
    hasher.update(serde_json::to_vec(value)?);
    Ok(hex::encode(hasher.finalize()))
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
{"type":"event_msg","payload":{"type":"token_count","input_tokens":1000,"cached_input_tokens":100,"output_tokens":50,"reasoning_output_tokens":25}}"#,
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
        assert_eq!(first.inserted_events, 4);
        assert_eq!(first.parse_errors, 0);

        let second = import_sources(
            &db,
            sources,
            ImportOptions {
                metadata_only: true,
            },
        )
        .unwrap();
        assert_eq!(second.inserted_events, 0);
        assert_eq!(second.skipped_existing_events, 4);
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
    fn codex_token_count_uses_last_usage_and_splits_cached_input() {
        let dir = tempdir().unwrap();
        let codex = dir.path().join("codex/sessions");
        fs::create_dir_all(&codex).unwrap();
        fs::write(
            codex.join("session.jsonl"),
            r#"{"type":"turn_context","payload":{"model":"gpt-5.5","cwd":"/repo/codex"}}
{"timestamp":"2026-06-06T12:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5},"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5}}}}
{"timestamp":"2026-06-06T12:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":110,"cached_input_tokens":22,"output_tokens":33,"reasoning_output_tokens":6},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#,
        )
        .unwrap();

        let db = Database::open(dir.path().join("dirtydash.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();
        let report = import_sources(
            &db,
            vec![detected(SourceKind::Codex, codex)],
            ImportOptions {
                metadata_only: true,
            },
        )
        .unwrap();
        assert_eq!(report.inserted_events, 2);

        let conn = db.connection().unwrap();
        let totals = conn
            .query_row(
                r#"
                SELECT SUM(prompt_tokens), SUM(completion_tokens), SUM(cache_read_tokens),
                    SUM(reasoning_tokens), SUM(total_tokens)
                FROM usage_events
                "#,
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(totals, (88, 33, 22, 6, 149));
    }

    #[test]
    fn imports_modern_opencode_sqlite_messages() {
        let dir = tempdir().unwrap();
        let opencode_db = dir.path().join("opencode.db");
        let conn = rusqlite::Connection::open(&opencode_db).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE message (
                id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                data TEXT NOT NULL
            );
            "#,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message(id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "msg_1",
                "ses_1",
                r#"{"role":"assistant","time":{"created":1780739054000},"modelID":"gpt-5.4","providerID":"openai","cost":0,"tokens":{"input":1000,"output":200,"reasoning":50,"cache":{"read":3000,"write":0}}}"#
            ],
        )
        .unwrap();
        drop(conn);

        let db = Database::open(dir.path().join("dirtydash.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();
        let report = import_sources(
            &db,
            vec![detected(SourceKind::OpenCode, opencode_db)],
            ImportOptions {
                metadata_only: true,
            },
        )
        .unwrap();
        assert_eq!(report.inserted_events, 1);

        let conn = db.connection().unwrap();
        let row = conn
            .query_row(
                r#"
                SELECT source, model, prompt_tokens, completion_tokens, cache_read_tokens,
                    reasoning_tokens, event_timestamp
                FROM usage_events
                "#,
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, "opencode");
        assert_eq!(row.1, "gpt-5.4");
        assert_eq!((row.2, row.3, row.4, row.5), (1000, 200, 3000, 50));
        assert_eq!(row.6, "2026-06-06T09:44:14+00:00");
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
