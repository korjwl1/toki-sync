use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

fn expand_env(s: &str) -> String {
    let mut result = s.to_owned();
    let mut search_from = 0;
    // Replace ${VAR} with env var value (fallback: empty string).
    // Advance past each replacement to avoid infinite loops when
    // an env var value itself contains "${".
    while let Some(rel) = result[search_from..].find("${") {
        let start = search_from + rel;
        let end = match result[start..].find('}') {
            Some(i) => start + i,
            None => break,
        };
        let var_name = &result[start + 2..end];
        let value = std::env::var(var_name).unwrap_or_default();
        let value_len = value.len();
        result.replace_range(start..=end, &value);
        search_from = start + value_len;
    }
    result
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default = "default_tcp_port")]
    pub tcp_port: u16,
    #[serde(default = "default_bind")]
    pub bind: String,
    /// External URL used for JWT `iss` claim and OIDC redirect_uri derivation.
    /// Example: "https://sync.example.com"
    #[serde(default)]
    pub external_url: String,
    /// Maximum number of concurrent VM write operations across all TCP connections.
    /// Limits bulk-batch thundering-herd pressure on VictoriaMetrics.
    #[serde(default = "default_max_concurrent_writes")]
    pub max_concurrent_writes: usize,
    /// Trust X-Forwarded-For header (set to true when behind a reverse proxy).
    /// When false (default), the header is ignored and the direct connection IP is used.
    #[serde(default)]
    pub trust_proxy: bool,
}

fn default_http_port() -> u16 { 9091 }
fn default_tcp_port() -> u16 { 9090 }
fn default_bind() -> String { "0.0.0.0".to_string() }
fn default_max_concurrent_writes() -> usize { 10 }

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            http_port: default_http_port(),
            tcp_port: default_tcp_port(),
            bind: default_bind(),
            external_url: String::new(),
            max_concurrent_writes: default_max_concurrent_writes(),
            trust_proxy: false,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct AuthConfig {
    /// Secret key for JWT signing (HS256). Use ${JWT_SECRET} for env var.
    pub jwt_secret: String,
    #[serde(default = "default_access_ttl")]
    pub access_token_ttl_secs: u64,
    #[serde(default = "default_refresh_ttl")]
    pub refresh_token_ttl_secs: u64,
    #[serde(default = "default_brute_max_attempts")]
    pub brute_force_max_attempts: u32,
    #[serde(default = "default_brute_window")]
    pub brute_force_window_secs: u64,
    #[serde(default = "default_brute_lockout")]
    pub brute_force_lockout_secs: u64,
    /// Registration mode: "open" | "approval" | "closed". Default: "closed".
    #[serde(default = "default_registration_mode")]
    pub registration_mode: String,
    /// Legacy compat: if old `allow_registration` is present, map true -> "open", false -> "closed".
    #[serde(default)]
    pub allow_registration: Option<bool>,
    /// OIDC configuration (Phase 3). Empty = disabled.
    #[serde(default)]
    pub oidc_issuer: String,
    #[serde(default)]
    pub oidc_client_id: String,
    #[serde(default)]
    pub oidc_client_secret: String,
    #[serde(default)]
    pub oidc_redirect_uri: String,
}

fn default_registration_mode() -> String { "closed".to_string() }

impl AuthConfig {
    /// Resolve the effective registration mode, supporting legacy `allow_registration` field.
    pub fn effective_registration_mode(&self) -> &str {
        if !self.registration_mode.is_empty() && self.registration_mode != "closed" {
            return &self.registration_mode;
        }
        // Legacy fallback
        match self.allow_registration {
            Some(true) => "open",
            _ => &self.registration_mode,
        }
    }
}

fn default_access_ttl() -> u64 { 3600 }         // 1h
fn default_refresh_ttl() -> u64 { 86400 * 90 }  // 90d
fn default_brute_max_attempts() -> u32 { 5 }
fn default_brute_window() -> u64 { 300 }         // 5m
fn default_brute_lockout() -> u64 { 900 }        // 15m

#[derive(Debug, Deserialize, Clone, Default)]
pub struct BackendConfig {
    #[serde(default = "default_vm_url")]
    pub vm_url: String,
}

