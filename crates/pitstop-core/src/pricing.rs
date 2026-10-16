//! What a request would cost at the public API prices, from LiteLLM's
//! price table (`model_prices_and_context_window.json`, MIT).
//!
//! A trimmed copy ships inside Pitstop so costs show offline; the app
//! fetches the current table once a day and keeps it in
//! `~/.pitstop/prices.json`. Models found in neither are unpriced.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::host::Host;
use crate::ledger::Tokens;

pub const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// A fetched table older than this is fetched again.
pub const MAX_AGE_SECS: i64 = 24 * 3_600;

/// Above this many prompt tokens a request is billed at the long-context
/// prices, where the table has them.
const LONG_CONTEXT: u64 = 200_000;

/// One model's prices, per token, under LiteLLM's own field names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Price {
    #[serde(rename = "input_cost_per_token")]
    pub input: f64,
    #[serde(rename = "output_cost_per_token", default)]
    pub output: f64,
    #[serde(
        rename = "cache_creation_input_token_cost",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_write: Option<f64>,
    #[serde(
        rename = "cache_creation_input_token_cost_above_1hr",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_write_1h: Option<f64>,
    #[serde(
        rename = "cache_read_input_token_cost",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_read: Option<f64>,
    #[serde(
        rename = "input_cost_per_token_above_200k_tokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub long_input: Option<f64>,
    #[serde(
        rename = "output_cost_per_token_above_200k_tokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub long_output: Option<f64>,
    #[serde(
        rename = "cache_creation_input_token_cost_above_200k_tokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub long_cache_write: Option<f64>,
    #[serde(
        rename = "cache_read_input_token_cost_above_200k_tokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub long_cache_read: Option<f64>,
}

impl Price {
    /// What `tokens` cost in dollars.
    pub fn cost(&self, tokens: &Tokens) -> f64 {
        let rates = self.rates(tokens);
        tokens.input as f64 * rates.input
            + tokens.cache_write_5m as f64 * rates.cache_write
            + tokens.cache_write_1h as f64 * rates.cache_write_1h
            + tokens.cache_read as f64 * rates.cache_read
            + tokens.output as f64 * rates.output
    }

    /// What reading `tokens`' cached input saved over reading it fresh.
    pub fn savings(&self, tokens: &Tokens) -> f64 {
        let rates = self.rates(tokens);
        tokens.cache_read as f64 * (rates.input - rates.cache_read)
    }

    /// The per-token prices for a request this size: a missing cache price
    /// is the input (or write) price, and past the long-context threshold
    /// the long prices apply, the hour's write scaled with the input.
    fn rates(&self, tokens: &Tokens) -> Rates {
        let base = Rates {
            input: self.input,
            output: self.output,
            cache_write: self.cache_write.unwrap_or(self.input),
            cache_write_1h: self
                .cache_write_1h
                .or(self.cache_write)
                .unwrap_or(self.input),
            cache_read: self.cache_read.unwrap_or(self.input),
        };
        match self.long_input {
            Some(input) if tokens.prompt() > LONG_CONTEXT && self.input > 0.0 => Rates {
                input,
                output: self.long_output.unwrap_or(base.output),
                cache_write: self.long_cache_write.unwrap_or(base.cache_write),
                cache_write_1h: base.cache_write_1h * input / self.input,
                cache_read: self.long_cache_read.unwrap_or(base.cache_read),
            },
            _ => base,
        }
    }
}

struct Rates {
    input: f64,
    output: f64,
    cache_write: f64,
    cache_write_1h: f64,
    cache_read: f64,
}

/// A price table, as fetched or as shipped.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Table {
    /// Epoch seconds the table was fetched at.
    pub fetched_at: Option<i64>,
    pub models: BTreeMap<String, Price>,
}

impl Table {
    /// LiteLLM's whole table cut down to what Pitstop prices with: models
    /// named without a `provider/` prefix that have an input price, and
    /// only their price fields.
    pub fn from_litellm(json: &[u8], fetched_at: i64) -> Result<Table> {
        let all: BTreeMap<String, Value> = serde_json::from_slice(json)
            .map_err(|e| Error::InvalidData(format!("price table: {e}")))?;
        let models = all
            .into_iter()
            .filter(|(name, _)| !name.contains('/') && name != "sample_spec")
            .filter_map(|(name, entry)| Some((name, serde_json::from_value(entry).ok()?)))
            .collect();
        Ok(Table {
            fetched_at: Some(fetched_at),
            models,
        })
    }

