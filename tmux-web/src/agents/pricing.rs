//! Versioned, dated token-price estimates independent of reported charges.
//!
//! Rates are USD per million tokens. Unknown models, unsupported fast modes
//! and cache writes without enough TTL information deliberately remain unpriced.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use reqwest::{redirect, Url};
use serde::{Deserialize, Serialize};

use super::model::{PriceStatus, UsageRecord};

pub const MAX_PRICE_BYTES: usize = 1024 * 1024;
pub const PRICE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PriceCatalog {
    pub version: u32,
    pub date: String,
    pub source: String,
    pub rows: Vec<ModelPrice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelPrice {
    pub model: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub source: String,
    pub rates: PriceRates,
    #[serde(default)]
    pub fast_multiplier: Option<f64>,
    #[serde(default)]
    pub long_context: Option<LongContextPrice>,
    #[serde(default)]
    pub cache_write_policy: CacheWritePolicy,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheWritePolicy {
    /// Different retention prices exist but a journal does not name the TTL.
    #[default]
    Unknown,
    /// Usage splits the aggregate writes into five-minute and one-hour tokens.
    Claude,
    /// An explicitly configured model has only one cache-write rate.
    Single,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PriceRates {
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(default)]
    pub cache_write_1h: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LongContextPrice {
    /// The higher rates apply strictly above this prompt-token count.
    pub above: u64,
    pub rates: PriceRates,
}

impl Default for PriceCatalog {
    fn default() -> Self {
        Self::decode(include_bytes!("prices.json"))
            .expect("bundled agent price catalogue is validated by tests")
    }
}

impl PriceCatalog {
    /// Load an explicitly configured or cached catalogue. Bad files are ignored.
    pub fn load(path: &Path) -> Option<Self> {
        let initial = std::fs::metadata(path).ok()?;
        if !initial.is_file() || initial.len() > MAX_PRICE_BYTES as u64 {
            return None;
        }
        let file = std::fs::File::open(path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > MAX_PRICE_BYTES as u64 {
            return None;
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take((MAX_PRICE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        Self::decode(&bytes).ok()
    }

    /// Save a validated catalogue atomically; a failed update never replaces it.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_PRICE_BYTES {
            bail!("price catalogue exceeds the size limit");
        }
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        forge_atomic::write_bytes_atomic(path, &bytes)
    }

    pub fn status(&self) -> PriceStatus {
        PriceStatus {
            date: Some(self.date.clone()),
            source: Some(self.source.clone()),
            models: self.rows.len(),
            error: None,
        }
    }

    /// Estimate token API value. Provider-reported cost is deliberately not used.
    ///
    /// Aggregate session counters (such as Copilot shutdown metrics) use base
    /// rates: their sum cannot establish a per-request context tier.
    pub fn price(&self, usage: &UsageRecord) -> Option<f64> {
        let tokens = usage.tokens.as_ref()?;
        let canonical = usage.model.to_ascii_lowercase();
        let name = normalized_model(&canonical);
        let model = self.rows.iter().find(|row| row.matches(name))?;
        let prompt = tokens
            .input
            .saturating_add(tokens.cache_read)
            .saturating_add(tokens.cache_write);
        let rates = if usage.requests <= 1 {
            model
                .long_context
                .as_ref()
                .filter(|tier| prompt > tier.above)
                .map(|tier| &tier.rates)
                .unwrap_or(&model.rates)
        } else {
            &model.rates
        };
        let fast = if usage.fast {
            model.fast_multiplier?
        } else {
            1.0
        };
        let cached = if tokens.cache_read > 0 {
            tokens.cache_read as f64 * rates.cache_read?
        } else {
            0.0
        };
        let writes = if tokens.cache_write == 0 {
            0.0
        } else {
            match model.cache_write_policy {
                CacheWritePolicy::Unknown => return None,
                CacheWritePolicy::Single => tokens.cache_write as f64 * rates.cache_write?,
                CacheWritePolicy::Claude => {
                    let long = usage.cache_write_1h?;
                    if long > tokens.cache_write {
                        return None;
                    }
                    let short_cost = if tokens.cache_write > long {
                        (tokens.cache_write - long) as f64 * rates.cache_write?
                    } else {
                        0.0
                    };
                    let long_cost = if long > 0 {
                        long as f64 * rates.cache_write_1h?
                    } else {
                        0.0
                    };
                    short_cost + long_cost
                }
            }
        };
        let estimate = (tokens.input as f64 * rates.input
            + tokens.output as f64 * rates.output
            + cached
            + writes)
            * fast
            / 1_000_000.0;
        (estimate.is_finite() && estimate >= 0.0).then_some(estimate)
    }

    /// Fetch a replacement without mutating this last known-good catalogue.
    /// Scheduling (24-hour refresh / six-hour retry) belongs to the store.
    pub async fn update(&self, url: &str) -> Result<Self> {
        let url = checked_url(url)?;
        let initial = url.clone();
        let client = reqwest::Client::builder()
            .https_only(true)
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .referer(false)
            .redirect(redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= 4 || !same_origin(&initial, attempt.url()) {
                    attempt.error("price catalogue redirect refused")
                } else {
                    attempt.follow()
                }
            }))
            .build()?;
        // A fresh client has no cookie jar, credentials or usage headers.
        let mut response = client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, "Devforge-Agent-Prices/1")
            .send()
            .await?
            .error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_PRICE_BYTES as u64)
        {
            bail!("price catalogue exceeds the size limit");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            append_bounded(&mut bytes, &chunk)?;
        }
        self.replacement(&bytes)
    }

    fn replacement(&self, bytes: &[u8]) -> Result<Self> {
        let replacement = Self::decode(bytes).context("invalid price catalogue update")?;
        if replacement.date < self.date {
            bail!("price catalogue update is older than the current catalogue");
        }
        Ok(replacement)
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_PRICE_BYTES {
            bail!("price catalogue exceeds the size limit");
        }
        let catalog: Self = serde_json::from_slice(bytes)?;
        catalog.validate()?;
        Ok(catalog)
    }

    fn validate(&self) -> Result<()> {
        if self.version != PRICE_SCHEMA_VERSION {
            bail!("unsupported price catalogue version");
        }
        if self.date.len() != 10 || NaiveDate::parse_from_str(&self.date, "%Y-%m-%d").is_err() {
            bail!("invalid price catalogue date");
        }
        if self.source.is_empty()
            || self.source.len() > 4096
            || self.rows.is_empty()
            || self.rows.len() > 2048
        {
            bail!("invalid price catalogue metadata");
        }
        let mut names = BTreeSet::new();
        for row in &self.rows {
            checked_url(&row.source)?;
            if row.aliases.len() > 32 {
                bail!("too many model aliases");
            }
            for name in std::iter::once(&row.model).chain(row.aliases.iter()) {
                if name.is_empty()
                    || name.len() > 160
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-._/".contains(&byte))
                    || name != &name.to_ascii_lowercase()
                    || !names.insert(name)
                {
                    bail!("invalid or duplicate model identifier");
                }
            }
            row.rates.validate()?;
            if row
                .fast_multiplier
                .is_some_and(|rate| !valid_rate(rate) || rate < 1.0)
            {
                bail!("invalid fast multiplier");
            }
            if let Some(tier) = &row.long_context {
                if tier.above == 0 {
                    bail!("invalid context threshold");
                }
                tier.rates.validate()?;
            }
        }
        Ok(())
    }
}

impl ModelPrice {
    fn matches(&self, candidate: &str) -> bool {
        std::iter::once(self.model.as_str())
            .chain(self.aliases.iter().map(String::as_str))
            .any(|known| {
                candidate == known
                    || candidate
                        .strip_prefix(known)
                        .is_some_and(|tail| snapshot_date(tail))
            })
    }
}
impl PriceRates {
    fn validate(&self) -> Result<()> {
        if !valid_rate(self.input)
            || !valid_rate(self.output)
            || [self.cache_read, self.cache_write, self.cache_write_1h]
                .into_iter()
                .flatten()
                .any(|rate| !valid_rate(rate))
        {
            bail!("invalid model token rate");
        }
        Ok(())
    }
}
fn valid_rate(rate: f64) -> bool {
    rate.is_finite() && (0.0..=100_000.0).contains(&rate)
}
fn normalized_model(name: &str) -> &str {
    let name = name.trim();
    name.strip_prefix("openai/")
        .or_else(|| name.strip_prefix("anthropic/"))
        .unwrap_or(name)
}
fn snapshot_date(suffix: &str) -> bool {
    let Some(date) = suffix.strip_prefix('-') else {
        return false;
    };
    match date.len() {
        8 if date.bytes().all(|byte| byte.is_ascii_digit()) => {
            NaiveDate::parse_from_str(date, "%Y%m%d").is_ok()
        }
        10 => NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok(),
        _ => false,
    }
}
fn checked_url(value: &str) -> Result<Url> {
    let url = Url::parse(value)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("price catalogue URL must use HTTPS without credentials");
    }
    Ok(url)
}
fn same_origin(initial: &Url, next: &Url) -> bool {
    next.scheme() == "https"
        && next.host_str() == initial.host_str()
        && next.port_or_known_default() == initial.port_or_known_default()
        && next.username().is_empty()
        && next.password().is_none()
}
fn append_bounded(bytes: &mut Vec<u8>, chunk: &[u8]) -> Result<()> {
    if bytes.len().saturating_add(chunk.len()) > MAX_PRICE_BYTES {
        bail!("price catalogue exceeds the size limit");
    }
    bytes.extend_from_slice(chunk);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::model::{AgentProvider, AgentTokens};

    fn usage(model: &str, tokens: AgentTokens) -> UsageRecord {
        UsageRecord {
            id: "response".into(),
            session_id: "session".into(),
            root_session_id: "session".into(),
            provider: AgentProvider::Codex,
            profile: "profile".into(),
            project: "project".into(),
            model: model.into(),
            at_ms: 0,
            tokens: Some(tokens),
            requests: 1,
            estimated_cost_usd: None,
            reported_cost_usd: None,
            fast: false,
            cache_write_1h: None,
        }
    }
    fn close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-10, "{actual} != {expected}");
    }

    #[test]
    fn token_math_does_not_double_bill_reasoning_or_reported_cost() {
        let catalog = PriceCatalog::default();
        let mut record = usage(
            "gpt-6.1-sol",
            AgentTokens {
                input: 1_000_000,
                output: 2_000_000,
                cache_read: 1_000_000,
                cache_write: 0,
                reasoning: Some(1_000_000),
            },
        );
        record.reported_cost_usd = Some(999.0);
        close(catalog.price(&record).unwrap(), 22.1);
        record.fast = true;
        close(catalog.price(&record).unwrap(), 44.2);
    }
    #[test]
    fn aliases_are_exact_or_dated_known_snapshots() {
        let catalog = PriceCatalog::default();
        let tokens = AgentTokens {
            input: 1000,
            output: 1000,
            ..Default::default()
        };
        for name in [
            "gpt-6-astra",
            "openai/gpt-6-astra-2026-10-01",
            "claude-sonnet-4-5-20250929",
            "anthropic/claude-sonnet-4.5",
        ] {
            assert!(
                catalog.price(&usage(name, tokens.clone())).is_some(),
                "{name}"
            );
        }
        for name in [
            "fake-gpt-6-astra",
            "gpt-6-astra-new",
            "claude-sonnet-4-50",
            "claude-sonnet-4-5-20251399",
            "other/claude-sonnet-4-5",
        ] {
            assert!(
                catalog.price(&usage(name, tokens.clone())).is_none(),
                "{name}"
            );
        }
    }
    #[test]
    fn unknown_model_tokens_and_fast_tier_remain_unpriced() {
        let catalog = PriceCatalog::default();
        let mut record = usage(
            "claude-sonnet-5-5",
            AgentTokens {
                input: 1000,
                ..Default::default()
            },
        );
        record.fast = true;
        assert_eq!(catalog.price(&record), None);
        record.fast = false;
        record.model = "unknown-model".into();
        assert_eq!(catalog.price(&record), None);
        record.model = "gpt-6-astra".into();
        record.tokens = None;
        assert_eq!(catalog.price(&record), None);
    }
    #[test]
    fn claude_cache_ttl_is_required_and_fast_multiplier_stacks() {
        let catalog = PriceCatalog::default();
        let mut record = usage(
            "claude-opus-5-5",
            AgentTokens {
                cache_write: 1_000_000,
                ..Default::default()
            },
        );
        assert_eq!(catalog.price(&record), None);
        record.cache_write_1h = Some(250_000);
        close(catalog.price(&record).unwrap(), 5.75);
        record.fast = true;
        close(catalog.price(&record).unwrap(), 11.5);
        record.cache_write_1h = Some(1_000_001);
        assert_eq!(catalog.price(&record), None);
        record.model = "gpt-6-astra".into();
        record.cache_write_1h = None;
        assert_eq!(catalog.price(&record), None);
    }
    #[test]
    fn context_threshold_counts_cached_prompt_tokens_but_not_output() {
        let catalog = PriceCatalog::default();
        let mut record = usage(
            "claude-haiku-5-5",
            AgentTokens {
                input: 1,
                cache_read: 99_999,
                output: 1000,
                ..Default::default()
            },
        );
        close(
            catalog.price(&record).unwrap(),
            (0.1 + 999.99 + 500.0) / 1_000_000.0,
        );
        record.tokens.as_mut().unwrap().input = 2;
        close(
            catalog.price(&record).unwrap(),
            (1.0 + 4_999.95 + 2500.0) / 1_000_000.0,
        );
        record.requests = 2;
        close(
            catalog.price(&record).unwrap(),
            (0.2 + 999.99 + 500.0) / 1_000_000.0,
        );
        let mut legacy = usage(
            "gpt-5.4",
            AgentTokens {
                input: 272_000,
                output: 1000,
                ..Default::default()
            },
        );
        close(catalog.price(&legacy).unwrap(), 0.695);
        legacy.tokens.as_mut().unwrap().input = 272_001;
        close(catalog.price(&legacy).unwrap(), 1.382505);
    }
    #[test]
    fn invalid_catalogues_do_not_replace_last_good_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("prices.json");
        let catalog = PriceCatalog::default();
        catalog.save(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut invalid = catalog.clone();
        invalid.rows[0].rates.input = -1.0;
        assert!(invalid.save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(PriceCatalog::decode(
            br#"{"version":99,"date":"2026-10-25","source":"test","rows":[]}"#
        )
        .is_err());
        assert!(PriceCatalog::decode(b"broken JSON").is_err());
        assert!(catalog.replacement(b"broken JSON").is_err());
        let mut stale = catalog.clone();
        stale.date = "2026-10-24".into();
        assert!(catalog
            .replacement(&serde_json::to_vec(&stale).unwrap())
            .is_err());
        assert_eq!(
            PriceCatalog::load(&path).unwrap().status().models,
            catalog.rows.len()
        );
        assert!(PriceCatalog::load(directory.path()).is_none());
    }
    #[test]
    fn download_bound_and_origin_checks_are_strict() {
        let origin = checked_url("https://prices.example.test/catalogue.json").unwrap();
        assert!(same_origin(
            &origin,
            &Url::parse("https://prices.example.test/next.json").unwrap()
        ));
        for target in [
            "http://prices.example.test/next",
            "https://other.example.test/next",
            "https://prices.example.test:8443/next",
            "https://user@prices.example.test/next",
        ] {
            assert!(!same_origin(&origin, &Url::parse(target).unwrap()));
        }
        assert!(checked_url("http://localhost/prices").is_err());
        assert!(checked_url("https://user:secret@example.test/prices").is_err());
        let mut bytes = vec![0; MAX_PRICE_BYTES - 1];
        append_bounded(&mut bytes, &[1]).unwrap();
        assert!(append_bounded(&mut bytes, &[2]).is_err());
    }
    #[tokio::test]
    async fn rejected_update_keeps_original_catalogue_usable() {
        let catalog = PriceCatalog::default();
        let original = serde_json::to_value(&catalog).unwrap();
        assert!(catalog
            .update("http://localhost/no-plain-http")
            .await
            .is_err());
        assert_eq!(serde_json::to_value(&catalog).unwrap(), original);
        assert!(catalog
            .price(&usage(
                "gpt-6.1-sol",
                AgentTokens {
                    input: 1,
                    ..Default::default()
                }
            ))
            .is_some());
    }
}
