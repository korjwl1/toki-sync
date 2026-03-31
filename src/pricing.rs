use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// Per-model pricing (cost per token).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    #[serde(default)]
    pub cache_creation_input_token_cost: Option<f64>,
    #[serde(default)]
    pub cache_read_input_token_cost: Option<f64>,
}

impl ModelPricing {
    /// Calculate cost from token counts.
    pub fn cost(&self, input: u64, output: u64, cache_create: u64, cache_read: u64) -> f64 {
        (input as f64) * self.input_cost_per_token
            + (output as f64) * self.output_cost_per_token
            + (cache_create as f64) * self.cache_creation_input_token_cost.unwrap_or(0.0)
            + (cache_read as f64) * self.cache_read_input_token_cost.unwrap_or(0.0)
    }
}

/// Cached pricing table. Exact model name match only.
pub struct PricingTable {
    prices: HashMap<String, ModelPricing>,
}

impl PricingTable {
    pub fn new(prices: HashMap<String, ModelPricing>) -> Self {
        PricingTable { prices }
    }

    pub fn is_empty(&self) -> bool {
        self.prices.is_empty()
    }

    pub fn get(&self, model: &str) -> Option<&ModelPricing> {
        self.prices.get(model)
    }

    /// Calculate cost for given model + token counts.
    pub fn cost(&self, model: &str, input: u64, output: u64, cache_create: u64, cache_read: u64) -> Option<f64> {
        self.get(model).map(|p| p.cost(input, output, cache_create, cache_read))
    }
}

/// Parse LiteLLM JSON and extract all model prices.
fn parse_litellm_json(json_str: &str) -> HashMap<String, ModelPricing> {
    let raw: HashMap<String, LiteLLMEntry> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };

    let mut prices = HashMap::with_capacity(raw.len());

    for (key, entry) in &raw {
        let (input_cost, output_cost) = match (entry.input_cost_per_token, entry.output_cost_per_token) {
            (Some(i), Some(o)) if i > 0.0 || o > 0.0 => (i, o),
            _ => continue,
        };

        let pricing = ModelPricing {
            input_cost_per_token: input_cost,
            output_cost_per_token: output_cost,
            cache_creation_input_token_cost: entry.cache_creation_input_token_cost,
            cache_read_input_token_cost: entry.cache_read_input_token_cost,
        };

        let provider = match entry.litellm_provider.as_deref() {
            Some(p) => p,
            None => continue,
        };
        if !SUPPORTED_LITELLM_PROVIDERS.contains(&provider) {
            continue;
        }

        let model_name = if key.contains('/') {
            key.rsplit('/').next().unwrap_or(key)
        } else {
            key.as_str()
        };
        if model_name.is_empty() {
            continue;
        }

        prices.entry(model_name.to_string()).or_insert(pricing);
    }

    prices
}

#[derive(Deserialize)]
struct LiteLLMEntry {
    #[serde(default)]
    litellm_provider: Option<String>,
    #[serde(default)]
    input_cost_per_token: Option<f64>,
    #[serde(default)]
    output_cost_per_token: Option<f64>,
    #[serde(default)]
    cache_creation_input_token_cost: Option<f64>,
    #[serde(default)]
    cache_read_input_token_cost: Option<f64>,
}

const SUPPORTED_LITELLM_PROVIDERS: &[&str] = &["anthropic", "openai", "gemini"];

const PRICING_CACHE_VERSION: u32 = 5;

#[derive(Serialize, Deserialize)]
struct PricingCache {
    etag: Option<String>,
    #[serde(default)]
    version: u32,
    prices: HashMap<String, ModelPricing>,
}

/// Default pricing cache file path.
pub fn default_cache_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("toki-sync").join("pricing.json")
}

fn load_cache(path: &Path) -> Option<PricingCache> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_cache(path: &Path, cache: &PricingCache) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = path.with_extension("tmp");
    if let Ok(data) = serde_json::to_string(cache) {
        if std::fs::write(&tmp, &data).is_ok() {
            std::fs::rename(&tmp, path).ok();
        }
    }
}

