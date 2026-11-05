use crate::{
    config::Config,
    egress::{Egress, public_ip},
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
    sync::{Semaphore, watch},
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
    ensure!(stream.read_u8().await? == 5, "unsupported SOCKS version");
    let count = stream.read_u8().await? as usize;
    ensure!(count > 0, "missing authentication methods");
    let mut methods = vec![0; count];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        stream.write_all(&[5, 255]).await?;
        bail!("no supported authentication method");
    }
    stream.write_all(&[5, 0]).await?;
    let mut head = [0; 4];
    stream.read_exact(&mut head).await?;
    if head != [5, 1, 0, 3] {
        reply(stream, 7).await?;
        bail!("only domain CONNECT is supported");
    }
    let len = stream.read_u8().await? as usize;
    ensure!(len > 0, "empty target");
    let mut domain = vec![0; len];
    stream.read_exact(&mut domain).await?;
    let domain = String::from_utf8(domain)?.to_ascii_lowercase();
    let port = stream.read_u16().await?;
    Ok((domain, port))
}
async fn resolve_public(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    let addrs: Vec<_> = tokio::net::lookup_host((host, port)).await?.collect();
    ensure!(
        !addrs.is_empty() && addrs.iter().all(|a| public_ip(a.ip())),
        "target resolution rejected"
    );
    Ok(addrs)
}
async fn proxy_connect(egress: &Egress, host: &str, port: u16) -> Result<TcpStream> {
    let Some(proxy) = &egress.all else {
        ensure!(egress.direct, "direct egress denied");
        let addrs = resolve_public(host, port).await?;
        return Ok(TcpStream::connect(addrs.as_slice()).await?);
    };
    let proxy_host = proxy
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("missing proxy host"))?
        .trim_matches(['[', ']']);
    let proxy_port = proxy
        .port()
        .ok_or_else(|| anyhow::anyhow!("missing proxy port"))?;
    let mut stream = TcpStream::connect((proxy_host, proxy_port)).await?;
    let has_auth = !proxy.username().is_empty() || proxy.password().is_some();
    stream
        .write_all(if has_auth { &[5, 1, 2] } else { &[5, 1, 0] })
        .await?;
    let mut greeting = [0; 2];
    stream.read_exact(&mut greeting).await?;
    ensure!(
        greeting == [5, if has_auth { 2 } else { 0 }],
        "upstream SOCKS authentication rejected"
    );
    if has_auth {
        let user = percent_encoding::percent_decode_str(proxy.username()).decode_utf8()?;
        let password =
            percent_encoding::percent_decode_str(proxy.password().unwrap_or("")).decode_utf8()?;
        ensure!(
            user.len() <= 255 && password.len() <= 255,
            "proxy credentials too long"
        );
        let mut auth = vec![1, user.len() as u8];
        auth.extend_from_slice(user.as_bytes());
        auth.push(password.len() as u8);
        auth.extend_from_slice(password.as_bytes());
        stream.write_all(&auth).await?;
        stream.read_exact(&mut greeting).await?;
        ensure!(greeting == [1, 0], "upstream SOCKS authentication rejected");
    }
    let mut connect = vec![5, 1, 0];
    if proxy.scheme() == "socks5h" {
        ensure!(host.len() <= 255, "target domain too long");
        connect.extend_from_slice(&[3, host.len() as u8]);
        connect.extend_from_slice(host.as_bytes());
    } else {
        let addr = resolve_public(host, port)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no target address"))?;
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
        "upstream SOCKS connect rejected"
    );
    let len = match head[3] {
        1 => 4,
        4 => 16,
        3 => stream.read_u8().await? as usize,
        _ => bail!("invalid upstream SOCKS response"),
    };
    let mut rest = vec![0; len + 2];
    stream.read_exact(&mut rest).await?;
    Ok(stream)
}
async fn handle(mut client: TcpStream, config: Arc<Config>, egress: Arc<Egress>) -> Result<()> {
    let (host, port) = timeout(Duration::from_secs(10), request(&mut client)).await??;
    if !config
        .ssh_targets
        .iter()
        .any(|t| t.host == host && t.port == port)
    {
        reply(&mut client, 2).await?;
        bail!("SSH target denied");
    }
    let upstream = timeout(Duration::from_secs(15), proxy_connect(&egress, &host, port)).await;
    let mut upstream = match upstream {
        Ok(Ok(stream)) => stream,
        _ => {
            reply(&mut client, 1).await?;
            bail!("SSH upstream connection failed");
        }
    };
    reply(&mut client, 0).await?;
    timeout(
        egress.timeout,
        tokio::io::copy_bidirectional(&mut client, &mut upstream),
    )
    .await??;
    Ok(())
}
pub async fn serve(
    listener: TcpListener,
    config: Arc<Config>,
    egress: Arc<Egress>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let slots = Arc::new(Semaphore::new(config.max_connections));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                if !config.allows_client(peer.ip()) { tracing::warn!(entry="ssh", result="source_denied"); continue; }
                let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                let config = config.clone(); let egress = egress.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let ok = handle(stream, config, egress).await.is_ok();
                    // Deliberately exclude underlying error strings and user input.
                    tracing::info!(entry="ssh", success=ok);
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
