//! Public diagnostics contain fixed text only, never raw transport error chains or URLs.
use axum::http::StatusCode;
use std::fmt;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    EgressNotConfigured,
    DirectBypassDenied,
    UnsupportedProtocol,
    InvalidProxy,
    TransportInit,
    Busy,
    Timeout,
    ProxyAuthRequired,
    DnsRejected,
    DnsFailed,
    TlsFailed,
    ConnectFailed,
    BodyFailed,
    InvalidResponse,
    RedirectDenied,
    RedirectLimit,
    ClientBodyFailed,
    SshTargetDenied,
    SocksRequestRejected,
    SocksUpstreamRejected,
    SshIoFailed,
}
impl Fault {
    pub fn code(self) -> &'static str {
        match self {
            Self::EgressNotConfigured => "egress_not_configured",
            Self::DirectBypassDenied => "direct_bypass_denied",
            Self::UnsupportedProtocol => "unsupported_protocol",
            Self::InvalidProxy => "invalid_proxy",
            Self::TransportInit => "transport_init_failed",
            Self::Busy => "connection_limit_reached",
            Self::Timeout => "upstream_timeout",
            Self::ProxyAuthRequired => "upstream_proxy_auth_required",
            Self::DnsRejected => "target_dns_rejected",
            Self::DnsFailed => "dns_lookup_failed",
            Self::TlsFailed => "upstream_tls_failed",
            Self::ConnectFailed => "upstream_connect_failed",
            Self::BodyFailed => "upstream_body_failed",
            Self::InvalidResponse => "upstream_response_invalid",
            Self::RedirectDenied => "redirect_denied",
            Self::RedirectLimit => "redirect_limit",
            Self::ClientBodyFailed => "client_body_failed",
            Self::SshTargetDenied => "ssh_target_denied",
            Self::SocksRequestRejected => "socks_request_rejected",
            Self::SocksUpstreamRejected => "socks_upstream_rejected",
            Self::SshIoFailed => "ssh_io_failed",
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::EgressNotConfigured => {
                "未配置适用的上游代理，且禁止直连；请在控制台设置上游代理。"
            }
            Self::DirectBypassDenied => {
                "NO_PROXY 命中，但 direct=false；请检查绕过规则或明确允许直连。"
            }
            Self::UnsupportedProtocol => "目标协议不受支持。",
            Self::InvalidProxy => "上游代理配置无效。",
            Self::TransportInit => "无法初始化出站连接。",
            Self::Busy => "转发并发已达上限，请稍后重试。",
            Self::Timeout => "上游连接或传输超时。",
            Self::ProxyAuthRequired => {
                "上游代理认证失败；请检查代理用户名和密码，GitHub Token 不能用于代理认证。"
            }
            Self::DnsRejected => {
                "目标 DNS 返回内网或保留地址；Fake-IP 环境请使用 HTTP CONNECT 或 socks5h 上游。"
            }
            Self::DnsFailed => "无法解析上游地址。",
            Self::TlsFailed => "上游 TLS 校验或握手失败。",
            Self::ConnectFailed => "无法连接上游代理或平台，请检查代理地址和端口。",
            Self::BodyFailed => "上游响应传输中断。",
            Self::InvalidResponse => "上游返回了无效响应。",
            Self::RedirectDenied => "上游重定向被策略拒绝；请检查仓库地址和允许的目标。",
            Self::RedirectLimit => "上游重定向次数超过限制。",
            Self::ClientBodyFailed => "客户端请求体传输失败。",
            Self::SshTargetDenied => "SSH 目标域名或端口未获授权。",
            Self::SocksRequestRejected => "不支持的 SOCKS 请求。",
            Self::SocksUpstreamRejected => "上游 SOCKS 握手或连接被拒绝。",
            Self::SshIoFailed => "SSH 隧道连接中断。",
        }
    }
    pub fn status(self) -> StatusCode {
        match self {
            Self::EgressNotConfigured | Self::DirectBypassDenied | Self::Busy => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            Self::Timeout => StatusCode::GATEWAY_TIMEOUT,
            Self::ClientBodyFailed => StatusCode::BAD_REQUEST,
            _ => StatusCode::BAD_GATEWAY,
        }
    }
    pub fn transport(error: reqwest::Error) -> Self {
        use std::error::Error;
        if error.is_timeout() {
            return Self::Timeout;
        }
        // Classify library failures internally; none of these strings leave this function.
        let error = error.without_url();
        let mut source = error.source();
        while let Some(cause) = source {
            let text = cause.to_string().to_ascii_lowercase();
            if text.contains("proxy authorization required")
                || text.contains("proxy authentication required")
            {
                return Self::ProxyAuthRequired;
            }
            if text.contains("target dns rejected") {
                return Self::DnsRejected;
            }
            if text.contains("certificate") || text.contains("tls handshake") {
                return Self::TlsFailed;
            }
            if text.contains("dns error")
                || text.contains("failed to lookup")
                || text.contains("name or service not known")
            {
                return Self::DnsFailed;
            }
            if text.contains("client body failed") {
                return Self::ClientBodyFailed;
            }
            source = cause.source();
        }
        Self::ConnectFailed
    }
}
impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}
impl std::error::Error for Fault {}
