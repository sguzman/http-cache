use crate::config::CacheConfig;
use std::sync::Arc;

pub trait Cache: Send + Sync {
    fn name(&self) -> &'static str;
    fn is_enabled(&self) -> bool;
}

#[derive(Debug)]
pub struct NoopCache {
    enabled: bool,
}

impl NoopCache {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

impl Cache for NoopCache {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

pub fn build_cache(config: &CacheConfig) -> Arc<dyn Cache> {
    if config.enabled {
        tracing::warn!(
            "cache enabled in config, but only NoopCache is implemented"
        );
    }
    Arc::new(NoopCache::new(config.enabled))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_wiring_defaults_to_noop() {
        let config = CacheConfig {
            enabled: false,
            ttl_seconds: 10,
            max_entries: 10,
        };
        let cache = build_cache(&config);
        assert_eq!(cache.name(), "noop");
        assert!(!cache.is_enabled());
    }

    #[test]
    fn cache_wiring_flags_enabled_even_with_noop() {
        let config = CacheConfig {
            enabled: true,
            ttl_seconds: 10,
            max_entries: 10,
        };
        let cache = build_cache(&config);
        assert_eq!(cache.name(), "noop");
        assert!(cache.is_enabled());
    }
}
