//! Wire-level tests: a local HTTPS origin behind an HTTP CONNECT proxy.
//! The test-only trust root is never compiled into the production binary.
use crate::{config::Config, egress::Egress, http::App};
use axum::{
    body::{Body, Bytes, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use std::{
    convert::Infallible,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower::ServiceExt;

type Seen = Arc<Mutex<Vec<(String, String, bool, usize)>>>;
struct Fixture {
    app: App,
    seen: Seen,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn fixture() -> Fixture {
    fixture_with_limit(128).await
}
async fn fixture_with_limit(limit: usize) -> Fixture {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cert = rcgen::generate_simple_self_signed(vec![
        "api.github.com".into(),
        "uploads.github.com".into(),
        "github.com".into(),
        "release-assets.githubusercontent.com".into(),
    ])
    .unwrap();
    let root = reqwest::Certificate::from_der(cert.cert.der()).unwrap();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert.der().clone()], key.into())
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            let seen = captured.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(socket.read_u8().await.unwrap());
                    assert!(head.len() < 4096);
                }
                assert!(head.starts_with(b"CONNECT "));
                socket
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .unwrap();
                let tls = acceptor.accept(socket).await.unwrap();
                let service = service_fn(move |req: hyper::Request<Incoming>| {
                    let seen = seen.clone();
                    async move {
                        let method = req.method().to_string();
                        let path = req.uri().path().to_string();
                        let authorization = req.headers().get("authorization").cloned();
                        let auth = authorization.is_some();
                        let body = req.into_body().collect().await.unwrap().to_bytes();
                        seen.lock()
                            .unwrap()
                            .push((method.clone(), path.clone(), auth, body.len()));
                        let mut response = hyper::Response::builder();
                        let bytes = if path.ends_with("/info/refs") {
                            if authorization
                                .as_ref()
                                .is_some_and(|v| v == "Basic Zml4dHVyZTp0b2tlbg==")
                            {
                                response = response.header(
                                    "content-type",
                                    "application/x-git-upload-pack-advertisement",
                                );
                                Bytes::from_static(b"fixture-git-advertisement")
                            } else {
                                response = response
                                    .status(401)
                                    .header("www-authenticate", "Basic realm=\"GitHub\"")
                                    .header("www-authenticate", "Bearer realm=\"fixture\"");
                                Bytes::from_static(b"Authentication required")
                            }
                        } else if path == "/repos/class-undefined/gh-proxy/releases/401" {
                            response = response
                                .status(401)
                                .header("www-authenticate", "Basic realm=\"GitHub\"");
                            Bytes::from_static(b"Bad credentials")
                        } else if path == "/repos/class-undefined/gh-proxy/releases/403" {
                            response = response
                                .status(403)
                                .header(
                                    "x-github-sso",
                                    "required; url=https://github.com/orgs/fixture/sso",
                                )
                                .header("x-github-request-id", "fixture-request");
                            Bytes::from_static(b"SSO authorization required")
                        } else if path.starts_with("/github-production-release-asset/") {
                            assert!(!auth, "credential leaked to attachment CDN");
                            response = response.header("content-type", "application/octet-stream");
                            Bytes::from(vec![0x5a; 10 * 1024 * 1024])
                        } else if path == "/repos/class-undefined/gh-proxy/releases/assets/1" {
                            response = response.status(302).header("location","https://release-assets.githubusercontent.com/github-production-release-asset/123/asset?sig=secret");
                            Bytes::new()
                        } else if path.ends_with("/git-upload-pack")
                            || path.ends_with("/git-receive-pack")
                        {
                            Bytes::from(body.to_vec())
                        } else if path == "/repos/class-undefined/gh-proxy/releases/1/assets" {
                            response = response
                                .status(201)
                                .header("content-type", "application/json");
                            Bytes::from_static(b"{\"url\":\"https://api.github.com/repos/class-undefined/gh-proxy/releases/assets/2\"}")
                        } else {
                            response = response
                                .status(if method == "POST" { 201 } else { 200 })
                                .header("content-type", "application/json");
                            Bytes::from_static(b"{\"upload_url\":\"https://uploads.github.com/repos/class-undefined/gh-proxy/releases/1/assets{?name,label}\"}")
                        };
                        Ok::<_, Infallible>(response.body(Full::new(bytes)).unwrap())
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(tls), service)
                    .await;
            });
        }
    });
    let mut egress = Egress::from_lookup(
        |k| (k == "HTTPS_PROXY").then(|| format!("http://{addr}")),
        false,
        Duration::from_secs(5),
    )
    .unwrap();
    egress.test_root = Some(root);
    let mut config: Config =
        toml::from_str(include_str!("../tests/fixtures/gateway.toml")).unwrap();
    config.max_connections = limit;
    let r = &mut config.repositories[0];
    r.write = true;
    r.release.create = true;
    r.release.update = true;
    r.release.upload = true;
    Fixture {
        app: App::new(Arc::new(config), Arc::new(egress)),
        seen,
        task,
    }
}
async fn call(
    f: &Fixture,
    method: &str,
    path: &str,
    body: Vec<u8>,
    binary: bool,
) -> axum::response::Response {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", "Bearer test-token")
        .header(
            "accept",
            if binary {
                "application/octet-stream"
            } else {
                "application/vnd.github+json"
            },
        )
        .body(Body::from(body))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo("127.0.0.1:1000".parse::<SocketAddr>().unwrap()));
    f.app.clone().router().oneshot(request).await.unwrap()
}
#[tokio::test]
async fn release_create_update_upload_download_over_connect() {
    let f = fixture().await;
    let response = call(
        &f,
        "POST",
        "/api/github/repos/class-undefined/gh-proxy/releases",
        b"{\"tag_name\":\"v1\"}".to_vec(),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let json: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(
        json["upload_url"],
        "http://127.0.0.1:8080/uploads/github/repos/class-undefined/gh-proxy/releases/1/assets{?name,label}"
    );
    let response = call(
        &f,
        "PATCH",
        "/api/github/repos/class-undefined/gh-proxy/releases/1",
        b"{}".to_vec(),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let upload_size = 2 * 1024 * 1024;
    let response = call(
        &f,
        "POST",
        "/uploads/github/repos/class-undefined/gh-proxy/releases/1/assets?name=large.bin",
        vec![0x44; upload_size],
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = call(
        &f,
        "GET",
        "/api/github/repos/class-undefined/gh-proxy/releases/assets/1",
        vec![],
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("location"));
    let body = to_bytes(response.into_body(), 11 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(body.len(), 10 * 1024 * 1024);
    assert!(body.iter().all(|b| *b == 0x5a));
    let seen = f.seen.lock().unwrap();
    assert!(seen.iter().any(|(m, p, a, n)| m == "POST"
        && p.ends_with("/releases/1/assets")
        && *a
        && *n == upload_size));
    assert!(
        seen.iter()
            .any(|(_, p, a, _)| p.starts_with("/github-production-release-asset/") && !a)
    );
}
#[tokio::test]
async fn git_rpc_streams_and_unknown_routes_never_reach_upstream() {
    let f = fixture().await;
    for service in ["git-upload-pack", "git-receive-pack"] {
        let data = vec![0x66; 1024 * 1024];
        let response = call(
            &f,
            "POST",
            &format!("/class-undefined/gh-proxy.git/{service}"),
            data.clone(),
            false,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), 2 * 1024 * 1024)
                .await
                .unwrap(),
            data
        );
    }
    let count = f.seen.lock().unwrap().len();
    let response = call(
        &f,
        "POST",
        "/api/github/repos/other/repo/releases",
        b"{}".to_vec(),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(f.seen.lock().unwrap().len(), count);
}

#[tokio::test]
async fn metadata_body_holds_capacity_until_consumed_or_dropped() {
    let f = fixture_with_limit(1).await;
    let first = call(
        &f,
        "GET",
        "/api/github/repos/class-undefined/gh-proxy/releases",
        vec![],
        false,
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let blocked = call(
        &f,
        "GET",
        "/api/github/repos/class-undefined/gh-proxy/releases",
        vec![],
        false,
    )
    .await;
    assert_eq!(blocked.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(first);
    let next = call(
        &f,
        "GET",
        "/api/github/repos/class-undefined/gh-proxy/releases",
        vec![],
        false,
    )
    .await;
    assert_eq!(next.status(), StatusCode::OK);
}

#[tokio::test]
async fn git_credentials_challenges_and_api_auth_failures_are_preserved() {
    let f = fixture().await;
    for auth in [
        None,
        Some("Basic aW52YWxpZDppbnZhbGlk"),
        Some("Basic Zml4dHVyZTp0b2tlbg=="),
    ] {
        let mut request = Request::builder()
            .uri("/class-undefined/gh-proxy.git/info/refs?service=git-upload-pack")
            .body(Body::empty())
            .unwrap();
        if let Some(auth) = auth {
            request
                .headers_mut()
                .insert("authorization", auth.parse().unwrap());
        }
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
        let response = f.app.clone().router().oneshot(request).await.unwrap();
        let accepted = auth == Some("Basic Zml4dHVyZTp0b2tlbg==");
        assert_eq!(
            response.status(),
            if accepted {
                StatusCode::OK
            } else {
                StatusCode::UNAUTHORIZED
            }
        );
        assert!(!response.headers().contains_key("x-gh-proxy-error"));
        if !accepted {
            assert_eq!(
                response
                    .headers()
                    .get_all("www-authenticate")
                    .iter()
                    .count(),
                2
            );
            assert_eq!(
                to_bytes(response.into_body(), 1024).await.unwrap(),
                "Authentication required"
            );
        }
    }
    for status in [401, 403] {
        let response = call(
            &f,
            "GET",
            &format!("/api/github/repos/class-undefined/gh-proxy/releases/{status}"),
            vec![],
            false,
        )
        .await;
        assert_eq!(response.status().as_u16(), status);
        assert!(!response.headers().contains_key("x-gh-proxy-error"));
        if status == 401 {
            assert!(response.headers().contains_key("www-authenticate"));
        } else {
            assert!(response.headers().contains_key("x-github-sso"));
        }
        // Plain-text upstream auth failures must not be parsed as successful JSON metadata.
        assert!(
            !to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn connect_proxy_auth_failure_is_diagnostic_and_redacted() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            bytes.push(socket.read_u8().await.unwrap());
        }
        assert!(
            String::from_utf8_lossy(&bytes)
                .to_lowercase()
                .contains("proxy-authorization: basic ")
        );
        socket
            .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });
    let egress = Egress::from_lookup(
        |k| (k == "HTTPS_PROXY").then(|| format!("http://fixture-user:fixture-secret@{addr}")),
        false,
        Duration::from_secs(2),
    )
    .unwrap();
    let config: Config = toml::from_str(include_str!("../tests/fixtures/gateway.toml")).unwrap();
    let mut request = Request::builder()
        .uri("/class-undefined/gh-proxy.git/info/refs?service=git-upload-pack")
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
    let response = App::new(Arc::new(config), Arc::new(egress))
        .router()
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        response.headers()["x-gh-proxy-error"],
        "upstream_proxy_auth_required"
    );
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("fixture-"));
    task.await.unwrap();
}
