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
        let config: Self = toml::from_str(&contents).map_err(|e| ProxyError::ConfigParse {
            path: path.display().to_string(),
            source: e,
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ProxyError> {
        if self.logging.redact_headers.iter().any(|name| name.trim().is_empty()) {
            return Err(ProxyError::ConfigValidation(
                "logging.redact_headers cannot contain empty header names".to_string(),
            ));
        }

        if self.limits.max_header_bytes == 0 {
            return Err(ProxyError::ConfigValidation(
                "limits.max_header_bytes must be greater than 0".to_string(),
            ));
        }

        if self.limits.max_connections == 0 {
            return Err(ProxyError::ConfigValidation(
                "limits.max_connections must be greater than 0".to_string(),
            ));
        }

        if self.limits.connect_timeout_ms == 0 {
            return Err(ProxyError::ConfigValidation(
                "limits.connect_timeout_ms must be greater than 0".to_string(),
            ));
        }

        if self.limits.idle_timeout_ms == 0 {
            return Err(ProxyError::ConfigValidation(
                "limits.idle_timeout_ms must be greater than 0".to_string(),
            ));
        }

        if self.caching.enabled && self.caching.ttl_seconds == 0 {
            return Err(ProxyError::ConfigValidation(
                "caching.ttl_seconds must be greater than 0 when caching is enabled".to_string(),
            ));
        }

        Ok(())
    }

    pub fn startup_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        if self.env.mode == EnvMode::Dev && self.caching.enabled {
            warnings.push(
                "dev mode clears .cache on startup, so cached objects are not persistent across restarts"
                    .to_string(),
            );
        }

        if self.caching.enabled && self.caching.cache_channel_capacity_chunks == 0 {
            warnings.push(
                "caching is enabled but cache_channel_capacity_chunks is 0, so cache writes will be skipped"
                    .to_string(),
            );
        }

        if self.caching.enabled && self.caching.max_entries == 0 {
            warnings.push(
                "caching is enabled but max_entries is 0, so no SQLite LRU eviction limit is enforced"
                    .to_string(),
            );
        }

        let denied_ports: std::collections::HashSet<u16> =
            self.policy.denied_ports.iter().copied().collect();
        let overlapping_ports: Vec<u16> = self
            .policy
            .allowed_ports
            .iter()
            .copied()
            .filter(|port| denied_ports.contains(port))
            .collect();
        if !overlapping_ports.is_empty() {
            warnings.push(format!(
                "policy.allowed_ports and policy.denied_ports overlap; denied ports win: {:?}",
                overlapping_ports
            ));
        }

        warnings
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
    pub cache_channel_capacity_chunks: usize,
    pub cache_max_object_size_bytes: u64,
    pub cache_require_content_length: bool,
    pub cache_writer_delay_ms: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_seconds: 300,
            max_entries: 1024,
            cache_channel_capacity_chunks: 64,
            cache_max_object_size_bytes: 1_073_741_824,
            cache_require_content_length: true,
            cache_writer_delay_ms: 0,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_rejects_zero_connection_limit() {
        let mut config = Config::default();
        config.limits.max_connections = 0;

        let err = config.validate().unwrap_err();
        assert!(err
            .to_string()
            .contains("limits.max_connections must be greater than 0"));
    }

    #[test]
    fn validation_rejects_empty_redacted_header_names() {
        let mut config = Config::default();
        config.logging.redact_headers.push("   ".to_string());

        let err = config.validate().unwrap_err();
        assert!(err
            .to_string()
            .contains("logging.redact_headers cannot contain empty header names"));
    }

    #[test]
    fn startup_warnings_report_dev_cache_reset_and_port_overlap() {
        let mut config = Config::default();
        config.env.mode = EnvMode::Dev;
        config.caching.enabled = true;
        config.policy.allowed_ports = vec![80, 443];
        config.policy.denied_ports = vec![443];

        let warnings = config.startup_warnings();
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("dev mode clears .cache on startup")));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("denied ports win")));
    }

    #[test]
    fn load_validates_parsed_config() {
        let toml = r#"
            [limits]
            max_connections = 0
        "#;

        let config: Config = toml::from_str(toml).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err
            .to_string()
            .contains("limits.max_connections must be greater than 0"));
    }
}
