use crate::cache::parse_cache_control;
use crate::cache::now_unix;
use crate::headers::strip_hop_by_hop;
use hyper::header::HeaderMap;

pub fn headers_for_storage(headers: &HeaderMap) -> Vec<(String, String)> {
    let mut sanitized = headers.clone();
    strip_hop_by_hop(&mut sanitized);
    sanitized
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str().to_string();
            let value = value.to_str().ok()?.to_string();
            Some((name, value))
        })
        .collect()
}

pub fn compute_expiry(headers: &HeaderMap, ttl_seconds: u64) -> Option<i64> {
    let now = now_unix();
    let mut expires_at = now.saturating_add(ttl_seconds as i64);

    if let Some(value) = headers.get(hyper::header::CACHE_CONTROL) {
        if let Ok(control) = value.to_str() {
            let parsed = parse_cache_control(control);
            if parsed.no_store || parsed.no_cache {
                return None;
            }
            if let Some(max_age) = parsed.max_age {
                expires_at = expires_at.min(now.saturating_add(max_age as i64));
            }
        }
    }

    if let Some(value) = headers.get(hyper::header::EXPIRES) {
        if let Ok(value) = value.to_str() {
            if let Ok(datetime) = httpdate::parse_http_date(value) {
                let expires = datetime
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                expires_at = expires_at.min(expires);
            }
        }
    }

    Some(expires_at)
}

pub fn parse_content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}
