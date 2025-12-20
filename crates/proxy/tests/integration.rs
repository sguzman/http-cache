use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, Uri};
use hyper_util::rt::TokioIo;
use proxy::config::Config;
use proxy::server::serve;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Mutex};
use std::sync::Arc;

struct UpstreamCapture {
    uri: String,
    host: Option<String>,
}

async fn spawn_upstream() -> (SocketAddr, oneshot::Receiver<UpstreamCapture>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    let tx = Arc::new(Mutex::new(Some(tx)));

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let service = service_fn(move |req: Request<Incoming>| {
            let tx = tx.clone();
            async move {
                let capture = UpstreamCapture {
                    uri: req.uri().to_string(),
                    host: req
                        .headers()
                        .get(hyper::header::HOST)
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string()),
                };
                if let Some(sender) = tx.lock().await.take() {
                    let _ = sender.send(capture);
                }
                Ok::<_, hyper::Error>(Response::new(
                    Full::new(Bytes::from_static(b"ok")),
                ))
            }
        });

        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service)
            .await;
    });

    (addr, rx)
}

async fn spawn_proxy(mut config: Config) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    config.listen.host = "127.0.0.1".to_string();
    config.listen.port = addr.port();

    let handle = tokio::spawn(async move {
        let _ = serve(listener, config).await;
    });

    (addr, handle)
}

#[tokio::test]
async fn http_proxy_forwards_origin_form() {
    let (upstream_addr, upstream_rx) = spawn_upstream().await;
    let mut config = Config::default();
    config.policy.allowed_domains = vec!["*".to_string()];
    config.policy.allowed_ports = vec![upstream_addr.port()];

    let (proxy_addr, proxy_handle) = spawn_proxy(config).await;

    let stream = TcpStream::connect(proxy_addr).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let uri: Uri = format!("http://{}/hello?name=test", upstream_addr)
        .parse()
        .unwrap();
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Empty::<Bytes>::new())
        .unwrap();

    let response = sender.send_request(req).await.unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body, Bytes::from_static(b"ok"));

    let capture = upstream_rx.await.unwrap();
    assert_eq!(capture.uri, "/hello?name=test");
    let expected_host = format!("{upstream_addr}");
    assert_eq!(capture.host.as_deref(), Some(expected_host.as_str()));

    proxy_handle.abort();
}

#[tokio::test]
async fn connect_tunnel_echoes_bytes() {
    let echo_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo_listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = echo_listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).await.unwrap();
        stream.write_all(&buf[..n]).await.unwrap();
    });

    let mut config = Config::default();
    config.policy.allowed_domains = vec!["*".to_string()];
    config.policy.allowed_ports = vec![echo_addr.port()];
    let (proxy_addr, proxy_handle) = spawn_proxy(config).await;

    let mut stream = TcpStream::connect(proxy_addr).await.unwrap();
    let connect_req = format!(
        "CONNECT {} HTTP/1.1\r\nHost: {}\r\n\r\n",
        echo_addr, echo_addr
    );
    stream.write_all(connect_req.as_bytes()).await.unwrap();

    let mut response_buf = Vec::new();
    let mut tmp = [0u8; 128];
    loop {
        let n = stream.read(&mut tmp).await.unwrap();
        if n == 0 {
            break;
        }
        response_buf.extend_from_slice(&tmp[..n]);
        if response_buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    let response_text = String::from_utf8_lossy(&response_buf);
    assert!(response_text.starts_with("HTTP/1.1 200"));

    let payload = b"ping-proxy";
    stream.write_all(payload).await.unwrap();

    let mut echoed = vec![0u8; payload.len()];
    stream.read_exact(&mut echoed).await.unwrap();
    assert_eq!(echoed, payload);

    proxy_handle.abort();
}
