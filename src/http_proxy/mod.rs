use crate::cache::{cache_key, now_unix, Cache, CacheStoreRequest};
use crate::config::CacheConfig;
use crate::errors::{ProxyBody, ProxyError};
use crate::headers::strip_hop_by_hop;
use crate::policy::PolicyEngine;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
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
    max_request_body_bytes: u64,
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
    let req = wrap_limited_request_body(req, max_request_body_bytes)?;

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

fn wrap_limited_request_body<B>(
    req: Request<B>,
    max_request_body_bytes: u64,
) -> Result<Request<LimitedBody<B>>, ProxyError>
where
    B: Body<Data = Bytes>,
{
    if let Some(content_length) = req
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        if content_length > max_request_body_bytes {
            return Err(ProxyError::RequestBodyTooLarge);
        }
    }

    Ok(req.map(|body| LimitedBody::new(body, max_request_body_bytes)))
}

#[derive(Debug)]
struct LimitedBody<B> {
    inner: B,
    max_bytes: u64,
    seen_bytes: u64,
}

impl<B> LimitedBody<B> {
    fn new(inner: B, max_bytes: u64) -> Self {
        Self {
            inner,
            max_bytes,
            seen_bytes: 0,
        }
    }
}

impl<B> Body for LimitedBody<B>
where
    B: Body<Data = Bytes> + Unpin,
    ProxyError: From<B::Error>,
{
    type Data = Bytes;
    type Error = ProxyError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    self.seen_bytes = self.seen_bytes.saturating_add(data.len() as u64);
                    if self.seen_bytes > self.max_bytes {
                        return Poll::Ready(Some(Err(ProxyError::RequestBodyTooLarge)));
                    }
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(ProxyError::from(err)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::{Empty, Full};
    use hyper::Request;

    #[test]
    fn wrap_limited_request_body_rejects_large_content_length() {
        let req = Request::builder()
            .method("POST")
            .uri("http://example.com/upload")
            .header("content-length", "11")
            .body(Full::new(Bytes::from_static(b"hello world")))
            .unwrap();

        let err = wrap_limited_request_body(req, 10).unwrap_err();
        assert!(matches!(err, ProxyError::RequestBodyTooLarge));
    }

    #[tokio::test]
    async fn limited_body_allows_small_payloads() {
        let mut body = LimitedBody::new(Full::new(Bytes::from_static(b"ok")), 8);
        let frame = BodyExt::frame(&mut body).await.unwrap().unwrap();
        assert_eq!(frame.into_data().unwrap(), Bytes::from_static(b"ok"));
    }

    #[tokio::test]
    async fn limited_body_rejects_payloads_that_exceed_limit() {
        let mut body = LimitedBody::new(Full::new(Bytes::from(vec![b'x'; 16])), 8);
        let result = BodyExt::frame(&mut body).await.unwrap();
        assert!(matches!(result, Err(ProxyError::RequestBodyTooLarge)));
    }

    #[tokio::test]
    async fn limited_body_handles_empty_payloads() {
        let mut body = LimitedBody::new(Empty::<Bytes>::new(), 1);
        assert!(BodyExt::frame(&mut body).await.is_none());
    }
}
