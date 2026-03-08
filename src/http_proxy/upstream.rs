use crate::errors::ProxyError;
use hyper::header::{HeaderValue, HOST};
use hyper::{Request, Uri};

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
        assert_eq!(req.headers().get(HOST).unwrap(), "example.com");
    }

    #[test]
    fn preserves_explicit_port_in_host_header() {
        let uri = "http://example.com:8080/".parse::<Uri>().unwrap();
        let mut req = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Empty::<Bytes>::new())
            .unwrap();

        let target = rewrite_absolute_form(&mut req).unwrap();

        assert_eq!(target.port, 8080);
        assert_eq!(target.authority, "example.com:8080");
        assert_eq!(req.headers().get(HOST).unwrap(), "example.com:8080");
    }

    #[test]
    fn rejects_non_http_schemes() {
        let uri = "https://example.com/asset".parse::<Uri>().unwrap();
        let mut req = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Empty::<Bytes>::new())
            .unwrap();

        let err = rewrite_absolute_form(&mut req).unwrap_err();
        assert!(err.to_string().contains("unsupported scheme"));
    }

    #[test]
    fn defaults_to_root_path_when_missing_path_and_query() {
        let uri = "http://example.com".parse::<Uri>().unwrap();
        let mut req = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Empty::<Bytes>::new())
            .unwrap();

        let target = rewrite_absolute_form(&mut req).unwrap();
        assert_eq!(target.host, "example.com");
        assert_eq!(req.uri().to_string(), "/");
    }
}
