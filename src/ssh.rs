use crate::{
    config::Config,
    egress::{Egress, public_ip},
    fault::Fault,
};
use anyhow::{Result, bail, ensure};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinSet,
    time::timeout,
};

async fn reply(stream: &mut TcpStream, status: u8) -> Result<()> {
    stream
        .write_all(&[5, status, 0, 1, 0, 0, 0, 0, 0, 0])
        .await?;
    Ok(())
}
async fn request(stream: &mut TcpStream) -> Result<(String, u16)> {
    ensure!(stream.read_u8().await? == 5, Fault::SocksRequestRejected);
    let count = stream.read_u8().await? as usize;
    ensure!(count > 0, Fault::SocksRequestRejected);
    let mut methods = vec![0; count];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        stream.write_all(&[5, 255]).await?;
        bail!(Fault::SocksRequestRejected);
    }
    stream.write_all(&[5, 0]).await?;
    let mut head = [0; 4];
    stream.read_exact(&mut head).await?;
    if head != [5, 1, 0, 3] {
        reply(stream, 7).await?;
        bail!(Fault::SocksRequestRejected);
    }
    let len = stream.read_u8().await? as usize;
    ensure!(len > 0, Fault::SocksRequestRejected);
    let mut domain = vec![0; len];
    stream.read_exact(&mut domain).await?;
    let domain = String::from_utf8(domain)
        .map_err(|_| Fault::SocksRequestRejected)?
        .to_ascii_lowercase();
    let port = stream.read_u16().await?;
    Ok((domain, port))
}
async fn resolve_public(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    let addrs: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| Fault::DnsFailed)?
        .collect();
    ensure!(
        !addrs.is_empty() && addrs.iter().all(|a| public_ip(a.ip())),
        Fault::DnsRejected
    );
    Ok(addrs)
}
async fn proxy_connect(egress: &Egress, host: &str, port: u16) -> Result<TcpStream> {
    let Some(proxy) = &egress.all else {
        ensure!(egress.direct, Fault::EgressNotConfigured);
        let addrs = resolve_public(host, port).await?;
        return Ok(TcpStream::connect(addrs.as_slice())
            .await
            .map_err(|_| Fault::ConnectFailed)?);
    };
    let proxy_host = proxy
        .host_str()
        .ok_or_else(|| anyhow::anyhow!(Fault::InvalidProxy))?
        .trim_matches(['[', ']']);
    let proxy_port = proxy
        .port()
        .ok_or_else(|| anyhow::anyhow!(Fault::InvalidProxy))?;
    let mut stream = TcpStream::connect((proxy_host, proxy_port))
        .await
        .map_err(|_| Fault::ConnectFailed)?;
    let has_auth = !proxy.username().is_empty() || proxy.password().is_some();
    stream
        .write_all(if has_auth { &[5, 1, 2] } else { &[5, 1, 0] })
        .await?;
    let mut greeting = [0; 2];
    stream.read_exact(&mut greeting).await?;
    ensure!(
        greeting == [5, if has_auth { 2 } else { 0 }],
        Fault::ProxyAuthRequired
    );
    if has_auth {
        let user = percent_encoding::percent_decode_str(proxy.username())
            .decode_utf8()
            .map_err(|_| Fault::InvalidProxy)?;
        let password = percent_encoding::percent_decode_str(proxy.password().unwrap_or(""))
            .decode_utf8()
            .map_err(|_| Fault::InvalidProxy)?;
        ensure!(
            user.len() <= 255 && password.len() <= 255,
            Fault::InvalidProxy
        );
        let mut auth = vec![1, user.len() as u8];
        auth.extend_from_slice(user.as_bytes());
        auth.push(password.len() as u8);
        auth.extend_from_slice(password.as_bytes());
        stream.write_all(&auth).await?;
        stream.read_exact(&mut greeting).await?;
        ensure!(greeting == [1, 0], Fault::ProxyAuthRequired);
    }
    let mut connect = vec![5, 1, 0];
    if proxy.scheme() == "socks5h" {
        ensure!(host.len() <= 255, Fault::SocksRequestRejected);
        connect.extend_from_slice(&[3, host.len() as u8]);
        connect.extend_from_slice(host.as_bytes());
    } else {
        let addr = resolve_public(host, port)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!(Fault::DnsFailed))?;
        match addr.ip() {
            IpAddr::V4(ip) => {
                connect.push(1);
                connect.extend_from_slice(&ip.octets());
            }
            IpAddr::V6(ip) => {
                connect.push(4);
                connect.extend_from_slice(&ip.octets());
            }
        }
    }
    connect.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&connect).await?;
    let mut head = [0; 4];
    stream.read_exact(&mut head).await?;
    ensure!(
        head[0] == 5 && head[1] == 0 && head[2] == 0,
        Fault::SocksUpstreamRejected
    );
    let len = match head[3] {
        1 => 4,
        4 => 16,
        3 => stream.read_u8().await? as usize,
        _ => bail!(Fault::SocksUpstreamRejected),
    };
    let mut rest = vec![0; len + 2];
    stream.read_exact(&mut rest).await?;
    Ok(stream)
}
async fn handle(mut client: TcpStream, config: Arc<Config>, egress: Arc<Egress>) -> Result<()> {
    let (host, port) = timeout(Duration::from_secs(10), request(&mut client))
        .await
        .map_err(|_| Fault::Timeout)??;
    if !config
        .ssh_targets
        .iter()
        .any(|t| t.host == host && t.port == port)
    {
        reply(&mut client, 2).await?;
        bail!(Fault::SshTargetDenied);
    }
    let upstream = timeout(Duration::from_secs(15), proxy_connect(&egress, &host, port)).await;
    let mut upstream = match upstream {
        Ok(Ok(stream)) => stream,
        failed => {
            let fault = match failed {
                Ok(Err(error)) => error
                    .downcast_ref::<Fault>()
                    .copied()
                    .unwrap_or(Fault::SocksUpstreamRejected),
                Err(_) => Fault::Timeout,
                Ok(Ok(_)) => unreachable!(),
            };
            let _ = reply(&mut client, 1).await;
            bail!(fault);
        }
    };
    reply(&mut client, 0).await?;
    tracing::info!(entry = "ssh", event = "tunnel_established");
    timeout(
        egress.timeout,
        tokio::io::copy_bidirectional(&mut client, &mut upstream),
    )
    .await
    .map_err(|_| Fault::Timeout)??;
    Ok(())
}
pub async fn serve(
    listener: TcpListener,
    config: Arc<Config>,
    egress: Arc<Egress>,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    serve_managed(
        listener,
        crate::control::Control::readonly(config, egress),
        shutdown,
    )
    .await
}
pub async fn serve_managed(
    listener: TcpListener,
    control: Arc<crate::control::Control>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let current=control.snapshot();
                let config=&current.config; let egress=&current.egress;
                if !config.allows_client(peer.ip()) { tracing::warn!(entry="ssh", result="source_denied"); continue; }
                let Ok(permit) = current.ssh_slots.clone().try_acquire_owned() else { continue; };
                let config = config.clone(); let egress = egress.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    match handle(stream, config, egress).await {
                        Ok(()) => tracing::info!(entry="ssh", event="tunnel_closed"),
                        Err(error) => {
                            let fault = error.downcast_ref::<Fault>().copied().unwrap_or(Fault::SshIoFailed);
                            tracing::warn!(entry="ssh", event="tunnel_failed", error_code=fault.code());
                        }
                    }
                });
            }
        }
    }
    if timeout(Duration::from_secs(10), async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tasks.abort_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn socks_proxy_credentials_and_authentication_rejection() {
        for accepted in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut greeting = [0; 3];
                s.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting, [5, 1, 2]);
                s.write_all(&[5, 2]).await.unwrap();
                assert_eq!(s.read_u8().await.unwrap(), 1);
                let mut user = vec![0; s.read_u8().await.unwrap() as usize];
                s.read_exact(&mut user).await.unwrap();
                let mut password = vec![0; s.read_u8().await.unwrap() as usize];
                s.read_exact(&mut password).await.unwrap();
                assert_eq!(user, b"user@fixture");
                assert_eq!(password, b"pass:fixture");
                s.write_all(&[1, if accepted { 0 } else { 1 }])
                    .await
                    .unwrap();
                if accepted {
                    let mut request = [0; 17];
                    s.read_exact(&mut request).await.unwrap();
                    assert_eq!(&request[5..15], b"github.com");
                    s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
                    s.write_all(b"SSH-2.0-fixture\r\n").await.unwrap();
                }
            });
            let egress = Egress::from_lookup(
                |k| {
                    (k == "ALL_PROXY")
                        .then(|| format!("socks5h://user%40fixture:pass%3Afixture@{addr}"))
                },
                false,
                Duration::from_secs(1),
            )
            .unwrap();
            let result = proxy_connect(&egress, "github.com", 22).await;
            if accepted {
                let mut body = Vec::new();
                result.unwrap().read_to_end(&mut body).await.unwrap();
                assert_eq!(body, b"SSH-2.0-fixture\r\n");
            } else {
                let error = result.unwrap_err();
                assert_eq!(
                    error.downcast_ref::<Fault>(),
                    Some(&Fault::ProxyAuthRequired)
                );
                assert!(!error.to_string().contains("fixture"));
            }
            task.await.unwrap();
        }
        let egress = Egress::from_lookup(|_| None, false, Duration::from_secs(1)).unwrap();
        let error = proxy_connect(&egress, "github.com", 22).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<Fault>(),
            Some(&Fault::EgressNotConfigured)
        );
    }
    #[tokio::test]
    async fn remote_dns_and_transparent_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut head = [0; 3];
            s.read_exact(&mut head).await.unwrap();
            assert_eq!(head, [5, 1, 0]);
            s.write_all(&[5, 0]).await.unwrap();
            let mut head = [0; 5];
            s.read_exact(&mut head).await.unwrap();
            assert_eq!(head, [5, 1, 0, 3, 10]);
            let mut domain = [0; 10];
            s.read_exact(&mut domain).await.unwrap();
            assert_eq!(&domain, b"github.com");
            assert_eq!(s.read_u16().await.unwrap(), 22);
            s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
            s.write_all(b"SSH-2.0-test\r\n").await.unwrap();
        });
        let e = Egress::from_lookup(
            |k| (k == "ALL_PROXY").then(|| format!("socks5h://{addr}")),
            false,
            Duration::from_secs(1),
        )
        .unwrap();
        let mut s = proxy_connect(&e, "github.com", 22).await.unwrap();
        let mut bytes = Vec::new();
        s.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"SSH-2.0-test\r\n");
        task.await.unwrap();
    }
    #[tokio::test]
    async fn local_dns_rejects_reserved_destination() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut head = [0; 3];
            s.read_exact(&mut head).await.unwrap();
            s.write_all(&[5, 0]).await.unwrap();
            let mut rest = Vec::new();
            s.read_to_end(&mut rest).await.unwrap();
            assert!(rest.is_empty(), "reserved address sent to upstream");
        });
        let e = Egress::from_lookup(
            |k| (k == "ALL_PROXY").then(|| format!("socks5://{addr}")),
            false,
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(proxy_connect(&e, "localhost", 22).await.is_err());
        task.await.unwrap();
    }
    #[tokio::test]
    async fn target_denial_and_source_acl_at_listener() {
        let config: Config =
            toml::from_str(include_str!("../tests/fixtures/gateway.toml")).unwrap();
        let config = Arc::new(config);
        let e = Arc::new(Egress::from_lookup(|_| None, false, Duration::from_secs(1)).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(serve(listener, config, e, stopped));
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(&[5, 1, 0]).await.unwrap();
        let mut greeting = [0; 2];
        s.read_exact(&mut greeting).await.unwrap();
        s.write_all(&[5, 1, 0, 3, 8]).await.unwrap();
        s.write_all(b"evil.com").await.unwrap();
        s.write_u16(22).await.unwrap();
        let mut reply = [0; 10];
        s.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 2);
        drop(s);
        stop.send(true).unwrap();
        task.await.unwrap().unwrap();
        let mut config: Config =
            toml::from_str(include_str!("../tests/fixtures/gateway.toml")).unwrap();
        config.clients = vec!["192.0.2.0/24".parse().unwrap()];
        let e = Arc::new(Egress::from_lookup(|_| None, false, Duration::from_secs(1)).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(serve(listener, Arc::new(config), e, stopped));
        let mut s = TcpStream::connect(addr).await.unwrap();
        let mut byte = [0; 1];
        assert!(matches!(s.read(&mut byte).await, Ok(0) | Err(_)));
        stop.send(true).unwrap();
        task.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn downstream_rejects_ip_and_other_commands() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            assert!(request(&mut s).await.is_err());
        });
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(&[5, 1, 0]).await.unwrap();
        let mut greeting = [0; 2];
        s.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 0]);
        s.write_all(&[5, 1, 0, 1, 127, 0, 0, 1, 0, 22])
            .await
            .unwrap();
        let mut reply = [0; 10];
        s.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 7);
        task.await.unwrap();
    }
}
