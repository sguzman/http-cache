use hyper::header::{HeaderMap, HeaderName, CONNECTION, PROXY_AUTHORIZATION};

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "proxy-connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

pub fn strip_hop_by_hop(headers: &mut HeaderMap) {
    if let Some(connection_value) = headers.get(CONNECTION).cloned() {
        if let Ok(value) = connection_value.to_str() {
            for name in value.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                if let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) {
                    headers.remove(header_name);
                }
            }
        }
    }

    for name in HOP_BY_HOP {
        if let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) {
            headers.remove(header_name);
        }
    }

    headers.remove(PROXY_AUTHORIZATION);
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::{HeaderValue, HOST};

    #[test]
    fn strip_hop_by_hop_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("Connection", HeaderValue::from_static("keep-alive, upgrade"));
        headers.insert("Keep-Alive", HeaderValue::from_static("timeout=5"));
        headers.insert("Upgrade", HeaderValue::from_static("websocket"));
        headers.insert("Proxy-Authorization", HeaderValue::from_static("token"));
        headers.insert(HOST, HeaderValue::from_static("example.com"));

        strip_hop_by_hop(&mut headers);

        assert!(headers.get("connection").is_none());
        assert!(headers.get("keep-alive").is_none());
        assert!(headers.get("upgrade").is_none());
        assert!(headers.get("proxy-authorization").is_none());
        assert!(headers.get(HOST).is_some());
    }
}
