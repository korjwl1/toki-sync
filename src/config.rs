use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

fn expand_env(s: &str) -> String {
    let mut result = s.to_owned();
    // Replace ${VAR} with env var value (fallback: empty string)
    while let Some(start) = result.find("${") {
        let end = match result[start..].find('}') {
            Some(i) => start + i,
            None => break,
        };
        let var_name = &result[start + 2..end];
        let value = std::env::var(var_name).unwrap_or_default();
        result.replace_range(start..=end, &value);
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
}

fn default_http_port() -> u16 { 9091 }
fn default_tcp_port() -> u16 { 9090 }
fn default_bind() -> String { "0.0.0.0".to_string() }

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            http_port: default_http_port(),
            tcp_port: default_tcp_port(),
            bind: default_bind(),
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
}

fn default_access_ttl() -> u64 { 3600 }         // 1h
fn default_refresh_ttl() -> u64 { 86400 * 30 }  // 30d
fn default_brute_max_attempts() -> u32 { 5 }
fn default_brute_window() -> u64 { 300 }         // 5m
fn default_brute_lockout() -> u64 { 900 }        // 15m

#[derive(Debug, Deserialize, Clone, Default)]
pub struct BackendConfig {
    #[serde(default = "default_vm_url")]
    #[allow(dead_code)]
    pub vm_url: String,
}

fn default_vm_url() -> String { "http://victoriametrics:8428".to_string() }

#[derive(Debug, Deserialize, Clone)]
pub struct StorageConfig {
    #[serde(default = "default_db_path")]
    pub db_path: String,
}

fn default_db_path() -> String { "./data/toki_sync.db".to_string() }

impl Default for StorageConfig {
    fn default() -> Self {
        Self { db_path: default_db_path() }
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
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    pub auth: AuthConfig,
    #[serde(default)]
    #[allow(dead_code)]
    pub backend: BackendConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub log: LogConfig,
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
                },
                backend: BackendConfig::default(),
                storage: StorageConfig::default(),
                log: LogConfig::default(),
            })
        }
    }
}
