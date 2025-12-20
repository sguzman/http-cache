use crate::cache::{cache_objects_dir, Cache, CacheEntry, CacheStoreRequest};
use crate::errors::{ProxyBody, ProxyError};
use crate::headers::strip_hop_by_hop;
use bytes::Bytes;
use http_body::Frame;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use hyper::{Response, StatusCode};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

pub(super) async fn cached_response(
    method: &hyper::Method,
    entry: &CacheEntry,
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
        HeaderValue::from_str(&entry.body_size.to_string()).map_err(|_| ProxyError::Internal)?,
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

    let path = entry.body_path.as_ref().ok_or_else(|| ProxyError::Internal)?;
    let file = tokio::fs::File::open(path).await?;
    let stream = ReaderStream::new(file).map(|result| result.map(Frame::data));
    let body = StreamBody::new(stream).map_err(ProxyError::from).boxed();

    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

pub(super) async fn cache_body(
    body: hyper::body::Incoming,
    cache: Arc<dyn Cache>,
    permit: CacheWritePermit,
    key: String,
    method: String,
    url: String,
    status: StatusCode,
    headers: Vec<(String, String)>,
    expires_at: i64,
    cache_dir: PathBuf,
    content_length: Option<u64>,
    max_object_size_bytes: u64,
    require_content_length: bool,
    channel_capacity_chunks: usize,
    writer_delay_ms: u64,
) -> Result<
    (
        ProxyBody,
        impl std::future::Future<Output = ()> + Send + 'static,
    ),
    ProxyError,
> {
    let objects_dir = cache_objects_dir(&cache_dir);
    let hash = hash_key(&key);
    let final_path = objects_dir.join(&hash);
    let temp_path = objects_dir.join(format!("{}.tmp-{}", hash, Uuid::new_v4()));

    let (tx, mut rx) = mpsc::channel::<Bytes>(channel_capacity_chunks);
    let (completion_tx, completion_rx) = oneshot::channel::<BodyCompletion>();
    let abort_flag = Arc::new(AtomicBool::new(false));
    let writer_cache = cache.clone();
    let created_at = crate::cache::now_unix();
    let abort_flag_writer = abort_flag.clone();

    let writer = async move {
        let _permit = permit;
        tracing::info!("cache writer started");
        let mut size: u64 = 0;
        let file = match tokio::fs::File::create(&temp_path).await {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!(error = %err, "cache file create failed");
                return;
            }
        };
        let mut writer = BufWriter::new(file);

        while let Some(chunk) = rx.recv().await {
            if abort_flag_writer.load(Ordering::Relaxed) {
                break;
            }
            if max_object_size_bytes > 0
                && size.saturating_add(chunk.len() as u64) > max_object_size_bytes
            {
                tracing::info!(
                    size = size,
                    max_size = max_object_size_bytes,
                    "cache object exceeded size limit"
                );
                abort_flag_writer.store(true, Ordering::Relaxed);
                break;
            }
            size += chunk.len() as u64;
            if let Err(err) = writer.write_all(&chunk).await {
                tracing::warn!(error = %err, "cache write failed");
                abort_flag_writer.store(true, Ordering::Relaxed);
                let _ = tokio::fs::remove_file(&temp_path).await;
                return;
            }
            if writer_delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(writer_delay_ms)).await;
            }
        }

        if abort_flag_writer.load(Ordering::Relaxed) {
            tracing::info!("cache writer aborted");
            let _ = tokio::fs::remove_file(&temp_path).await;
            return;
        }

        if require_content_length && content_length.is_none() {
            tracing::info!("cache writer missing content-length");
            tracing::info!("missing content-length; cache discarded");
            let _ = tokio::fs::remove_file(&temp_path).await;
            return;
        }

        let completion = completion_rx.await.ok();
        let completion_clean = completion.map(|c| c.clean).unwrap_or(false);

        if let Some(expected) = content_length {
            if size != expected {
                tracing::info!(size = size, expected = expected, "cache writer size mismatch");
                tracing::info!(
                    size = size,
                    expected = expected,
                    "content-length mismatch; cache discarded"
                );
                let _ = tokio::fs::remove_file(&temp_path).await;
                return;
            }
            if !completion_clean {
                tracing::info!(
                    size = size,
                    expected = expected,
                    "completion missing but size matches; committing"
                );
            }
        } else if completion_clean == false {
            tracing::info!("cache writer saw incomplete stream");
            tracing::info!("upstream body incomplete; cache discarded");
            let _ = tokio::fs::remove_file(&temp_path).await;
            return;
        }

        if max_object_size_bytes > 0 && size > max_object_size_bytes {
            tracing::info!(
                size = size,
                max_size = max_object_size_bytes,
                "cache object exceeded size limit"
            );
            let _ = tokio::fs::remove_file(&temp_path).await;
            return;
        }

        if let Err(err) = writer.flush().await {
            tracing::warn!(error = %err, "cache flush failed");
            let _ = tokio::fs::remove_file(&temp_path).await;
            return;
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
            let _ = tokio::fs::remove_file(&final_path).await;
            return;
        }

        tracing::info!(size = size, "cache commit succeeded");
    };

    let proxy_body = TeeBody::new(body, tx, completion_tx, abort_flag).boxed();
    Ok((proxy_body, writer))
}

