use bytes::Bytes;
use http_body_util::{BodyExt, StreamBody};
use httpcache::cache::cache_key;
use httpcache::config::Config;
use httpcache::server::serve;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use rusqlite::OptionalExtension;
use serial_test::serial;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::sync::Once;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_stream::{self as stream};

static TEST_LOCK: Mutex<()> = Mutex::new(());
static TEST_TRACING: Once = Once::new();

fn lock_test() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

fn init_tracing() {
    TEST_TRACING.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter("info")
            .try_init();
    });
}

fn clear_cache_dir() {
    let _ = std::fs::remove_dir_all(".cache");
}

async fn spawn_proxy(mut config: Config) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    config.listen.host = "127.0.0.1".to_string();
    config.listen.port = addr.port();

    let handle = tokio::spawn(async move {
        let _ = serve(listener, config).await;
    });

    (addr, handle)
}

#[derive(Clone, Copy)]
enum UpstreamMode {
    Normal,
    ErrorAfterBytes(usize),
    RangeAware,
}

async fn spawn_streaming_upstream(
    mode: UpstreamMode,
    total_bytes: usize,
    chunk_size: usize,
    include_content_length: bool,
) -> (SocketAddr, JoinHandle<()>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let request_count = Arc::new(AtomicUsize::new(0));
    let request_count_task = request_count.clone();

    let handle = tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            let request_count = request_count_task.clone();
            let mode = mode;
            let service = service_fn(move |req: Request<Incoming>| {
                request_count.fetch_add(1, Ordering::SeqCst);
                let mode = mode;
                async move {
                    let is_head = req.method() == Method::HEAD;
                    let mut response = match mode {
                        UpstreamMode::RangeAware => {
                            if req.headers().contains_key(hyper::header::RANGE) {
                                Response::builder()
                                    .status(StatusCode::PARTIAL_CONTENT)
                                    .header(hyper::header::CONTENT_RANGE, "bytes 0-99/1000")
                                    .body(build_stream_body(
                                        if is_head { 0 } else { 100 },
                                        chunk_size,
                                        None,
                                    ))
                                    .unwrap()
                            } else {
                                Response::builder()
                                    .status(StatusCode::OK)
                                    .body(build_stream_body(
                                        if is_head { 0 } else { total_bytes },
                                        chunk_size,
                                        None,
                                    ))
                                    .unwrap()
                            }
                        }
                        UpstreamMode::ErrorAfterBytes(limit) => Response::builder()
                            .status(StatusCode::OK)
                            .body(build_stream_body(
                                if is_head { 0 } else { total_bytes },
                                chunk_size,
                                Some(limit),
                            ))
                            .unwrap(),
                        UpstreamMode::Normal => Response::builder()
                            .status(StatusCode::OK)
                            .body(build_stream_body(
                                if is_head { 0 } else { total_bytes },
                                chunk_size,
                                None,
                            ))
                            .unwrap(),
                    };

                    if include_content_length {
                        let len = match mode {
                            UpstreamMode::RangeAware
                                if req.headers().contains_key(hyper::header::RANGE) =>
                            {
                                100
                            }
                            _ => total_bytes,
                        };
                        response.headers_mut().insert(
                            hyper::header::CONTENT_LENGTH,
                            len.to_string().parse().unwrap(),
                        );
                    }

                    Ok::<_, hyper::Error>(response)
                }
            });

            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        }
    });

    (addr, handle, request_count)
}

fn build_stream_body(
    total_bytes: usize,
    chunk_size: usize,
    error_after: Option<usize>,
) -> StreamBody<impl stream::Stream<Item = Result<http_body::Frame<Bytes>, std::io::Error>>> {
    let mut remaining = total_bytes;
    let mut sent = 0;
    let mut frames = Vec::new();

    while remaining > 0 {
        let to_send = remaining.min(chunk_size);
        remaining -= to_send;
        sent += to_send;
        frames.push(Ok(http_body::Frame::data(Bytes::from(vec![b'x'; to_send]))));

        if let Some(limit) = error_after {
            if sent >= limit {
                frames.push(Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "stream error",
                )));
                break;
            }
        }
    }

    StreamBody::new(stream::iter(frames))
}

async fn fetch_via_proxy(
    proxy_addr: SocketAddr,
    uri: Uri,
    headers: Vec<(&'static str, &'static str)>,
) -> (StatusCode, hyper::HeaderMap, Result<Bytes, hyper::Error>) {
    fetch_via_proxy_with_method(proxy_addr, Method::GET, uri, headers).await
}

