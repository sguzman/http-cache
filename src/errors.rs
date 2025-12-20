use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::Response;
use hyper::StatusCode;
use std::convert::Infallible;
use std::io;
use thiserror::Error;

pub type ProxyBody = BoxBody<Bytes, ProxyError>;

#[derive(Error, Debug)]
pub enum ProxyError {
    #[error("config read failed for {path}: {source}")]
    ConfigRead { path: String, source: io::Error },
    #[error("config parse failed for {path}: {source}")]
    ConfigParse { path: String, source: toml::de::Error },
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("http error: {0}")]
    Http(#[from] hyper::Error),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("policy denied: {0}")]
    PolicyDenied(String),
    #[error("upstream connect failed: {0}")]
    UpstreamConnect(String),
    #[error("timeout")]
    Timeout,
    #[error("internal error")]
    Internal,
}

impl ProxyError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            ProxyError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ProxyError::PolicyDenied(_) => StatusCode::FORBIDDEN,
            ProxyError::UpstreamConnect(_) => StatusCode::BAD_GATEWAY,
            ProxyError::Timeout => StatusCode::GATEWAY_TIMEOUT,
            ProxyError::ConfigRead { .. }
            | ProxyError::ConfigParse { .. }
            | ProxyError::Io(_)
            | ProxyError::Sqlite(_)
            | ProxyError::Http(_)
            | ProxyError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn to_response(&self) -> Response<ProxyBody> {
        let status = self.status_code();
        let body = Full::new(Bytes::from(self.to_string()))
            .map_err(|_: Infallible| ProxyError::Internal)
            .boxed();
        Response::builder()
            .status(status)
            .body(body)
            .unwrap_or_else(|_| {
                Response::new(
                    Full::new(Bytes::from("failed to build response"))
                        .map_err(|_: Infallible| ProxyError::Internal)
                        .boxed(),
                )
            })
    }
}

impl From<tokio::time::error::Elapsed> for ProxyError {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        ProxyError::Timeout
    }
}