/// Fetch pricing with ETag-based caching.
pub fn fetch_pricing(cache_path: &Path) -> PricingTable {
    let cached = load_cache(cache_path);

    let cache_valid = cached.as_ref().map_or(false, |c| c.version == PRICING_CACHE_VERSION);
    let cached_etag = if cache_valid {
        cached.as_ref().and_then(|c| c.etag.clone())
    } else {
        None
    };

    let mut req = ureq::get(LITELLM_URL);
    if let Some(ref etag) = cached_etag {
        req = req.set("If-None-Match", etag);
    }

    match req.call() {
        Ok(resp) => {
            if resp.status() == 304 {
                return cached.map(|c| PricingTable::new(c.prices))
                    .unwrap_or_else(|| PricingTable::new(HashMap::new()));
            }

            let new_etag = resp.header("ETag").map(|s| s.to_string());
            let body = match resp.into_string() {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("Pricing: failed to read response body: {}", e);
                    return fallback(cached);
                }
            };

            let prices = parse_litellm_json(&body);
            if prices.is_empty() {
                tracing::warn!("Pricing: no supported models found in response");
                return fallback(cached);
            }

            save_cache(cache_path, &PricingCache { etag: new_etag, version: PRICING_CACHE_VERSION, prices: prices.clone() });
            PricingTable::new(prices)
        }
        Err(ureq::Error::Status(304, _)) => fallback(cached),
        Err(e) => {
            tracing::warn!("Pricing: network error ({})", e);
            fallback(cached)
        }
    }
}

/// Load pricing from cache file only (no network).
pub fn load_cached_pricing(cache_path: &Path) -> PricingTable {
    match load_cache(cache_path) {
        Some(cache) => PricingTable::new(cache.prices),
        None => PricingTable::new(HashMap::new()),
    }
}

fn fallback(cached: Option<PricingCache>) -> PricingTable {
    match cached {
        Some(cache) => PricingTable::new(cache.prices),
        None => PricingTable::new(HashMap::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_litellm_json() {
        let json = r#"{
            "claude-sonnet-4-20250514": {
                "litellm_provider": "anthropic",
                "input_cost_per_token": 0.000003,
                "output_cost_per_token": 0.000015,
                "cache_creation_input_token_cost": 0.00000375,
                "cache_read_input_token_cost": 0.0000003
            },
            "gpt-4": {
                "litellm_provider": "openai",
                "input_cost_per_token": 0.00003,
                "output_cost_per_token": 0.00006
            },
            "anthropic/claude-opus-4-20250514": {
                "litellm_provider": "anthropic",
                "input_cost_per_token": 0.000015,
                "output_cost_per_token": 0.000075,
                "cache_creation_input_token_cost": 0.00001875,
                "cache_read_input_token_cost": 0.0000015
            },
            "deepseek-v3": {
                "litellm_provider": "deepseek",
                "input_cost_per_token": 0.000001,
                "output_cost_per_token": 0.000002
            }
        }"#;

        let prices = parse_litellm_json(json);
        assert!(prices.contains_key("claude-sonnet-4-20250514"));
        assert!(prices.contains_key("gpt-4"));
        assert!(prices.contains_key("claude-opus-4-20250514"));
        assert!(!prices.contains_key("deepseek-v3")); // unsupported provider
    }

    #[test]
    fn test_cost_calculation() {
        let mut prices = HashMap::new();
        prices.insert("claude-sonnet-4-20250514".to_string(), ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cache_creation_input_token_cost: Some(0.00000375),
            cache_read_input_token_cost: Some(0.0000003),
        });
        let table = PricingTable::new(prices);

        let cost = table.cost("claude-sonnet-4-20250514", 1000, 500, 200, 3000).unwrap();
        assert!((cost - 0.01215).abs() < 1e-10);
    }

    #[test]
    fn test_no_match() {
        let table = PricingTable::new(HashMap::new());
        assert!(table.cost("unknown-model", 1000, 500, 0, 0).is_none());
    }
}