async fn fetch_via_proxy_with_method(
    proxy_addr: SocketAddr,
    method: Method,
    uri: Uri,
    headers: Vec<(&'static str, &'static str)>,
) -> (StatusCode, hyper::HeaderMap, Result<Bytes, hyper::Error>) {
    let stream = TcpStream::connect(proxy_addr).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = Request::builder().method(method).uri(uri);
    {
        let headers_map = builder.headers_mut().unwrap();
        for (name, value) in headers {
            headers_map.insert(
                hyper::header::HeaderName::from_static(name),
                value.parse().unwrap(),
            );
        }
    }
    let req = builder.body(http_body_util::Empty::<Bytes>::new()).unwrap();

    let response = sender.send_request(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.map(|b| b.to_bytes());
    (status, headers, body)
}

fn read_cache_entry(key: &str) -> Option<(String, u64)> {
    let db_path = Path::new(".cache/cache.sqlite");
    if !db_path.exists() {
        return None;
    }
    let conn = rusqlite::Connection::open(db_path).ok()?;
    conn.query_row(
        "SELECT body_path, body_size FROM cache_entries WHERE key = ?1",
        [key],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64)),
    )
    .optional()
    .ok()
    .flatten()
}

fn cache_entry_exists(key: &str) -> bool {
    let db_path = Path::new(".cache/cache.sqlite");
    if !db_path.exists() {
        return false;
    }
    let conn = match rusqlite::Connection::open(db_path) {
        Ok(conn) => conn,
        Err(_) => return false,
    };
    conn.query_row(
        "SELECT 1 FROM cache_entries WHERE key = ?1",
        [key],
        |_row| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

fn read_cache_keys() -> Vec<String> {
    let db_path = Path::new(".cache/cache.sqlite");
    if !db_path.exists() {
        return Vec::new();
    }
    let conn = match rusqlite::Connection::open(db_path) {
        Ok(conn) => conn,
        Err(_) => return Vec::new(),
    };
    let mut stmt = match conn.prepare("SELECT key FROM cache_entries ORDER BY key ASC") {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };
    let rows = match stmt.query_map([], |row| row.get::<_, String>(0)) {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };
    rows.filter_map(Result::ok).collect()
}

async fn wait_for_cache_entry(key: &str, timeout_ms: u64) -> Option<(String, u64)> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if let Some(entry) = read_cache_entry(key) {
            return Some(entry);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

fn list_cache_files() -> HashSet<String> {
    let mut files = HashSet::new();
    let path = Path::new(".cache/objects");
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                files.insert(name.to_string());
            }
        }
    }
    files
}