    /// A table saved by [`Table::to_json`].
    pub fn parse(json: &[u8]) -> Result<Table> {
        serde_json::from_slice(json).map_err(|e| Error::InvalidData(format!("price table: {e}")))
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// `model` by its own name, then without a `provider/` prefix, then
    /// without a `-YYYYMMDD` date.
    fn find(&self, model: &str) -> Option<&Price> {
        let bare = model.rsplit('/').next().unwrap_or(model);
        let undated = bare
            .rsplit_once('-')
            .filter(|(_, date)| date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()))
            .map(|(name, _)| name);
        [Some(model), Some(bare), undated]
            .into_iter()
            .flatten()
            .find_map(|name| self.models.get(name))
    }
}

/// The fetched table over the shipped one.
#[derive(Debug, Clone)]
pub struct Prices {
    fetched: Option<Table>,
    shipped: Table,
}

impl Prices {
    pub fn new(shipped: Table, fetched: Option<Table>) -> Self {
        Self { fetched, shipped }
    }

    /// The table Pitstop ships with.
    pub fn shipped() -> Table {
        Table::parse(include_bytes!("prices.json")).unwrap_or_default()
    }

    /// The price of `model`, if either table knows it.
    pub fn get(&self, model: &str) -> Option<&Price> {
        self.fetched
            .as_ref()
            .and_then(|t| t.find(model))
            .or_else(|| self.shipped.find(model))
    }

    /// When the table in use was fetched; none for the shipped one.
    pub fn fetched_at(&self) -> Option<i64> {
        self.fetched.as_ref().and_then(|t| t.fetched_at)
    }
}

fn cache_path(host: &dyn Host) -> PathBuf {
    host.home().join(".pitstop").join("prices.json")
}

/// The table fetched last, if one was saved and still reads.
pub fn cached(host: &dyn Host) -> Option<Table> {
    host.read(&cache_path(host))
        .ok()
        .flatten()
        .and_then(|bytes| Table::parse(&bytes).ok())
}

pub fn save(host: &dyn Host, table: &Table) -> Result<()> {
    host.write_atomic(&cache_path(host), &table.to_json())
}

/// Whether `table` (the one saved, if any) should be fetched again.
pub fn stale(table: Option<&Table>, now: i64) -> bool {
    match table.and_then(|t| t.fetched_at) {
        Some(at) => now - at >= MAX_AGE_SECS || at > now,
        None => true,
    }
}