#[derive(Debug)]
struct BodyCompletion {
    clean: bool,
}

struct TeeBody {
    inner: hyper::body::Incoming,
    tx: Option<mpsc::Sender<Bytes>>,
    completion_tx: Option<oneshot::Sender<BodyCompletion>>,
    abort_flag: Arc<AtomicBool>,
}

impl TeeBody {
    fn new(
        inner: hyper::body::Incoming,
        tx: mpsc::Sender<Bytes>,
        completion_tx: oneshot::Sender<BodyCompletion>,
        abort_flag: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inner,
            tx: Some(tx),
            completion_tx: Some(completion_tx),
            abort_flag,
        }
    }

    fn disable_cache(&mut self, reason: &'static str) {
        if !self.abort_flag.swap(true, Ordering::Relaxed) {
            tracing::info!(reason = reason, "cache disabled during streaming");
        }
        self.tx.take();
        self.completion_tx.take();
    }

    fn finish(&mut self, clean: bool) {
        if let Some(tx) = self.completion_tx.take() {
            let _ = tx.send(BodyCompletion { clean });
        }
        self.tx.take();
    }
}

impl http_body::Body for TeeBody {
    type Data = Bytes;
    type Error = ProxyError;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match std::pin::Pin::new(&mut self.inner).poll_frame(cx) {
            std::task::Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    if let Some(tx) = &self.tx {
                        match tx.try_send(data.clone()) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                self.disable_cache("cache buffer full");
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                self.disable_cache("cache writer closed");
                            }
                        }
                    }
                }
                std::task::Poll::Ready(Some(Ok(frame)))
            }
            std::task::Poll::Ready(Some(Err(err))) => {
                self.abort_flag.store(true, Ordering::Relaxed);
                self.finish(false);
                std::task::Poll::Ready(Some(Err(ProxyError::from(err))))
            }
            std::task::Poll::Ready(None) => {
                self.finish(true);
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for TeeBody {
    fn drop(&mut self) {
        if self.completion_tx.is_some() {
            self.finish(false);
        }
    }
}

pub(super) struct CacheWritePermit {
    key: String,
}

impl Drop for CacheWritePermit {
    fn drop(&mut self) {
        if let Some(locks) = CACHE_WRITE_LOCKS.get() {
            if let Ok(mut set) = locks.lock() {
                set.remove(&self.key);
            }
        }
    }
}

static CACHE_WRITE_LOCKS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

pub(super) fn try_acquire_cache_write(key: &str) -> Option<CacheWritePermit> {
    let locks = CACHE_WRITE_LOCKS.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = locks.lock().ok()?;
    if set.contains(key) {
        return None;
    }
    set.insert(key.to_string());
    Some(CacheWritePermit {
        key: key.to_string(),
    })
}

fn hash_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)
}
