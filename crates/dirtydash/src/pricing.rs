use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::importers::UsageNumbers;

pub const BUNDLED_PRICING_VERSION: &str = "2026-06-06-bundled";

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEstimate {
    pub estimated_cost_usd: f64,
    pub pricing_version: String,
    pub priced: bool,
}

pub fn seed_bundled_pricing(db: &Database) -> Result<()> {
    for record in bundled_records() {
        db.upsert_pricing_record(&record, false)?;
    }
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
) -> Result<CostEstimate> {
    if let Some(record) = db.pricing_record(provider, model)? {
        if record.local_free_flag {
            return Ok(CostEstimate {
                estimated_cost_usd: 0.0,
                pricing_version: record.snapshot_version,
                priced: true,
            });
        }

        let output_tokens = usage.completion_tokens + usage.reasoning_tokens;
        let estimated_cost_usd = per_million(usage.prompt_tokens, record.input_rate)
            + per_million(output_tokens, record.output_rate)
            + per_million(usage.cache_read_tokens, record.cache_read_rate)
            + per_million(usage.cache_write_tokens, record.cache_write_rate);

        Ok(CostEstimate {
            estimated_cost_usd,
            pricing_version: record.snapshot_version,
            priced: true,
        })
    } else {
        Ok(CostEstimate {
            estimated_cost_usd: 0.0,
            pricing_version: "unpriced".to_string(),
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

fn bundled_records() -> Vec<PricingRecord> {
    let now = Utc::now().to_rfc3339();
    [
        rec("openai", "gpt-5.5", 5.0, 30.0, 0.50, 5.0, "OpenAI pricing"),
        rec(
            "openai",
            "gpt-5.4",
            2.50,
            15.0,
            0.25,
            2.50,
            "OpenAI pricing",
        ),
        rec(
            "openai",
            "gpt-5.4-mini",
            0.75,
            4.50,
            0.075,
            0.75,
            "OpenAI pricing",
        ),
        rec(
            "openai",
            "gpt-5.4-nano",
            0.20,
            1.25,
            0.02,
            0.20,
            "OpenAI pricing",
        ),
        rec(
            "openai",
            "gpt-5.3-codex",
            1.75,
            14.0,
            0.175,
            1.75,
            "OpenAI pricing",
        ),
        rec(
            "openai",
            "codex-mini-latest",
            1.50,
            6.0,
            0.375,
            1.50,
            "OpenAI pricing",
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
        let estimate = estimate_cost(&db, "anthropic", "claude-sonnet-4-6", &usage).unwrap();
        assert!(estimate.priced);
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
        let estimate = estimate_cost(&db, "openai", "gpt-5.4", &usage).unwrap();
        assert_eq!(estimate.estimated_cost_usd, 0.0);
        assert!(estimate.priced);
    }
}
