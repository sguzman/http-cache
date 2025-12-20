use crate::errors::{ProxyBody, ProxyError};
use crate::policy::PolicyEngine;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tracing::Instrument;

pub async fn handle_connect(
    req: Request<Incoming>,
    policy: &PolicyEngine,
    connect_timeout: Duration,
    idle_timeout: Duration,
) -> Result<Response<ProxyBody>, ProxyError> {
    let authority = req
        .uri()
        .authority()
        .ok_or_else(|| ProxyError::BadRequest("missing authority".to_string()))?
        .clone();

    let host = authority.host().to_string();
    let port = authority
        .port_u16()
        .ok_or_else(|| ProxyError::BadRequest("missing port".to_string()))?;

    policy.check(&host, port)?;

    let addr = format!("{}:{}", host, port);
    let upstream = timeout(connect_timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| ProxyError::Timeout)??;

    let on_upgrade = hyper::upgrade::on(req);
    let span = tracing::info_span!("connect_tunnel", host = %host, port = port);

    tokio::spawn(
        async move {
            match on_upgrade.await {
                Ok(upgraded) => {
                    let mut upgraded = TokioIo::new(upgraded);
                    let mut upstream = upstream;
                    let result = timeout(
                        idle_timeout,
                        tokio::io::copy_bidirectional(&mut upgraded, &mut upstream),
                    )
                    .await;
                    match result {
                        Ok(Ok((_from_client, _from_upstream))) => {
                            tracing::info!("tunnel closed");
                        }
                        Ok(Err(err)) => {
                            tracing::warn!(error = %err, "tunnel error");
                        }
                        Err(_) => {
                            tracing::warn!("tunnel idle timeout");
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "upgrade failed");
                }
            }
        }
        .instrument(span),
    );

    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(Full::new(Bytes::new()).map_err(|_| ProxyError::Internal).boxed())
        .unwrap_or_else(|_| ProxyError::Internal.to_response()))
}
