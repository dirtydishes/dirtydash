use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::importers::UsageNumbers;

pub const BUNDLED_PRICING_VERSION: &str = "2026-06-08-codexbar-priority";
pub const CODEX_LONG_CONTEXT_THRESHOLD_TOKENS: u64 = 272_000;
pub const CODEX_PRIORITY_INPUT_TOKEN_LIMIT: u64 = 272_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingRecord {
    pub provider: String,
    pub model: String,
    pub input_rate: f64,
    pub output_rate: f64,
    pub cache_read_rate: f64,
    pub cache_write_rate: f64,
    pub source_label: String,
    pub snapshot_version: String,
    pub override_flag: bool,
    pub local_free_flag: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PricingMode {
    Reported,
    Manual,
    Free,
    Standard,
    LongContext,
    Priority,
    Unpriced,
}

impl PricingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PricingMode::Reported => "reported",
            PricingMode::Manual => "manual",
            PricingMode::Free => "free",
            PricingMode::Standard => "standard",
            PricingMode::LongContext => "long-context",
            PricingMode::Priority => "priority",
            PricingMode::Unpriced => "unpriced",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "reported" => PricingMode::Reported,
            "manual" => PricingMode::Manual,
            "free" => PricingMode::Free,
            "standard" => PricingMode::Standard,
            "long-context" | "long_context" | "long" => PricingMode::LongContext,
            "priority" | "fast" => PricingMode::Priority,
            _ => PricingMode::Unpriced,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEstimate {
    pub estimated_cost_usd: f64,
    pub pricing_version: String,
    pub pricing_mode: PricingMode,
    pub priced: bool,
}

pub fn seed_bundled_pricing(db: &Database) -> Result<()> {
    for record in bundled_records() {
        db.upsert_pricing_record(&record, false)?;
    }
    reclassify_legacy_codex_usage(db)?;
    crate::importers::reclassify_codex_priority_events_from_trace_db(db)?;
    db.delete_non_overridden_pricing_records(&[
        ("openai", "gpt-5.5-fast"),
        ("openai", "gpt-5.5-long"),
        ("openai", "gpt-5.4-fast"),
    ])?;
    Ok(())
}

pub fn list_pricing(db: &Database, provider: Option<&str>) -> Result<Vec<PricingRecord>> {
    db.list_pricing(provider)
}

pub fn estimate_cost(
    db: &Database,
    provider: &str,
    model: &str,
    usage: &UsageNumbers,
    requested_mode: Option<PricingMode>,
) -> Result<CostEstimate> {
    if let Some(record) = db.pricing_record(provider, model)? {
        if record.local_free_flag {
            return Ok(CostEstimate {
                estimated_cost_usd: 0.0,
                pricing_version: record.snapshot_version,
                pricing_mode: PricingMode::Free,
                priced: true,
            });
        }

        let pricing_mode = if record.override_flag {
            PricingMode::Manual
        } else {
            resolve_pricing_mode(&record, provider, model, usage, requested_mode)
        };
        let estimated_cost_usd = if record.override_flag {
            standard_record_cost(&record, usage)
        } else if is_codexbar_record(&record) {
            codexbar_cost(&record, usage, pricing_mode)
        } else {
            standard_record_cost(&record, usage)
        };

        Ok(CostEstimate {
            estimated_cost_usd,
            pricing_version: record.snapshot_version,
            pricing_mode,
            priced: true,
        })
    } else {
        Ok(CostEstimate {
            estimated_cost_usd: 0.0,
            pricing_version: "unpriced".to_string(),
            pricing_mode: PricingMode::Unpriced,
            priced: false,
        })
    }
}

pub fn override_price(
    db: &Database,
    provider: &str,
    model: &str,
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
) -> Result<()> {
    let record = PricingRecord {
        provider: provider.to_string(),
        model: model.to_string(),
        input_rate: input,
        output_rate: output,
        cache_read_rate: cache_read.unwrap_or(input),
        cache_write_rate: cache_write.unwrap_or(input),
        source_label: "manual override".to_string(),
        snapshot_version: format!("manual-{}", Utc::now().format("%Y-%m-%d")),
        override_flag: true,
        local_free_flag: false,
        updated_at: Utc::now().to_rfc3339(),
    };
    db.upsert_pricing_record(&record, true)
}

pub fn mark_free(db: &Database, provider: &str, model: &str) -> Result<()> {
    let record = PricingRecord {
        provider: provider.to_string(),
        model: model.to_string(),
        input_rate: 0.0,
        output_rate: 0.0,
        cache_read_rate: 0.0,
        cache_write_rate: 0.0,
        source_label: "manual local/free override".to_string(),
        snapshot_version: format!("manual-free-{}", Utc::now().format("%Y-%m-%d")),
        override_flag: true,
        local_free_flag: true,
        updated_at: Utc::now().to_rfc3339(),
    };
    db.upsert_pricing_record(&record, true)
}

fn per_million(tokens: u64, rate: f64) -> f64 {
    (tokens as f64 / 1_000_000.0) * rate
}

fn standard_record_cost(record: &PricingRecord, usage: &UsageNumbers) -> f64 {
    let output_tokens = usage.completion_tokens + usage.reasoning_tokens;
    per_million(usage.prompt_tokens, record.input_rate)
        + per_million(output_tokens, record.output_rate)
        + per_million(usage.cache_read_tokens, record.cache_read_rate)
        + per_million(usage.cache_write_tokens, record.cache_write_rate)
}

fn resolve_pricing_mode(
    record: &PricingRecord,
    provider: &str,
    model: &str,
    usage: &UsageNumbers,
    requested_mode: Option<PricingMode>,
) -> PricingMode {
    if !is_codexbar_record(record) {
        return PricingMode::Standard;
    }
    if requested_mode == Some(PricingMode::Priority) {
        return PricingMode::Priority;
    }
    if requested_mode == Some(PricingMode::LongContext)
        || codex_uses_long_context_rates(provider, model, record, usage)
    {
        return PricingMode::LongContext;
    }
    PricingMode::Standard
}

fn codex_uses_long_context_rates(
    _provider: &str,
    _model: &str,
    record: &PricingRecord,
    usage: &UsageNumbers,
) -> bool {
    codexbar_long_rates(&record.model).is_some()
        && codex_total_input_tokens(usage) > CODEX_LONG_CONTEXT_THRESHOLD_TOKENS
}

fn codex_total_input_tokens(usage: &UsageNumbers) -> u64 {
    usage
        .prompt_tokens
        .saturating_add(usage.cache_read_tokens)
        .saturating_add(usage.cache_write_tokens)
}

fn is_codexbar_record(record: &PricingRecord) -> bool {
    record
        .source_label
        .to_ascii_lowercase()
        .contains("codexbar")
}

#[derive(Debug, Clone, Copy)]
struct RateSet {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

fn codexbar_cost(record: &PricingRecord, usage: &UsageNumbers, mode: PricingMode) -> f64 {
    if mode == PricingMode::Priority {
        if let Some(rates) = codexbar_priority_rates(&record.model)
            .filter(|_| codex_total_input_tokens(usage) <= CODEX_PRIORITY_INPUT_TOKEN_LIMIT)
        {
            return codex_cost_with_rates(usage, rates);
        }
    }

    let use_long_rates =
        codex_uses_long_context_rates(&record.provider, &record.model, record, usage);
    let rates = if use_long_rates {
        codexbar_long_rates(&record.model).unwrap_or_else(|| standard_rates(record))
    } else {
        standard_rates(record)
    };
    codex_cost_with_rates(usage, rates)
}

fn codex_cost_with_rates(usage: &UsageNumbers, rates: RateSet) -> f64 {
    let output_tokens = usage.completion_tokens + usage.reasoning_tokens;
    per_million(usage.prompt_tokens, rates.input)
        + per_million(output_tokens, rates.output)
        + per_million(usage.cache_read_tokens, rates.cache_read)
        + per_million(usage.cache_write_tokens, rates.cache_write)
}

fn standard_rates(record: &PricingRecord) -> RateSet {
    RateSet {
        input: record.input_rate,
        output: record.output_rate,
        cache_read: record.cache_read_rate,
        cache_write: record.cache_write_rate,
    }
}

fn codexbar_long_rates(model: &str) -> Option<RateSet> {
    match model {
        "gpt-5.5" => Some(RateSet {
            input: 10.0,
            output: 45.0,
            cache_read: 1.0,
            cache_write: 0.0,
        }),
        "gpt-5.4" => Some(RateSet {
            input: 5.0,
            output: 22.5,
            cache_read: 0.5,
            cache_write: 0.0,
        }),
        _ => None,
    }
}

fn codexbar_priority_rates(model: &str) -> Option<RateSet> {
    match model {
        "gpt-5.5" => Some(RateSet {
            input: 12.50,
            output: 75.0,
            cache_read: 1.25,
            cache_write: 0.0,
        }),
        "gpt-5.4" => Some(RateSet {
            input: 5.0,
            output: 30.0,
            cache_read: 0.5,
            cache_write: 0.0,
        }),
        "gpt-5.4-mini" => Some(RateSet {
            input: 1.50,
            output: 9.0,
            cache_read: 0.15,
            cache_write: 0.0,
        }),
        _ => None,
    }
}

fn reclassify_legacy_codex_usage(db: &Database) -> Result<()> {
    let conn = db.connection()?;
    let mut stmt = conn.prepare(
        r#"
        SELECT id, provider, model, prompt_tokens, completion_tokens, cache_read_tokens,
            cache_write_tokens, reasoning_tokens
        FROM usage_events
        WHERE provider IN ('openai', 'openai-codex', 'openai-code', 'codex', 'codex-openai')
            AND model IN ('gpt-5.5-fast', 'gpt-5.5-long', 'gpt-5.4-fast')
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
                UsageNumbers {
                    prompt_tokens: row.get::<_, i64>(3)? as u64,
                    completion_tokens: row.get::<_, i64>(4)? as u64,
                    cache_read_tokens: row.get::<_, i64>(5)? as u64,
                    cache_write_tokens: row.get::<_, i64>(6)? as u64,
                    reasoning_tokens: row.get::<_, i64>(7)? as u64,
                },
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);

    for (id, provider, legacy_model, usage) in rows {
        let base_model = legacy_model
            .strip_suffix("-fast")
            .or_else(|| legacy_model.strip_suffix("-long"))
            .unwrap_or(&legacy_model);
        let requested_mode = if legacy_model.ends_with("-fast") {
            Some(PricingMode::Priority)
        } else if legacy_model.ends_with("-long") {
            Some(PricingMode::LongContext)
        } else {
            None
        };
        let estimate = estimate_cost(db, &provider, base_model, &usage, requested_mode)?;
        conn.execute(
            r#"
            UPDATE usage_events
            SET model = ?1,
                total_tokens = ?2,
                estimated_cost_usd = ?3,
                pricing_version = ?4,
                pricing_mode = ?5
            WHERE id = ?6
            "#,
            rusqlite::params![
                base_model,
                usage.total_tokens(),
                estimate.estimated_cost_usd,
                estimate.pricing_version,
                estimate.pricing_mode.as_str(),
                id,
            ],
        )?;
    }

    Ok(())
}

fn bundled_records() -> Vec<PricingRecord> {
    let now = Utc::now().to_rfc3339();
    [
        rec(
            "openai",
            "gpt-5.5",
            5.0,
            30.0,
            0.50,
            0.0,
            "CodexBar model catalog",
        ),
        rec(
            "openai",
            "gpt-5.4",
            2.50,
            15.0,
            0.25,
            0.0,
            "CodexBar model catalog",
        ),
        rec(
            "openai",
            "gpt-5.4-mini",
            0.75,
            4.50,
            0.075,
            0.0,
            "CodexBar model catalog",
        ),
        rec(
            "openai",
            "gpt-5.4-nano",
            0.20,
            1.25,
            0.02,
            0.0,
            "CodexBar model catalog",
        ),
        rec(
            "openai",
            "gpt-5.3-codex",
            1.75,
            14.0,
            0.175,
            0.0,
            "CodexBar model catalog",
        ),
        rec(
            "openai",
            "codex-mini-latest",
            1.50,
            6.0,
            0.375,
            0.0,
            "CodexBar model catalog",
        ),
        rec(
            "anthropic",
            "claude-opus-4-8",
            5.0,
            25.0,
            0.50,
            6.25,
            "Anthropic pricing",
        ),
        rec(
            "anthropic",
            "claude-opus-4-7",
            5.0,
            25.0,
            0.50,
            6.25,
            "Anthropic pricing",
        ),
        rec(
            "anthropic",
            "claude-opus-4-6",
            5.0,
            25.0,
            0.50,
            6.25,
            "Anthropic pricing",
        ),
        rec(
            "anthropic",
            "claude-opus-4-5",
            5.0,
            25.0,
            0.50,
            6.25,
            "Anthropic pricing",
        ),
        rec(
            "anthropic",
            "claude-sonnet-4-6",
            3.0,
            15.0,
            0.30,
            3.75,
            "Anthropic pricing",
        ),
        rec(
            "anthropic",
            "claude-sonnet-4-5",
            3.0,
            15.0,
            0.30,
            3.75,
            "Anthropic pricing",
        ),
        rec(
            "anthropic",
            "claude-haiku-4-5",
            1.0,
            5.0,
            0.10,
            1.25,
            "Anthropic pricing",
        ),
    ]
    .into_iter()
    .map(|mut record| {
        record.updated_at = now.clone();
        record
    })
    .collect()
}

fn rec(
    provider: &str,
    model: &str,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
    source_label: &str,
) -> PricingRecord {
    PricingRecord {
        provider: provider.to_string(),
        model: model.to_string(),
        input_rate: input,
        output_rate: output,
        cache_read_rate: cache_read,
        cache_write_rate: cache_write,
        source_label: source_label.to_string(),
        snapshot_version: BUNDLED_PRICING_VERSION.to_string(),
        override_flag: false,
        local_free_flag: false,
        updated_at: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::importers::{SourceKind, UsageEvent};

    use super::*;

    #[test]
    fn calculates_prompt_completion_cache_and_reasoning_cost() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("pricing.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();

        let usage = UsageNumbers {
            prompt_tokens: 1_000_000,
            completion_tokens: 500_000,
            cache_read_tokens: 250_000,
            cache_write_tokens: 100_000,
            reasoning_tokens: 100_000,
        };
        let estimate = estimate_cost(&db, "anthropic", "claude-sonnet-4-6", &usage, None).unwrap();
        assert!(estimate.priced);
        assert_eq!(estimate.pricing_mode, PricingMode::Standard);
        assert!((estimate.estimated_cost_usd - 12.45).abs() < 0.0001);
    }

    #[test]
    fn manual_free_override_zeroes_cost() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("pricing.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();
        mark_free(&db, "openai", "gpt-5.4").unwrap();

        let usage = UsageNumbers {
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
            cache_read_tokens: 1_000_000,
            cache_write_tokens: 1_000_000,
            reasoning_tokens: 1_000_000,
        };
        let estimate = estimate_cost(&db, "openai", "gpt-5.4", &usage, None).unwrap();
        assert_eq!(estimate.estimated_cost_usd, 0.0);
        assert_eq!(estimate.pricing_mode, PricingMode::Free);
        assert!(estimate.priced);
    }

    #[test]
    fn openai_codex_provider_alias_uses_openai_standard_pricing() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("pricing.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();

        let usage = UsageNumbers {
            prompt_tokens: 100_000,
            completion_tokens: 0,
            cache_read_tokens: 100_000,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        };
        let estimate = estimate_cost(&db, "openai-codex", "gpt-5.4-spark", &usage, None).unwrap();
        assert!(estimate.priced);
        assert_eq!(estimate.pricing_mode, PricingMode::Standard);
        assert!((estimate.estimated_cost_usd - 0.275).abs() < 0.0001);
    }

    #[test]
    fn codex_long_context_uses_codexbar_context_rates() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("pricing.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();

        let usage = UsageNumbers {
            prompt_tokens: 300_000,
            completion_tokens: 20_000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        };
        let estimate = estimate_cost(&db, "openai-codex", "gpt-5.5", &usage, None).unwrap();
        assert!(estimate.priced);
        assert_eq!(estimate.pricing_mode, PricingMode::LongContext);
        assert!((estimate.estimated_cost_usd - 3.9).abs() < 0.0001);
    }

    #[test]
    fn codex_priority_uses_codexbar_priority_rates_only_when_requested() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("pricing.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();

        let usage = UsageNumbers {
            prompt_tokens: 100_000,
            completion_tokens: 10_000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        };
        let standard = estimate_cost(&db, "openai-codex", "gpt-5.5", &usage, None).unwrap();
        let priority = estimate_cost(
            &db,
            "openai-codex",
            "gpt-5.5",
            &usage,
            Some(PricingMode::Priority),
        )
        .unwrap();

        assert_eq!(standard.pricing_mode, PricingMode::Standard);
        assert_eq!(priority.pricing_mode, PricingMode::Priority);
        assert!((standard.estimated_cost_usd - 0.8).abs() < 0.0001);
        assert!((priority.estimated_cost_usd - 2.0).abs() < 0.0001);
    }

    #[test]
    fn legacy_fast_model_rows_reclassify_as_priority_usage() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("pricing.sqlite3")).unwrap();
        db.migrate().unwrap();
        db.upsert_usage_event(&UsageEvent {
            machine: "test-machine".to_string(),
            source: SourceKind::Codex,
            project_path: "/repo".to_string(),
            session_id: "legacy-fast".to_string(),
            turn_id: Some("turn-legacy-fast".to_string()),
            provider: "openai-codex".to_string(),
            model: "gpt-5.5-fast".to_string(),
            prompt_tokens: 100_000,
            completion_tokens: 10_000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: 110_000,
            estimated_cost_usd: 0.0,
            confidence: 0.92,
            event_timestamp: None,
            raw_path: "/tmp/session.jsonl".to_string(),
            raw_span: None,
            parser_name: "test-parser".to_string(),
            parser_version: "test".to_string(),
            raw_event_hash: "legacy-fast-hash".to_string(),
            imported_at: Utc::now().to_rfc3339(),
            pricing_version: "2026-06-07-legacy-codexbar".to_string(),
            pricing_mode: PricingMode::Standard,
            metadata_only: true,
        })
        .unwrap();

        seed_bundled_pricing(&db).unwrap();

        let row = db
            .connection()
            .unwrap()
            .query_row(
                "SELECT model, pricing_mode, estimated_cost_usd FROM usage_events",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, f64>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, "gpt-5.5");
        assert_eq!(row.1, "priority");
        assert!((row.2 - 2.0).abs() < 0.0001);
    }

    #[test]
    fn codex_priority_rates_are_not_a_blanket_multiplier() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("pricing.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();

        let usage = UsageNumbers {
            prompt_tokens: 100_000,
            completion_tokens: 10_000,
            cache_read_tokens: 10_000,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        };
        let standard = estimate_cost(&db, "openai-codex", "gpt-5.4", &usage, None).unwrap();
        let priority = estimate_cost(
            &db,
            "openai-codex",
            "gpt-5.4",
            &usage,
            Some(PricingMode::Priority),
        )
        .unwrap();

        assert_eq!(standard.pricing_mode, PricingMode::Standard);
        assert_eq!(priority.pricing_mode, PricingMode::Priority);
        assert!((standard.estimated_cost_usd - 0.4025).abs() < 0.0001);
        assert!((priority.estimated_cost_usd - 0.805).abs() < 0.0001);
    }

    #[test]
    fn codex_priority_over_input_cap_falls_back_to_long_context_base_rates() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("pricing.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();

        let usage = UsageNumbers {
            prompt_tokens: 300_000,
            completion_tokens: 20_000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        };
        let estimate = estimate_cost(
            &db,
            "openai-codex",
            "gpt-5.5",
            &usage,
            Some(PricingMode::Priority),
        )
        .unwrap();

        assert_eq!(estimate.pricing_mode, PricingMode::Priority);
        assert!((estimate.estimated_cost_usd - 3.9).abs() < 0.0001);
    }

    #[test]
    fn gpt_cache_writes_are_not_charged_in_codexbar_snapshot() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("pricing.sqlite3")).unwrap();
        db.migrate().unwrap();
        seed_bundled_pricing(&db).unwrap();

        let usage = UsageNumbers {
            prompt_tokens: 0,
            completion_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 1_000_000,
            reasoning_tokens: 0,
        };
        let estimate = estimate_cost(&db, "openai-codex", "gpt-5.5", &usage, None).unwrap();
        assert!(estimate.priced);
        assert_eq!(estimate.estimated_cost_usd, 0.0);
    }

    #[test]
    fn bundled_seed_refreshes_non_override_pricing_rows() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("pricing.sqlite3")).unwrap();
        db.migrate().unwrap();
        db.upsert_pricing_record(
            &rec("openai", "gpt-5.5", 99.0, 99.0, 99.0, 99.0, "stale bundled"),
            false,
        )
        .unwrap();

        seed_bundled_pricing(&db).unwrap();

        let record = db.pricing_record("openai", "gpt-5.5").unwrap().unwrap();
        assert_eq!(record.input_rate, 5.0);
        assert_eq!(record.output_rate, 30.0);
        assert_eq!(record.cache_read_rate, 0.50);
        assert_eq!(record.cache_write_rate, 0.0);
        assert_eq!(record.snapshot_version, BUNDLED_PRICING_VERSION);
    }
}