pub fn default_vm_url() -> String { "http://victoriametrics:8428".to_string() }

#[derive(Debug, Deserialize, Clone)]
pub struct StorageConfig {
    #[serde(default = "default_db_backend")]
    pub backend: String,
    /// New canonical field for SQLite path.
    #[serde(default = "default_db_path")]
    pub sqlite_path: String,
    /// Legacy alias — if present in config, used as sqlite_path (backward compat).
    #[serde(default)]
    pub db_path: String,
    #[serde(default)]
    pub postgres_url: String,
}

fn default_db_backend() -> String { "sqlite".to_string() }
fn default_db_path() -> String { "./data/toki_sync.db".to_string() }

impl StorageConfig {
    /// Resolve the effective sqlite_path: prefer explicit `sqlite_path`,
    /// fall back to legacy `db_path` if set, otherwise use default.
    pub fn effective_sqlite_path(&self) -> &str {
        if self.sqlite_path != default_db_path() {
            // sqlite_path was explicitly set
            &self.sqlite_path
        } else if !self.db_path.is_empty() {
            // Legacy db_path present
            &self.db_path
        } else {
            &self.sqlite_path
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: default_db_backend(),
            sqlite_path: default_db_path(),
            db_path: String::new(),
            postgres_url: String::new(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct LogConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default)]
    pub json: bool,
}

fn default_log_level() -> String { "info".to_string() }

impl Default for LogConfig {
    fn default() -> Self {
        Self { level: default_log_level(), json: false }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct FeaturesConfig {
    #[serde(default = "default_max_query_scope")]
    pub max_query_scope: String,  // "self" | "team" | "all"
}

fn default_max_query_scope() -> String { "self".to_string() }

impl Default for FeaturesConfig {
    fn default() -> Self {
        Self { max_query_scope: default_max_query_scope() }
    }
}

/// Event storage backend config.
/// "fjall" (default, standalone) or "clickhouse" (external).
#[derive(Debug, Deserialize, Clone)]
pub struct EventsConfig {
    #[serde(default = "default_events_backend")]
    pub backend: String,
    #[serde(default = "default_events_fjall_path")]
    pub fjall_path: String,
    #[serde(default)]
    pub clickhouse_url: String,
}

fn default_events_backend() -> String { "fjall".to_string() }
fn default_events_fjall_path() -> String { "./data/events.fjall".to_string() }

impl Default for EventsConfig {
    fn default() -> Self {
        EventsConfig {
            backend: default_events_backend(),
            fjall_path: default_events_fjall_path(),
            clickhouse_url: String::new(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    pub auth: AuthConfig,
    #[serde(default)]
    pub backend: BackendConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub events: EventsConfig,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub features: FeaturesConfig,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        let expanded = expand_env(&raw);
        let config: Config = toml::from_str(&expanded)
            .with_context(|| format!("failed to parse config: {}", path.display()))?;
        Ok(config)
    }

    pub fn load_or_default(path: &Path) -> Result<Self> {
        if path.exists() {
            Self::load(path)
        } else {
            let jwt_secret = std::env::var("JWT_SECRET")
                .unwrap_or_else(|_| "change-me-in-production".to_string());
            Ok(Config {
                server: ServerConfig::default(),
                auth: AuthConfig {
                    jwt_secret,
                    access_token_ttl_secs: default_access_ttl(),
                    refresh_token_ttl_secs: default_refresh_ttl(),
                    brute_force_max_attempts: default_brute_max_attempts(),
                    brute_force_window_secs: default_brute_window(),
                    brute_force_lockout_secs: default_brute_lockout(),
                    registration_mode: default_registration_mode(),
                    allow_registration: None,
                    oidc_issuer: String::new(),
                    oidc_client_id: String::new(),
                    oidc_client_secret: String::new(),
                    oidc_redirect_uri: String::new(),
                },
                backend: BackendConfig::default(),
                storage: StorageConfig::default(),
                events: EventsConfig::default(),
                log: LogConfig::default(),
                features: FeaturesConfig::default(),
            })
        }
    }
}
