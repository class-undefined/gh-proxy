//! Read-only control surface. Only an explicit projection of configuration is serialized.
use crate::{config::Config, egress::Egress};
use axum::{
    Json,
    body::Body,
    http::{HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

pub(crate) struct Runtime {
    started: Instant,
    pub requests: AtomicU64,
    pub denied: AtomicU64,
    pub failed: AtomicU64,
}
impl Runtime {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            requests: AtomicU64::new(0),
            denied: AtomicU64::new(0),
            failed: AtomicU64::new(0),
        }
    }
    fn snapshot(&self, config: &Config, egress: &Egress, management: bool) -> Value {
        json!({
            "version": env!("CARGO_PKG_VERSION"), "management_enabled":management,
            "public_url": config.public_url.as_str(),
            "uptime_seconds": self.started.elapsed().as_secs(),
            "ready": egress.http_available() && (config.ssh_targets.is_empty() || egress.ssh_available()),
            "http_ready": egress.http_available(),
            "ssh_ready": egress.ssh_available(),
            "direct": config.direct,
            "tls": config.public_url.scheme() == "https",
            "socks_port": config.socks_listen.port(),
            "max_connections": config.max_connections,
            "timeout_seconds": config.timeout_seconds,
            "requests": self.requests.load(Ordering::Relaxed),
            "denied": self.denied.load(Ordering::Relaxed),
            "failed": self.failed.load(Ordering::Relaxed),
            "clients": config.clients.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "ssh_targets": config.ssh_targets.iter().map(|t| json!({"host": t.host, "port": t.port})).collect::<Vec<_>>(),
            "repositories": config.repositories.iter().map(|r| json!({
                "name": r.name, "write": r.write,
                "release": {"query": r.release.query, "create": r.release.create,
                    "update": r.release.update, "upload": r.release.upload,
                    "download": r.release.download, "delete": r.release.delete}
            })).collect::<Vec<_>>()
        })
    }
}
fn secured(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("content-security-policy", HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"));
    response
}
fn host_matches(authority: Option<&str>, config: &Config) -> bool {
    authority
        .filter(|host| !host.contains('@'))
        .and_then(|host| host.parse::<axum::http::uri::Authority>().ok())
        .is_some_and(|host| {
            host.host().trim_matches(['[', ']']).eq_ignore_ascii_case(
                config
                    .public_url
                    .host_str()
                    .unwrap_or("")
                    .trim_matches(['[', ']']),
            ) && host
                .port_u16()
                .unwrap_or(if config.public_url.scheme() == "https" {
                    443
                } else {
                    80
                })
                == config.public_url.port_or_known_default().unwrap_or(443)
        })
}
/// Called only after the shared source ACL. No mutations, proxy URLs or file paths are exposed.
pub(crate) fn respond(
    path: &str,
    method: &Method,
    authority: Option<&str>,
    config: &Config,
    egress: &Egress,
    runtime: &Runtime,
    management: bool,
) -> Option<Response> {
    if path != "/" && path != "/ui" && path != "/favicon.ico" && !path.starts_with("/_ui/") {
        return None;
    }
    // A local source ACL alone cannot prevent DNS rebinding to a loopback gateway.
    let valid_host = host_matches(authority, config);
    if !valid_host {
        return Some(secured(StatusCode::NOT_FOUND.into_response()));
    }
    if !config.web_enabled {
        return Some(secured(StatusCode::NOT_FOUND.into_response()));
    }
    if !matches!(*method, Method::GET | Method::HEAD) {
        return Some(secured(StatusCode::METHOD_NOT_ALLOWED.into_response()));
    }
    let mut response = match path {
        "/favicon.ico" => StatusCode::NO_CONTENT.into_response(),
        "/_ui/icon.svg" => (
            [("content-type", "image/svg+xml")],
            include_str!("../web/icon.svg"),
        )
            .into_response(),
        "/" | "/ui" => (
            [("content-type", "text/html; charset=utf-8")],
            include_str!("../web/index.html"),
        )
            .into_response(),
        "/_ui/style.css" => (
            [("content-type", "text/css; charset=utf-8")],
            include_str!("../web/style.css"),
        )
            .into_response(),
        "/_ui/app.js" => (
            [("content-type", "text/javascript; charset=utf-8")],
            include_str!("../web/app.js"),
        )
            .into_response(),
        "/_ui/view-model.js" => (
            [("content-type", "text/javascript; charset=utf-8")],
            include_str!("../web/view-model.js"),
        )
            .into_response(),
        "/_ui/status" => Json(runtime.snapshot(config, egress, management)).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    };
    if *method == Method::HEAD {
        *response.body_mut() = Body::empty();
    }
    Some(secured(response))
}

pub(crate) async fn manage(
    request: axum::extract::Request,
    peer: std::net::IpAddr,
    control: &Arc<crate::control::Control>,
) -> Response {
    use crate::control::{SaveError, Update};
    use axum::body::to_bytes;
    let current = control.snapshot();
    let config = &current.config;
    let authority = request
        .uri()
        .authority()
        .map(|v| v.as_str())
        .or_else(|| request.headers().get("host").and_then(|v| v.to_str().ok()));
    let error = |status: StatusCode, message: &str| {
        secured((status, Json(json!({"message":message}))).into_response())
    };
    if !config.web_enabled || !control.enabled() || !host_matches(authority, config) {
        return error(StatusCode::NOT_FOUND, "设置不可用");
    }
    if !control.authorized(
        request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
    ) {
        return error(StatusCode::UNAUTHORIZED, "管理密钥无效");
    }
    if request
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|v| v == "cross-site")
    {
        return error(StatusCode::FORBIDDEN, "请求来源无效");
    }
    match *request.method() {
        Method::GET => secured(Json(control.document().await).into_response()),
        Method::PUT => {
            let origin = config.public_url.origin().ascii_serialization();
            if request
                .headers()
                .get("origin")
                .and_then(|v| v.to_str().ok())
                != Some(&origin)
                || request
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .is_none_or(|s| s.split(';').next() != Some("application/json"))
            {
                return error(StatusCode::FORBIDDEN, "请求来源或格式无效");
            }
            let Ok(Ok(bytes)) = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                to_bytes(request.into_body(), 1024 * 1024),
            )
            .await
            else {
                return error(StatusCode::BAD_REQUEST, "配置内容过大或读取超时");
            };
            let Ok(update) = serde_json::from_slice::<Update>(&bytes) else {
                return error(StatusCode::BAD_REQUEST, "配置格式无效");
            };
            match control.save(update, peer).await {
                Ok(()) => secured(Json(control.document().await).into_response()),
                Err(SaveError::Conflict) => {
                    error(StatusCode::CONFLICT, "配置已被修改，请重新加载后再保存")
                }
                Err(SaveError::Invalid(message)) => error(StatusCode::BAD_REQUEST, &message),
                Err(SaveError::Storage) => error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "无法保存，请检查配置目录的写入权限",
                ),
                Err(SaveError::Disabled) => error(StatusCode::NOT_FOUND, "设置不可用"),
            }
        }
        _ => error(StatusCode::METHOD_NOT_ALLOWED, "不支持的操作"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::App;
    use axum::{body::to_bytes, extract::ConnectInfo, http::Request};
    use std::{net::SocketAddr, sync::Arc, time::Duration};
    use tower::ServiceExt;
    fn app(enabled: bool) -> App {
        let mut config: Config = toml::from_str(include_str!("../config/example.toml")).unwrap();
        config.web_enabled = enabled;
        let egress = Egress::from_lookup(
            |key| match key {
                "HTTPS_PROXY" => Some("http://proxyuser:proxysecret@127.0.0.1:9".into()),
                "ALL_PROXY" => Some("socks5h://sshuser:sshsecret@127.0.0.1:9".into()),
                _ => None,
            },
            false,
            Duration::from_secs(1),
        )
        .unwrap();
        App::new(Arc::new(config), Arc::new(egress))
    }
    async fn call(app: App, method: Method, path: &str, ip: &str, host: &str) -> Response {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", host)
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(ip.parse::<SocketAddr>().unwrap()));
        app.router().oneshot(request).await.unwrap()
    }
    #[tokio::test]
    async fn ui_assets_share_acl_and_only_allow_reads() {
        for path in [
            "/",
            "/ui",
            "/_ui/style.css",
            "/_ui/icon.svg",
            "/_ui/app.js",
            "/_ui/view-model.js",
            "/_ui/status",
        ] {
            assert_eq!(
                call(app(true), Method::GET, path, "10.0.0.1:9", "127.0.0.1:8080")
                    .await
                    .status(),
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                call(
                    app(false),
                    Method::GET,
                    path,
                    "127.0.0.1:9",
                    "127.0.0.1:8080"
                )
                .await
                .status(),
                StatusCode::NOT_FOUND
            );
            assert_eq!(
                call(
                    app(true),
                    Method::POST,
                    path,
                    "127.0.0.1:9",
                    "127.0.0.1:8080"
                )
                .await
                .status(),
                StatusCode::METHOD_NOT_ALLOWED
            );
            assert_eq!(
                call(
                    app(true),
                    Method::GET,
                    path,
                    "127.0.0.1:9",
                    "attacker.example:8080"
                )
                .await
                .status(),
                StatusCode::NOT_FOUND
            );
            let response = call(
                app(true),
                Method::GET,
                path,
                "127.0.0.1:9",
                "127.0.0.1:8080",
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            assert!(
                response.headers()["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .contains("script-src 'self'")
            );
        }
        let response = call(
            app(true),
            Method::HEAD,
            "/",
            "127.0.0.1:9",
            "127.0.0.1:8080",
        )
        .await;
        assert!(
            to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .is_empty()
        );
    }
    #[tokio::test]
    async fn status_is_redacted_and_polling_does_not_inflate_proxy_metrics() {
        let app = app(true);
        let response = call(
            app.clone(),
            Method::GET,
            "/_ui/status",
            "127.0.0.1:9",
            "127.0.0.1:8080",
        )
        .await;
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        for forbidden in [
            "proxyuser",
            "proxysecret",
            "sshuser",
            "sshsecret",
            "certificate",
            "private.key",
            "HTTP_PROXY",
        ] {
            assert!(!text.contains(forbidden), "unexpected field {forbidden}");
        }
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["repositories"][0]["name"], "class-undefined/gh-proxy");
        assert_eq!(value["requests"], 0);
        let _ = call(
            app.clone(),
            Method::GET,
            "/other/repo.git/info/refs?service=git-upload-pack",
            "127.0.0.1:9",
            "127.0.0.1:8080",
        )
        .await;
        let _ = call(
            app.clone(),
            Method::GET,
            "/health/live",
            "127.0.0.1:9",
            "127.0.0.1:8080",
        )
        .await;
        let response = call(
            app,
            Method::GET,
            "/_ui/status",
            "127.0.0.1:9",
            "127.0.0.1:8080",
        )
        .await;
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(value["requests"], 1);
        assert_eq!(value["denied"], 1);
    }
}