/// LiteLLM's current table, trimmed.
pub fn fetch(now: i64) -> Result<Table> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .into();
    let body = agent
        .get(LITELLM_URL)
        .call()
        .map_err(|e| Error::InvalidData(format!("price table: {e}")))?
        .body_mut()
        .with_config()
        .limit(32 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| Error::InvalidData(format!("price table: {e}")))?;
    Table::from_litellm(&body, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;

    fn opus() -> Price {
        Price {
            input: 4e-6,
            output: 2e-5,
            cache_write: Some(5e-6),
            cache_write_1h: Some(8e-6),
            cache_read: Some(2e-7),
            long_input: None,
            long_output: None,
            long_cache_write: None,
            long_cache_read: None,
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    #[test]
    fn a_request_costs_each_kind_of_token_at_its_own_price() {
        let tokens = Tokens {
            input: 1_000,
            cache_write_5m: 2_000,
            cache_write_1h: 1_000,
            cache_read: 10_000,
            output: 500,
        };
        let price = opus();
        assert!(close(price.cost(&tokens), 0.034), "{}", price.cost(&tokens));
        assert!(
            close(price.savings(&tokens), 0.038),
            "{}",
            price.savings(&tokens)
        );
    }

    #[test]
    fn missing_cache_prices_fall_back_to_the_input_and_write_prices() {
        let price = Price {
            cache_write: None,
            cache_write_1h: None,
            cache_read: None,
            ..opus()
        };
        let tokens = Tokens {
            cache_write_5m: 1_000,
            cache_write_1h: 1_000,
            cache_read: 1_000,
            ..Tokens::default()
        };
        // all three at the input price: 3_000 × 4e-6
        assert!(close(price.cost(&tokens), 0.012), "{}", price.cost(&tokens));
        assert!(close(price.savings(&tokens), 0.0));

        let written_only = Price {
            cache_write_1h: None,
            ..opus()
        };
        let one_hour = Tokens {
            cache_write_1h: 1_000,
            ..Tokens::default()
        };
        // an hour's write at the plain write price: 1_000 × 5e-6
        assert!(close(written_only.cost(&one_hour), 0.005));
    }

    #[test]
    fn a_long_prompt_is_billed_at_the_long_context_prices() {
        let price = Price {
            input: 3e-6,
            output: 1.5e-5,
            cache_write: Some(3.75e-6),
            cache_write_1h: Some(6e-6),
            cache_read: Some(3e-7),
            long_input: Some(6e-6),
            long_output: Some(2.25e-5),
            long_cache_write: Some(7.5e-6),
            long_cache_read: Some(6e-7),
        };
        let short = Tokens {
            input: 100_000,
            cache_read: 100_000,
            output: 1_000,
            ..Tokens::default()
        };
        // 0.3 + 0.03 + 0.015
        assert!(close(price.cost(&short), 0.345), "{}", price.cost(&short));
        let long = Tokens {
            input: 150_000,
            cache_write_1h: 10_000,
            cache_read: 100_000,
            output: 1_000,
            ..Tokens::default()
        };
        // 0.9 + 10_000 × 1.2e-5 (the hour's write doubled with input) + 0.06 + 0.0225
        assert!(close(price.cost(&long), 1.1025), "{}", price.cost(&long));
    }

    const LITELLM: &str = r#"{
        "sample_spec": {"input_cost_per_token": 0.0, "output_cost_per_token": 0.0, "litellm_provider": "one of https://docs.litellm.ai/docs/providers"},
        "claude-opus-5-5": {"input_cost_per_token": 4e-06, "output_cost_per_token": 2e-05, "cache_creation_input_token_cost": 5e-06, "cache_creation_input_token_cost_above_1hr": 8e-06, "cache_read_input_token_cost": 2e-07, "litellm_provider": "anthropic", "max_input_tokens": 1000000, "mode": "chat", "supports_vision": true},
        "vertex_ai/claude-opus-5-5": {"input_cost_per_token": 9e-06, "output_cost_per_token": 9e-05, "litellm_provider": "vertex_ai"},
        "gpt-6-luna": {"input_cost_per_token": 1e-07, "output_cost_per_token": 5e-07, "cache_read_input_token_cost": 1e-08, "litellm_provider": "openai", "mode": "chat"},
        "text-embedding-9": {"output_vector_size": 3072, "litellm_provider": "openai", "mode": "embedding"},
        "dall-e-9": {"input_cost_per_pixel": 1e-08, "litellm_provider": "openai", "mode": "image_generation"}
    }"#;

    #[test]
    fn the_litellm_table_is_cut_down_to_priced_models_named_without_a_provider() {
        let table = Table::from_litellm(LITELLM.as_bytes(), 1_800_000_000).unwrap();
        let names: Vec<&str> = table.models.keys().map(String::as_str).collect();
        assert_eq!(names, ["claude-opus-5-5", "gpt-6-luna"]);
        assert_eq!(table.models["claude-opus-5-5"], opus());
        assert_eq!(table.fetched_at, Some(1_800_000_000));
        let saved = String::from_utf8(table.to_json()).unwrap();
        assert!(!saved.contains("supports_vision"), "{saved}");
        assert_eq!(Table::parse(saved.as_bytes()).unwrap(), table);
    }

    #[test]
    fn a_model_is_found_by_its_name_without_provider_or_date() {
        let prices = Prices::new(Table::from_litellm(LITELLM.as_bytes(), 0).unwrap(), None);
        let input = |model: &str| prices.get(model).map(|p| p.input);
        assert_eq!(input("claude-opus-5-5"), Some(4e-6));
        assert_eq!(input("anthropic/claude-opus-5-5"), Some(4e-6));
        assert_eq!(input("claude-opus-5-5-20261001"), Some(4e-6));
        assert_eq!(input("deepseek-v4-1-flash"), None);
        assert_eq!(
            input("claude-opus-5"),
            None,
            "a prefix of a name is another model"
        );
    }

    #[test]
    fn the_fetched_table_wins_over_the_shipped_one() {
        let shipped = Table::from_litellm(LITELLM.as_bytes(), 0).unwrap();
        let mut fetched = Table::default();
        fetched.models.insert(
            "gpt-6-luna".into(),
            Price {
                input: 2e-7,
                ..opus()
            },
        );
        let prices = Prices::new(shipped, Some(fetched));
        assert_eq!(prices.get("gpt-6-luna").map(|p| p.input), Some(2e-7));
        assert_eq!(prices.get("claude-opus-5-5").map(|p| p.input), Some(4e-6));
    }

    #[test]
    fn a_saved_table_is_read_back_and_fetched_again_after_a_day() {
        let host = FakeHost::new();
        assert_eq!(cached(&host), None);
        let table = Table::from_litellm(LITELLM.as_bytes(), 1_000_000).unwrap();
        save(&host, &table).unwrap();
        let saved = cached(&host);
        assert_eq!(saved.as_ref(), Some(&table));
        assert!(!stale(saved.as_ref(), 1_000_000 + MAX_AGE_SECS - 1));
        assert!(stale(saved.as_ref(), 1_000_000 + MAX_AGE_SECS));
        assert!(stale(None, 1_000_000));
    }

    #[test]
    fn pitstop_ships_prices_for_the_models_its_users_run() {
        let prices = Prices::new(Prices::shipped(), None);
        for model in ["claude-opus-5-5", "claude-sonnet-5-5", "gpt-5.6-sol"] {
            assert!(prices.get(model).is_some(), "{model} is unpriced");
        }
    }
}
