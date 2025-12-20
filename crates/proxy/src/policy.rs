use crate::config::PolicyConfig;
use crate::errors::ProxyError;

#[derive(Debug, Clone)]
pub struct PolicyEngine {
    allowed_domains: Vec<String>,
    denied_domains: Vec<String>,
    allowed_ports: Vec<u16>,
    denied_ports: Vec<u16>,
}

impl PolicyEngine {
    pub fn new(config: &PolicyConfig) -> Self {
        Self {
            allowed_domains: config
                .allowed_domains
                .iter()
                .map(|s| s.to_ascii_lowercase())
                .collect(),
            denied_domains: config
                .denied_domains
                .iter()
                .map(|s| s.to_ascii_lowercase())
                .collect(),
            allowed_ports: config.allowed_ports.clone(),
            denied_ports: config.denied_ports.clone(),
        }
    }

    pub fn check(&self, host: &str, port: u16) -> Result<(), ProxyError> {
        let host = host.to_ascii_lowercase();
        if self
            .denied_domains
            .iter()
            .any(|pattern| domain_matches(pattern, &host))
        {
            return Err(ProxyError::PolicyDenied(format!(
                "domain denied: {host}"
            )));
        }

        if !self.allowed_domains.is_empty()
            && !self
                .allowed_domains
                .iter()
                .any(|pattern| domain_matches(pattern, &host))
        {
            return Err(ProxyError::PolicyDenied(format!(
                "domain not allowed: {host}"
            )));
        }

        if self.denied_ports.contains(&port) {
            return Err(ProxyError::PolicyDenied(format!(
                "port denied: {port}"
            )));
        }

        if !self.allowed_ports.is_empty() && !self.allowed_ports.contains(&port) {
            return Err(ProxyError::PolicyDenied(format!(
                "port not allowed: {port}"
            )));
        }

        Ok(())
    }
}

pub fn domain_matches(pattern: &str, domain: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    if let Some(suffix) = pattern.strip_prefix("*.") {
        let suffix = format!(".{suffix}");
        return domain.ends_with(&suffix) && domain.len() > suffix.len();
    }

    pattern == domain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_matching_supports_wildcards() {
        assert!(domain_matches("*", "example.com"));
        assert!(domain_matches("example.com", "example.com"));
        assert!(!domain_matches("example.com", "api.example.com"));
        assert!(domain_matches("*.example.com", "api.example.com"));
        assert!(!domain_matches("*.example.com", "example.com"));
    }
}
