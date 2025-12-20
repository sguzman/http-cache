use crate::cache::{cache_key, now_unix, Cache, CacheStoreRequest};
use crate::config::CacheConfig;
use crate::errors::{ProxyBody, ProxyError};
use crate::headers::strip_hop_by_hop;
use crate::policy::PolicyEngine;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tracing::Instrument;

mod cache_pipeline;
mod headers;
mod upstream;

pub use upstream::{rewrite_absolute_form, UpstreamTarget};

pub async fn handle_http(
    mut req: Request<Incoming>,
    policy: &PolicyEngine,
    cache: Arc<dyn Cache>,
    connect_timeout: Duration,
    cache_config: CacheConfig,
) -> Result<Response<ProxyBody>, ProxyError> {
    let method = req.method().clone();
    let original_uri = req.uri().to_string();
    let range_requested = req.headers().contains_key(hyper::header::RANGE);

    let target = rewrite_absolute_form(&mut req)?;
    policy.check(&target.host, target.port)?;

    if cache.is_enabled()
        && (method == hyper::Method::GET || method == hyper::Method::HEAD)
        && !range_requested
    {
        if let Some(entry) = cache.get(method.as_str(), &original_uri).await? {
            tracing::info!(key = %entry.key, "cache hit");
            if let Ok(response) = cache_pipeline::cached_response(&method, &entry).await {
                return Ok(response);
            }
            tracing::warn!(key = %entry.key, "cache entry invalid, bypassing");
        } else {
            tracing::info!("cache miss");
        }
    } else if range_requested {
        tracing::info!("range request bypasses cache");
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
        if range_requested {
            tracing::info!("range response not cached");
        } else if status == StatusCode::PARTIAL_CONTENT {
            tracing::info!("206 response not cached (TODO: add range-aware caching)");
        } else if let Some(expires_at) = headers::compute_expiry(&parts.headers, cache_config.ttl_seconds) {
            if expires_at > now_unix() && status.is_success() {
                let stored_headers = headers::headers_for_storage(&parts.headers);
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
                    let response = Response::from_parts(parts, body.map_err(ProxyError::from).boxed());
                    return Ok(response);
                }

                let content_length = headers::parse_content_length(&parts.headers);
                if let Some(value) = content_length {
                    if cache_config.cache_max_object_size_bytes > 0
                        && value > cache_config.cache_max_object_size_bytes
                    {
                        tracing::info!(
                            content_length = value,
                            max_size = cache_config.cache_max_object_size_bytes,
                            "response too large to cache"
                        );
                        let response = Response::from_parts(
                            parts,
                            body.map_err(ProxyError::from).boxed(),
                        );
                        return Ok(response);
                    }
                }

                if cache_config.cache_require_content_length && content_length.is_none() {
                    tracing::info!("missing content-length; caching disabled");
                    let response = Response::from_parts(parts, body.map_err(ProxyError::from).boxed());
                    return Ok(response);
                }

                if cache_config.cache_channel_capacity_chunks == 0 {
                    tracing::warn!("cache_channel_capacity_chunks is 0; caching disabled");
                    let response = Response::from_parts(parts, body.map_err(ProxyError::from).boxed());
                    return Ok(response);
                }

                let key = cache_key(method.as_str(), &original_uri);
                let permit = match cache_pipeline::try_acquire_cache_write(&key) {
                    Some(permit) => permit,
                    None => {
                        tracing::info!(key = %key, "cache write in progress; skipping");
                        let response = Response::from_parts(
                            parts,
                            body.map_err(ProxyError::from).boxed(),
                        );
                        return Ok(response);
                    }
                };

                tracing::info!(key = %key, "cache store started");
                let (cached_body, writer) = cache_pipeline::cache_body(
                    body,
                    cache.clone(),
                    permit,
                    key,
                    method.as_str().to_string(),
                    original_uri.clone(),
                    status,
                    stored_headers,
                    expires_at,
                    cache.cache_dir().to_path_buf(),
                    content_length,
                    cache_config.cache_max_object_size_bytes,
                    cache_config.cache_require_content_length,
                    cache_config.cache_channel_capacity_chunks,
                    cache_config.cache_writer_delay_ms,
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
