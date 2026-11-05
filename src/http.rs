use crate::{
    config::Config,
    egress::Egress,
    policy::{self, Kind, Operation, Route},
};
use anyhow::{Result, ensure};
use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct App {
    control: Arc<crate::control::Control>,
    config: Arc<Config>,
    egress: Arc<Egress>,
    slots: Arc<Semaphore>,
    runtime: Arc<crate::web::Runtime>,
    sequence: Arc<AtomicU64>,
}
impl App {
    pub fn new(config: Arc<Config>, egress: Arc<Egress>) -> Self {
        Self::managed(crate::control::Control::readonly(config, egress))
    }
    pub fn managed(control: Arc<crate::control::Control>) -> Self {
        let snapshot = control.snapshot();
        Self {
            control,
            config: snapshot.config.clone(),
            egress: snapshot.egress.clone(),
            slots: snapshot.http_slots.clone(),
            runtime: Arc::new(crate::web::Runtime::new()),
            sequence: Arc::new(AtomicU64::new(1)),
        }
    }
    pub fn router(self) -> Router {
        Router::new().fallback(handle).with_state(self)
    }
}
fn nominated(input: &HeaderMap, key: &str) -> bool {
    input
        .get_all("connection")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .any(|s| s.trim().eq_ignore_ascii_case(key))
}
fn request_headers(input: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for key in [
        "authorization",
        "accept",
        "content-type",
        "content-length",
        "git-protocol",
        "range",
        "if-none-match",
        "if-modified-since",
        "if-range",
        "x-github-api-version",
    ] {
        if let Some(value) = input.get(key)
            && !nominated(input, key)
        {
            headers.insert(axum::http::HeaderName::from_static(key), value.clone());
        }
    }
    headers
}
fn response_headers(input: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for key in [
        "content-type",
        "content-length",
        "content-range",
        "accept-ranges",
        "etag",
        "last-modified",
        "cache-control",
        "expires",
        "vary",
        "x-ratelimit-limit",
        "x-ratelimit-remaining",
        "x-ratelimit-reset",
        "retry-after",
        "www-authenticate",
    ] {
        if let Some(value) = input.get(key)
            && !nominated(input, key)
        {
            headers.insert(axum::http::HeaderName::from_static(key), value.clone());
        }
    }
    headers
}
fn rewrite_url(value: &str, public: &str, repository: &str) -> String {
    for (from, to) in [
        ("https://api.github.com/", "api/github/"),
        ("https://uploads.github.com/", "uploads/github/"),
    ] {
        if let Some(rest) = value.strip_prefix(from) {
            return format!("{public}{to}{rest}");
        }
    }
    if let Some(rest) = value.strip_prefix("https://github.com/")
        && rest
            .to_ascii_lowercase()
            .starts_with(&format!("{repository}/releases/download/"))
    {
        return format!("{public}{rest}");
    }
    value.into()
}
fn rewrite_json(value: &mut serde_json::Value, public: &str, repository: &str) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "url" | "upload_url" | "browser_download_url" | "assets_url"
                ) {
                    if let Some(s) = value.as_str() {
                        *value = serde_json::Value::String(rewrite_url(s, public, repository));
                    }
                } else {
                    rewrite_json(value, public, repository);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                rewrite_json(value, public, repository);
            }
        }
        _ => {}
    }
}
async fn bounded_json(response: reqwest::Response) -> Result<serde_json::Value> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| anyhow::anyhow!("upstream body failed"))?;
        ensure!(
            bytes.len() + chunk.len() <= 8 * 1024 * 1024,
            "API metadata exceeds 8 MiB"
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid API JSON"))
}
async fn forward(app: &App, route: &Route, request: Request) -> Result<Response> {
    let permit = app
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| anyhow::anyhow!("request limit reached"))?;
    let deadline = tokio::time::Instant::now() + app.egress.timeout;
    let (parts, body) = request.into_parts();
    let mut headers = request_headers(&parts.headers);
    // Send only canonical media types; prevent content negotiation bypassing download policy.
    if route.kind == Kind::Api {
        headers.insert(
            "accept",
            HeaderValue::from_static(if route.operation == Operation::Download {
                "application/octet-stream"
            } else {
                "application/vnd.github+json"
            }),
        );
    }
    let client = app.egress.client(&route.target)?;
    let body = reqwest::Body::wrap_stream(
        body.into_data_stream()
            .map(|c| c.map_err(|_| std::io::Error::other("client body failed"))),
    );
    let mut response = client
        .request(parts.method.clone(), route.target.clone())
        .headers(headers.clone())
        .timeout(deadline.saturating_duration_since(tokio::time::Instant::now()))
        .body(body)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("upstream request failed"))?;
    let mut previous = route.target.clone();
    for hop in 0..=5 {
        if !response.status().is_redirection() || response.status() == StatusCode::NOT_MODIFIED {
            break;
        }
        ensure!(hop < 5, "too many redirects");
        let location = response
            .headers()
            .get("location")
            .and_then(|s| s.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("missing redirect location"))?;
        let next = policy::redirect_target(route, &previous, location)?;
        if previous.host_str() != next.host_str() {
            headers.remove("authorization");
        }
        let client = app.egress.client(&next)?;
        response = client
            .request(parts.method.clone(), next.clone())
            .headers(headers.clone())
            .timeout(deadline.saturating_duration_since(tokio::time::Instant::now()))
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("attachment request failed"))?;
        previous = next;
    }
    let status = response.status();
    let mut output_headers = response_headers(response.headers());
    let json = matches!(route.kind, Kind::Api | Kind::Upload)
        && route.operation != Operation::Download
        && parts.method != Method::HEAD
        && status != StatusCode::NO_CONTENT
        && status != StatusCode::NOT_MODIFIED;
    let body = if json {
        // JSON metadata is bounded; Git packs and attachments always stream.
        let link = response
            .headers()
            .get("link")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let mut value = bounded_json(response).await?;
        rewrite_json(
            &mut value,
            app.config.public_url.as_str(),
            &route.repository,
        );
        let bytes = serde_json::to_vec(&value)?;
        output_headers.remove("content-length");
        output_headers.remove("etag");
        output_headers.insert("content-type", HeaderValue::from_static("application/json"));
        if let Some(link) = link {
            let rewritten = link.replace(
                "https://api.github.com/",
                &format!("{}api/github/", app.config.public_url),
            );
            if let Ok(v) = HeaderValue::from_str(&rewritten) {
                output_headers.insert("link", v);
            }
        }
        Body::from_stream(
            futures_util::stream::iter([Ok::<_, std::io::Error>(axum::body::Bytes::from(bytes))])
                .map(move |chunk| {
                    let _keep_slot = &permit;
                    chunk
                }),
        )
    } else {
        Body::from_stream(response.bytes_stream().map(move |chunk| {
            let _keep_slot = &permit;
            chunk.map_err(|_| std::io::Error::other("upstream body failed"))
        }))
    };
    let mut out = Response::new(body);
    *out.status_mut() = status;
    *out.headers_mut() = output_headers;
    Ok(out)
}
async fn handle(
    State(mut app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    let current = app.control.snapshot();
    app.config = current.config.clone();
    app.egress = current.egress.clone();
    app.slots = current.http_slots.clone();
    let id = app.sequence.fetch_add(1, Ordering::Relaxed);
    let start = Instant::now();
    if !app.config.allows_client(peer.ip()) {
        return (StatusCode::FORBIDDEN, "client denied\n").into_response();
    }
    if request.uri().path() == "/_ui/config" {
        return crate::web::manage(request, peer.ip(), &app.control).await;
    }
    if let Some(response) = crate::web::respond(
        request.uri().path(),
        request.method(),
        request
            .uri()
            .authority()
            .map(|host| host.as_str())
            .or_else(|| {
                request
                    .headers()
                    .get("host")
                    .and_then(|host| host.to_str().ok())
            }),
        &app.config,
        &app.egress,
        &app.runtime,
        app.control.enabled(),
    ) {
        return response;
    }
    if matches!(request.uri().path(), "/health/live" | "/health/ready") {
        if request.method() != Method::GET {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let ready = app.egress.http_available()
            && (app.config.ssh_targets.is_empty() || app.egress.ssh_available());
        return if request.uri().path() == "/health/live" || ready {
            (StatusCode::OK, "ok\n").into_response()
        } else {
            (StatusCode::SERVICE_UNAVAILABLE, "egress not configured\n").into_response()
        };
    }
    app.runtime.requests.fetch_add(1, Ordering::Relaxed);
    let binary = request
        .headers()
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("application/octet-stream"));
    // HTTP/2 reconstructs URI scheme/authority from mandatory pseudo-headers.
    // Only HTTP/1 absolute-form is a proxy request; all upstream origins stay fixed.
    let policy_uri = if request.version() == axum::http::Version::HTTP_2 {
        match request
            .uri()
            .path_and_query()
            .map(|p| p.as_str().parse::<axum::http::Uri>())
        {
            Some(Ok(uri)) => uri,
            _ => return StatusCode::FORBIDDEN.into_response(),
        }
    } else {
        request.uri().clone()
    };
    let route = match policy::github(&app.config, request.method(), &policy_uri, binary) {
        Ok(route) => route,
        Err(_) => {
            app.runtime.denied.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(request_id = id, entry = "http", result = "policy_denied");
            return (StatusCode::FORBIDDEN, "request denied\n").into_response();
        }
    };
    let response = match forward(&app, &route, request).await {
        Ok(response) => response,
        Err(_) => {
            app.runtime.failed.fetch_add(1, Ordering::Relaxed);
            (
                StatusCode::BAD_GATEWAY,
                "upstream unavailable or response rejected\n",
            )
                .into_response()
        }
    };
    tracing::info!(request_id=id,entry="http",repository=%route.repository,operation=?route.operation,status=response.status().as_u16(),elapsed_ms=start.elapsed().as_millis() as u64);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    fn app() -> App {
        let c: Config = toml::from_str(include_str!("../tests/fixtures/gateway.toml")).unwrap();
        let e = Egress::from_lookup(|_| None, false, std::time::Duration::from_secs(1)).unwrap();
        App::new(Arc::new(c), Arc::new(e))
    }
    async fn call(ip: &str, path: &str) -> Response {
        let mut request = Request::builder().uri(path).body(Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(ip.parse::<SocketAddr>().unwrap()));
        app().router().oneshot(request).await.unwrap()
    }
    #[tokio::test]
    async fn ingress_acl_health_and_default_denial() {
        assert_eq!(
            call("10.1.1.1:1234", "/health/live").await.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call("127.0.0.1:1234", "/health/live").await.status(),
            StatusCode::OK
        );
        assert_eq!(
            call("127.0.0.1:1234", "/health/ready").await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            call("127.0.0.1:1234", "/other/repo.git/git-upload-pack")
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                "127.0.0.1:1234",
                "/class-undefined/gh-proxy.git/info/refs?service=git-upload-pack"
            )
            .await
            .status(),
            StatusCode::BAD_GATEWAY
        );
    }
    #[tokio::test]
    async fn http2_pseudo_headers_are_not_absolute_proxy_requests() {
        let mut request = Request::builder().version(axum::http::Version::HTTP_2)
            .uri("https://gateway.example/class-undefined/gh-proxy.git/info/refs?service=git-upload-pack")
            .body(Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
        assert_eq!(
            app().router().oneshot(request).await.unwrap().status(),
            StatusCode::BAD_GATEWAY
        );
        let response = call("127.0.0.1:1234","https://gateway.example/class-undefined/gh-proxy.git/info/refs?service=git-upload-pack").await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    #[test]
    fn rewrite_release_links_not_user_content() {
        let mut v = serde_json::json!({"upload_url":"https://uploads.github.com/repos/a/b/releases/1/assets{?name,label}","assets":[{"browser_download_url":"https://github.com/a/b/releases/download/v1/file"}],"body":"https://api.github.com/repos/a/b"});
        rewrite_json(&mut v, "https://gateway.example/", "a/b");
        assert_eq!(
            v["upload_url"],
            "https://gateway.example/uploads/github/repos/a/b/releases/1/assets{?name,label}"
        );
        assert_eq!(
            v["assets"][0]["browser_download_url"],
            "https://gateway.example/a/b/releases/download/v1/file"
        );
        assert_eq!(v["body"], "https://api.github.com/repos/a/b");
    }
    #[test]
    fn credentials_and_hop_headers_are_filtered() {
        let mut h = HeaderMap::new();
        h.insert("proxy-authorization", HeaderValue::from_static("secret"));
        h.insert("connection", HeaderValue::from_static("keep-alive"));
        h.insert("set-cookie", HeaderValue::from_static("secret"));
        assert!(request_headers(&h).is_empty());
        assert!(response_headers(&h).is_empty());
    }
}
