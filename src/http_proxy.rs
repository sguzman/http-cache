use crate::cache::{
    cache_key, cache_objects_dir, now_unix, parse_cache_control, Cache, CacheStoreRequest,
};
use crate::errors::{ProxyBody, ProxyError};
use crate::headers::strip_hop_by_hop;
use crate::policy::PolicyEngine;
use bytes::Bytes;
use http_body::Frame;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Incoming;
use hyper::header::{HeaderMap, HeaderName, HeaderValue, HOST};
use hyper::{Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tracing::Instrument;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct UpstreamTarget {
    pub host: String,
    pub port: u16,
    pub authority: String,
}

pub fn rewrite_absolute_form<B>(req: &mut Request<B>) -> Result<UpstreamTarget, ProxyError> {
    let uri = req.uri().clone();
    let scheme = uri
        .scheme()
        .ok_or_else(|| ProxyError::BadRequest("missing scheme".to_string()))?;
    if scheme != "http" {
        return Err(ProxyError::BadRequest(format!(
            "unsupported scheme: {scheme}"
        )));
    }

    let authority = uri
        .authority()
        .ok_or_else(|| ProxyError::BadRequest("missing authority".to_string()))?
        .clone();

    let host = authority.host().to_string();
    let port = authority.port_u16().unwrap_or(80);
    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let new_uri = Uri::builder()
        .path_and_query(path_and_query)
        .build()
        .map_err(|_| ProxyError::BadRequest("invalid path".to_string()))?;
    *req.uri_mut() = new_uri;

    let host_header = authority.as_str();
    req.headers_mut().insert(
        HOST,
        HeaderValue::from_str(host_header)
            .map_err(|_| ProxyError::BadRequest("invalid host".to_string()))?,
    );

    Ok(UpstreamTarget {
        host,
        port,
        authority: authority.to_string(),
    })
}

pub async fn handle_http(
    mut req: Request<Incoming>,
    policy: &PolicyEngine,
    cache: Arc<dyn Cache>,
    connect_timeout: Duration,
    cache_ttl_seconds: u64,
) -> Result<Response<ProxyBody>, ProxyError> {
    let method = req.method().clone();
    let original_uri = req.uri().to_string();

    let target = rewrite_absolute_form(&mut req)?;
    policy.check(&target.host, target.port)?;

    if cache.is_enabled() && (method == hyper::Method::GET || method == hyper::Method::HEAD) {
        if let Some(entry) = cache.get(method.as_str(), &original_uri).await? {
            tracing::info!(key = %entry.key, "cache hit");
            if let Ok(response) = cached_response(&method, &entry).await {
                return Ok(response);
            }
            tracing::warn!(key = %entry.key, "cache entry invalid, bypassing");
        } else {
            tracing::info!("cache miss");
        }
    }

    strip_hop_by_hop(req.headers_mut());

    let addr = format!("{}:{}", target.host, target.port);
    let stream = timeout(connect_timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| ProxyError::Timeout)??;

    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(
        async move {
            if let Err(err) = conn.await {
                tracing::warn!(error = %err, "upstream connection error");
            }
        }
        .instrument(tracing::debug_span!("upstream_conn")),
    );

    let response = sender.send_request(req).await?;
    let (parts, body) = response.into_parts();
    let status = parts.status;

    if cache.is_enabled() && (method == hyper::Method::GET || method == hyper::Method::HEAD) {
        if let Some(expires_at) = compute_expiry(&parts.headers, cache_ttl_seconds) {
            if expires_at > now_unix() && status.is_success() {
                let stored_headers = headers_for_storage(&parts.headers);
                if method == hyper::Method::HEAD {
                    let entry = CacheStoreRequest {
                        key: cache_key(method.as_str(), &original_uri),
                        method: method.as_str().to_string(),
                        url: original_uri.clone(),
                        status: status.as_u16(),
                        headers: stored_headers,
                        body_path: None,
                        body_size: 0,
                        created_at: now_unix(),
                        expires_at,
                    };
                    let cache_clone = cache.clone();
                    tokio::spawn(async move {
                        if let Err(err) = cache_clone.store(entry).await {
                            tracing::warn!(error = %err, "cache store failed");
                        }
                    });
                    let response = Response::from_parts(
                        parts,
                        body.map_err(ProxyError::from).boxed(),
                    );
                    return Ok(response);
                }

                let (cached_body, writer) = cache_body(
                    body,
                    cache.clone(),
                    method.as_str().to_string(),
                    original_uri.clone(),
                    status,
                    stored_headers,
                    expires_at,
                    cache.cache_dir().to_path_buf(),
                )
                .await?;

                tokio::spawn(writer);
                return Ok(Response::from_parts(parts, cached_body));
            }
        } else {
            tracing::debug!("response not cacheable due to headers");
        }
    }

    Ok(Response::from_parts(
        parts,
        body.map_err(ProxyError::from).boxed(),
    ))
}

fn headers_for_storage(headers: &HeaderMap) -> Vec<(String, String)> {
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

fn compute_expiry(headers: &HeaderMap, ttl_seconds: u64) -> Option<i64> {
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

async fn cached_response(
    method: &hyper::Method,
    entry: &crate::cache::CacheEntry,
) -> Result<Response<ProxyBody>, ProxyError> {
    let mut headers = HeaderMap::new();
    for (name, value) in &entry.headers {
        let name = HeaderName::from_bytes(name.as_bytes()).ok();
        let value = HeaderValue::from_str(value).ok();
        if let (Some(name), Some(value)) = (name, value) {
            headers.insert(name, value);
        }
    }

    strip_hop_by_hop(&mut headers);
    headers.insert(
        hyper::header::CONTENT_LENGTH,
        HeaderValue::from_str(&entry.body_size.to_string())
            .map_err(|_| ProxyError::Internal)?,
    );

    let status = StatusCode::from_u16(entry.status).unwrap_or(StatusCode::OK);

    if method == hyper::Method::HEAD {
        let body = Full::new(Bytes::new())
            .map_err(|_| ProxyError::Internal)
            .boxed();
        let mut response = Response::new(body);
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        return Ok(response);
    }

    let path = entry
        .body_path
        .as_ref()
        .ok_or_else(|| ProxyError::Internal)?;
    let file = tokio::fs::File::open(path).await?;
    let stream = ReaderStream::new(file).map(|result| result.map(Frame::data));
    let body = StreamBody::new(stream)
        .map_err(ProxyError::from)
        .boxed();

    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

async fn cache_body(
    body: Incoming,
    cache: Arc<dyn Cache>,
    method: String,
    url: String,
    status: StatusCode,
    headers: Vec<(String, String)>,
    expires_at: i64,
    cache_dir: PathBuf,
) -> Result<
    (
        ProxyBody,
        impl std::future::Future<Output = ()> + Send + 'static,
    ),
    ProxyError,
> {
    let key = cache_key(&method, &url);
    let objects_dir = cache_objects_dir(&cache_dir);
    let hash = hash_key(&key);
    let final_path = objects_dir.join(&hash);
    let temp_path = objects_dir.join(format!("{}.tmp-{}", hash, Uuid::new_v4()));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    let writer_cache = cache.clone();
    let created_at = now_unix();

    let writer = async move {
        let mut size: u64 = 0;
        let mut file = match tokio::fs::File::create(&temp_path).await {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!(error = %err, "cache file create failed");
                return;
            }
        };

        while let Some(chunk) = rx.recv().await {
            size += chunk.len() as u64;
            if let Err(err) = tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await {
                tracing::warn!(error = %err, "cache write failed");
                let _ = tokio::fs::remove_file(&temp_path).await;
                return;
            }
        }

        if let Err(err) = tokio::fs::rename(&temp_path, &final_path).await {
            tracing::warn!(error = %err, "cache file rename failed");
            let _ = tokio::fs::remove_file(&temp_path).await;
            return;
        }

        let entry = CacheStoreRequest {
            key,
            method,
            url,
            status: status.as_u16(),
            headers,
            body_path: Some(final_path.to_string_lossy().to_string()),
            body_size: size,
            created_at,
            expires_at,
        };

        if let Err(err) = writer_cache.store(entry).await {
            tracing::warn!(error = %err, "cache store failed");
        }
    };

    let mapped_body = body.map_frame(move |frame| {
        if let Some(data) = frame.data_ref() {
            let _ = tx.send(data.clone());
        }
        frame
    });

    let proxy_body = mapped_body.map_err(ProxyError::from).boxed();
    Ok((proxy_body, writer))
}

fn hash_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::Empty;

    #[test]
    fn rewrites_absolute_form() {
        let uri = "http://example.com/a?b=c".parse::<Uri>().unwrap();
        let mut req = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Empty::<Bytes>::new())
            .unwrap();
        let target = rewrite_absolute_form(&mut req).unwrap();

        assert_eq!(target.host, "example.com");
        assert_eq!(target.port, 80);
        assert_eq!(req.uri().to_string(), "/a?b=c");
        assert_eq!(req.headers().get(HOST).unwrap(), "example.com");
    }
}
