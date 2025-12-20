use crate::errors::ProxyError;
use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub env: EnvConfig,
    pub logging: LoggingConfig,
    pub time: TimeConfig,
    pub caching: CacheConfig,
    pub limits: LimitsConfig,
    pub policy: PolicyConfig,
    pub listen: ListenConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            env: EnvConfig::default(),
            logging: LoggingConfig::default(),
            time: TimeConfig::default(),
            caching: CacheConfig::default(),
            limits: LimitsConfig::default(),
            policy: PolicyConfig::default(),
            listen: ListenConfig::default(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ProxyError> {
        let contents = fs::read_to_string(path).map_err(|e| ProxyError::ConfigRead {
            path: path.display().to_string(),
            source: e,
        })?;
        toml::from_str(&contents).map_err(|e| ProxyError::ConfigParse {
            path: path.display().to_string(),
            source: e,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct EnvConfig {
    pub mode: EnvMode,
}

impl Default for EnvConfig {
    fn default() -> Self {
        Self {
            mode: EnvMode::Dev,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EnvMode {
    Dev,
    Prod,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: String,
    pub format: LoggingFormat,
    pub request_id: bool,
    pub redact_headers: Vec<String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: LoggingFormat::Pretty,
            request_id: true,
            redact_headers: vec![
                "authorization".to_string(),
                "proxy-authorization".to_string(),
                "cookie".to_string(),
                "set-cookie".to_string(),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LoggingFormat {
    Pretty,
    Json,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TimeConfig {
    pub timezone: String,
}

impl Default for TimeConfig {
    fn default() -> Self {
        Self {
            timezone: "UTC".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    pub enabled: bool,
    pub ttl_seconds: u64,
    pub max_entries: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_seconds: 300,
            max_entries: 1024,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    pub max_header_bytes: usize,
    pub max_connections: usize,
    pub per_ip_rps: u32,
    pub connect_timeout_ms: u64,
    pub idle_timeout_ms: u64,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_header_bytes: 32_768,
            max_connections: 1024,
            per_ip_rps: 50,
            connect_timeout_ms: 5_000,
            idle_timeout_ms: 60_000,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PolicyConfig {
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
    pub allowed_ports: Vec<u16>,
    pub denied_ports: Vec<u16>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            allowed_domains: vec!["*".to_string()],
            denied_domains: Vec::new(),
            allowed_ports: vec![80, 443],
            denied_ports: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ListenConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ListenConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 3128,
        }
    }
}
