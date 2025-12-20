use crate::cache::{build_cache, Cache};
use crate::config::Config;
use crate::connect_tunnel::handle_connect;
use crate::errors::{ProxyBody, ProxyError};
use crate::http_proxy::handle_http;
use crate::obs::init_tracing;
use crate::policy::PolicyEngine;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::time::Duration;
use tracing::Instrument;
use uuid::Uuid;
use std::time::Instant;

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    policy: Arc<PolicyEngine>,
    cache: Arc<dyn Cache>,
}

impl AppState {
    fn new(config: Arc<Config>, cache: Arc<dyn Cache>) -> Self {
        let policy = Arc::new(PolicyEngine::new(&config.policy));
        Self {
            config,
            policy,
            cache,
        }
    }
}

pub async fn run(config: Config) -> Result<(), ProxyError> {
    init_tracing(config.env.mode, &config.logging, &config.time);
    let addr = format!("{}:{}", config.listen.host, config.listen.port);
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("listening on {addr}");
    serve(listener, config).await
}

pub async fn serve(listener: TcpListener, config: Config) -> Result<(), ProxyError> {
    let cache = build_cache(&config.caching).await?;
    let state = AppState::new(Arc::new(config), cache);
    serve_with_state(listener, state).await
}

async fn serve_with_state(listener: TcpListener, state: AppState) -> Result<(), ProxyError> {
    let max_conns = state.config.limits.max_connections;
    let semaphore = Arc::new(Semaphore::new(max_conns));

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let state = state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(err) = handle_connection(stream, peer_addr, state).await {
                tracing::warn!(error = %err, "connection failed");
            }
        });
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    state: AppState,
) -> Result<(), ProxyError> {
    let max_header_bytes = state.config.limits.max_header_bytes;
    let service = service_fn(move |req| {
        let state = state.clone();
        async move { handle_request(req, peer_addr, state).await }
    });

    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .keep_alive(true)
        // hyper exposes max_headers; we map the byte limit to a header count cap.
        .max_headers(max_header_bytes);

    builder
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades()
        .await?;

    Ok(())
}

async fn handle_request(
    req: Request<Incoming>,
    peer_addr: SocketAddr,
    state: AppState,
) -> Result<Response<ProxyBody>, Infallible> {
    let request_id = Uuid::new_v4().to_string();
    let start = Instant::now();
    let method = req.method().clone();
    let uri = req.uri().to_string();
    let cache_name = state.cache.name();
    let cache_enabled = state.cache.is_enabled();
    let span = tracing::info_span!(
        "request",
        request_id = %request_id,
        method = %method,
        uri = %uri,
        client_ip = %peer_addr.ip(),
        cache = cache_name,
        cache_enabled = cache_enabled
    );

    let response = async move {
        let connect_timeout = Duration::from_millis(state.config.limits.connect_timeout_ms);
        let idle_timeout = Duration::from_millis(state.config.limits.idle_timeout_ms);

        if state.config.limits.per_ip_rps == 0 {
            tracing::warn!("per_ip_rps is 0; all requests will be effectively denied");
        }

        let result = match method {
            Method::CONNECT => {
                handle_connect(req, &state.policy, connect_timeout, idle_timeout).await
            }
            _ => {
                handle_http(
                    req,
                    &state.policy,
                    state.cache.clone(),
                    connect_timeout,
                    state.config.caching.ttl_seconds,
                )
                .await
            }
        };

        match result {
            Ok(response) => {
                let status = response.status();
                tracing::debug!(status = %status, headers = ?response.headers(), "response details");
                let span = tracing::Span::current();
                response.map(|body| TimedBody::new(body, start, status, span).boxed())
            }
            Err(err) => {
                tracing::warn!(error = %err, "request failed");
                err.to_response()
            }
        }
    }
    .instrument(span)
    .await;

    Ok(response)
}

struct TimedBody<B> {
    inner: B,
    start: Instant,
    ttfb: Option<Duration>,
    bytes: u64,
    status: hyper::StatusCode,
    span: tracing::Span,
    logged: bool,
}

impl<B> TimedBody<B> {
    fn new(inner: B, start: Instant, status: hyper::StatusCode, span: tracing::Span) -> Self {
        Self {
            inner,
            start,
            ttfb: None,
            bytes: 0,
            status,
            span,
            logged: false,
        }
    }

    fn note_first_byte(&mut self) {
        if self.ttfb.is_none() {
            self.ttfb = Some(self.start.elapsed());
        }
    }

    fn log_complete(&mut self) {
        if self.logged {
            return;
        }
        self.logged = true;
        let duration_ms = self.start.elapsed().as_millis();
        let ttfb_ms = self.ttfb.map(|d| d.as_millis());
        let bytes = self.bytes;
        let status = self.status;
        self.span.in_scope(|| {
            tracing::info!(
                status = %status,
                duration_ms = duration_ms,
                ttfb_ms = ?ttfb_ms,
                bytes = bytes,
                "request completed"
            );
        });
    }
}

impl<B> Body for TimedBody<B>
where
    B: Body<Data = Bytes, Error = ProxyError> + Unpin,
{
    type Data = Bytes;
    type Error = ProxyError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let poll = Pin::new(&mut self.inner).poll_frame(cx);
        match poll {
            Poll::Ready(Some(Ok(frame))) => {
                self.note_first_byte();
                if let Some(data) = frame.data_ref() {
                    self.bytes += data.len() as u64;
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(err))) => {
                self.note_first_byte();
                Poll::Ready(Some(Err(err)))
            }
            Poll::Ready(None) => {
                self.log_complete();
                Poll::Ready(None)
            }
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
