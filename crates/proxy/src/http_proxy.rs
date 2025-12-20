use crate::errors::{ProxyBody, ProxyError};
use crate::headers::strip_hop_by_hop;
use crate::policy::PolicyEngine;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::header::{HeaderValue, HOST};
use hyper::{Request, Response, Uri};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tracing::Instrument;

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
    connect_timeout: Duration,
) -> Result<Response<ProxyBody>, ProxyError> {
    let target = rewrite_absolute_form(&mut req)?;
    policy.check(&target.host, target.port)?;

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
    Ok(response.map(|body| body.map_err(ProxyError::from).boxed()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
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
        assert_eq!(
            req.headers().get(HOST).unwrap(),
            "example.com"
        );
    }
}