#[tokio::test]
#[serial]
async fn cache_streams_and_replays() {
    let _guard = lock_test();
    init_tracing();
    clear_cache_dir();

    let (upstream_addr, upstream_handle, request_count) =
        spawn_streaming_upstream(UpstreamMode::Normal, 262_144, 8192, true).await;

    let mut config = Config::default();
    config.policy.allowed_domains = vec!["*".to_string()];
    config.policy.allowed_ports = vec![upstream_addr.port()];
    config.caching.enabled = true;
    config.caching.cache_channel_capacity_chunks = 128;
    config.caching.cache_max_object_size_bytes = 1_073_741_824;
    config.caching.cache_require_content_length = false;

    let (proxy_addr, proxy_handle) = spawn_proxy(config).await;

    let uri: Uri = format!("http://{}/large", upstream_addr).parse().unwrap();
    let (status, headers, body) = fetch_via_proxy(proxy_addr, uri.clone(), Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    let body = body.unwrap();
    let content_length = headers
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    assert_eq!(content_length, Some(body.len()));

    let key = cache_key("GET", &format!("http://{}/large", upstream_addr));
    let entry = wait_for_cache_entry(&key, 500).await;
    let (body_path, body_size) = match entry {
        Some(entry) => entry,
        None => {
            panic!("cache entry missing; files: {:?}", list_cache_files());
        }
    };

    let (status, _headers, cached_body) = fetch_via_proxy(proxy_addr, uri, Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    let cached_body = cached_body.unwrap();

    assert_eq!(request_count.load(Ordering::SeqCst), 1);
    assert_eq!(body.len(), cached_body.len());
    assert_eq!(body, cached_body);

    assert_eq!(body_size as usize, body.len());
    let file_bytes = std::fs::read(body_path).unwrap();
    assert_eq!(file_bytes.len(), body.len());
    assert_eq!(Sha256::digest(&file_bytes), Sha256::digest(&body));

    proxy_handle.abort();
    upstream_handle.abort();
}

#[tokio::test]
#[serial]
async fn cache_discards_on_body_error() {
    let _guard = lock_test();
    init_tracing();
    clear_cache_dir();

    let total_bytes = 131_072;
    let (upstream_addr, upstream_handle, _) =
        spawn_streaming_upstream(UpstreamMode::ErrorAfterBytes(32_768), total_bytes, 8192, true)
            .await;

    let mut config = Config::default();
    config.policy.allowed_domains = vec!["*".to_string()];
    config.policy.allowed_ports = vec![upstream_addr.port()];
    config.caching.enabled = true;
    config.caching.cache_channel_capacity_chunks = 8;
    config.caching.cache_max_object_size_bytes = 1_073_741_824;
    config.caching.cache_require_content_length = true;

    let (proxy_addr, proxy_handle) = spawn_proxy(config).await;

    let uri: Uri = format!("http://{}/abort", upstream_addr).parse().unwrap();
    let (_status, _headers, body) = fetch_via_proxy(proxy_addr, uri.clone(), Vec::new()).await;
    if let Ok(bytes) = body {
        assert!(bytes.len() < total_bytes);
    }

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let key = cache_key("GET", &format!("http://{}/abort", upstream_addr));
    assert!(read_cache_entry(&key).is_none());
    assert!(list_cache_files().is_empty());

    proxy_handle.abort();
    upstream_handle.abort();
}

#[tokio::test]
#[serial]
async fn range_responses_are_not_cached() {
    let _guard = lock_test();
    init_tracing();
    clear_cache_dir();

    let (upstream_addr, upstream_handle, _) =
        spawn_streaming_upstream(UpstreamMode::RangeAware, 1000, 128, true).await;

    let mut config = Config::default();
    config.policy.allowed_domains = vec!["*".to_string()];
    config.policy.allowed_ports = vec![upstream_addr.port()];
    config.caching.enabled = true;
    config.caching.cache_channel_capacity_chunks = 8;
    config.caching.cache_max_object_size_bytes = 1_073_741_824;
    config.caching.cache_require_content_length = true;

    let (proxy_addr, proxy_handle) = spawn_proxy(config).await;

    let uri: Uri = format!("http://{}/range", upstream_addr).parse().unwrap();
    let (status, _headers, body) = fetch_via_proxy(
        proxy_addr,
        uri.clone(),
        vec![("range", "bytes=0-99")],
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body.unwrap().len(), 100);

    let key = cache_key("GET", &format!("http://{}/range", upstream_addr));
    assert!(read_cache_entry(&key).is_none());
    assert!(list_cache_files().is_empty());

    proxy_handle.abort();
    upstream_handle.abort();
}

#[tokio::test]
#[serial]
async fn cache_disables_on_buffer_pressure() {
    let _guard = lock_test();
    init_tracing();
    clear_cache_dir();

    let (upstream_addr, upstream_handle, _) =
        spawn_streaming_upstream(UpstreamMode::Normal, 262_144, 4096, true).await;

    let mut config = Config::default();
    config.policy.allowed_domains = vec!["*".to_string()];
    config.policy.allowed_ports = vec![upstream_addr.port()];
    config.caching.enabled = true;
    config.caching.cache_channel_capacity_chunks = 1;
    config.caching.cache_max_object_size_bytes = 1_073_741_824;
    config.caching.cache_require_content_length = true;
    config.caching.cache_writer_delay_ms = 20;

    let (proxy_addr, proxy_handle) = spawn_proxy(config).await;

    let uri: Uri = format!("http://{}/pressure", upstream_addr).parse().unwrap();
    let (status, _headers, body) = fetch_via_proxy(proxy_addr, uri.clone(), Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.unwrap().len(), 262_144);

    let key = cache_key("GET", &format!("http://{}/pressure", upstream_addr));
    assert!(read_cache_entry(&key).is_none());
    assert!(list_cache_files().is_empty());

    proxy_handle.abort();
    upstream_handle.abort();
}

#[tokio::test]
#[serial]
async fn head_cache_replays_without_upstream_round_trip() {
    let _guard = lock_test();
    init_tracing();
    clear_cache_dir();

    let total_bytes = 4096;
    let (upstream_addr, upstream_handle, request_count) =
        spawn_streaming_upstream(UpstreamMode::Normal, total_bytes, 1024, true).await;

    let mut config = Config::default();
    config.policy.allowed_domains = vec!["*".to_string()];
    config.policy.allowed_ports = vec![upstream_addr.port()];
    config.caching.enabled = true;
    config.caching.cache_require_content_length = true;

    let (proxy_addr, proxy_handle) = spawn_proxy(config).await;

    let url = format!("http://{}/head", upstream_addr);
    let uri: Uri = url.parse().unwrap();
    let (status, headers, body) =
        fetch_via_proxy_with_method(proxy_addr, Method::HEAD, uri.clone(), Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.unwrap().is_empty());
    assert_eq!(
        headers.get(hyper::header::CONTENT_LENGTH).unwrap(),
        total_bytes.to_string().as_str()
    );

    let key = cache_key("HEAD", &url);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(cache_entry_exists(&key), "HEAD cache entry missing");

    let (status, headers, body) =
        fetch_via_proxy_with_method(proxy_addr, Method::HEAD, uri, Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.unwrap().is_empty());
    assert_eq!(
        headers.get(hyper::header::CONTENT_LENGTH).unwrap(),
        total_bytes.to_string().as_str()
    );
    assert_eq!(request_count.load(Ordering::SeqCst), 1);

    proxy_handle.abort();
    upstream_handle.abort();
}

#[tokio::test]
#[serial]
async fn cache_entries_expire_and_refetch() {
    let _guard = lock_test();
    init_tracing();
    clear_cache_dir();

    let (upstream_addr, upstream_handle, request_count) =
        spawn_streaming_upstream(UpstreamMode::Normal, 2048, 256, true).await;

    let mut config = Config::default();
    config.policy.allowed_domains = vec!["*".to_string()];
    config.policy.allowed_ports = vec![upstream_addr.port()];
    config.caching.enabled = true;
    config.caching.ttl_seconds = 1;

    let (proxy_addr, proxy_handle) = spawn_proxy(config).await;

    let uri: Uri = format!("http://{}/expiring", upstream_addr).parse().unwrap();
    let (status, _headers, body) = fetch_via_proxy(proxy_addr, uri.clone(), Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.unwrap().len(), 2048);

    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;

    let (status, _headers, body) = fetch_via_proxy(proxy_addr, uri, Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.unwrap().len(), 2048);
    assert_eq!(request_count.load(Ordering::SeqCst), 2);

    proxy_handle.abort();
    upstream_handle.abort();
}

#[tokio::test]
#[serial]
async fn cache_eviction_removes_least_recently_used_entry() {
    let _guard = lock_test();
    init_tracing();
    clear_cache_dir();

    let (upstream_addr, upstream_handle, _request_count) =
        spawn_streaming_upstream(UpstreamMode::Normal, 1024, 256, true).await;

    let mut config = Config::default();
    config.policy.allowed_domains = vec!["*".to_string()];
    config.policy.allowed_ports = vec![upstream_addr.port()];
    config.caching.enabled = true;
    config.caching.max_entries = 1;

    let (proxy_addr, proxy_handle) = spawn_proxy(config).await;

    let one_url = format!("http://{}/one", upstream_addr);
    let two_url = format!("http://{}/two", upstream_addr);
    let one_uri: Uri = one_url.parse().unwrap();
    let two_uri: Uri = two_url.parse().unwrap();

    assert_eq!(
        fetch_via_proxy(proxy_addr, one_uri, Vec::new()).await.0,
        StatusCode::OK
    );
    assert!(wait_for_cache_entry(&cache_key("GET", &one_url), 500).await.is_some());

    assert_eq!(
        fetch_via_proxy(proxy_addr, two_uri, Vec::new()).await.0,
        StatusCode::OK
    );
    assert!(wait_for_cache_entry(&cache_key("GET", &two_url), 500).await.is_some());

    let keys = read_cache_keys();
    assert_eq!(keys, vec![cache_key("GET", &two_url)]);
    assert_eq!(list_cache_files().len(), 1);

    proxy_handle.abort();
    upstream_handle.abort();
}

#[tokio::test]
#[serial]
async fn missing_cached_body_file_triggers_refetch() {
    let _guard = lock_test();
    init_tracing();
    clear_cache_dir();

    let (upstream_addr, upstream_handle, request_count) =
        spawn_streaming_upstream(UpstreamMode::Normal, 3072, 512, true).await;

    let mut config = Config::default();
    config.policy.allowed_domains = vec!["*".to_string()];
    config.policy.allowed_ports = vec![upstream_addr.port()];
    config.caching.enabled = true;

    let (proxy_addr, proxy_handle) = spawn_proxy(config).await;

    let url = format!("http://{}/missing-file", upstream_addr);
    let uri: Uri = url.parse().unwrap();
    let key = cache_key("GET", &url);

    assert_eq!(
        fetch_via_proxy(proxy_addr, uri.clone(), Vec::new()).await.0,
        StatusCode::OK
    );
    let (body_path, _) = wait_for_cache_entry(&key, 500)
        .await
        .expect("cache entry missing after initial fetch");
    std::fs::remove_file(&body_path).unwrap();

    assert_eq!(
        fetch_via_proxy(proxy_addr, uri, Vec::new()).await.0,
        StatusCode::OK
    );
    assert_eq!(request_count.load(Ordering::SeqCst), 2);
    assert!(wait_for_cache_entry(&key, 500).await.is_some());

    proxy_handle.abort();
    upstream_handle.abort();
}
