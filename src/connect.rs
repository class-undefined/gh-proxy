//! Client-side SOCKS5 transport for OpenSSH ProxyCommand.
//! stdout carries tunnel bytes only; SSH authentication remains with OpenSSH.
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{self, Read, Write},
    net::Shutdown,
    sync::mpsc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

fn gateway(value: &str) -> Result<(String, u16)> {
    let url = url::Url::parse(&format!("socks5://{value}"))
        .map_err(|_| anyhow::anyhow!("SOCKS gateway must be host:port or [IPv6]:port"))?;
    ensure!(
        url.host_str().is_some()
            && url.port().is_some_and(|p| p != 0)
            && url.username().is_empty()
            && url.password().is_none()
            && url.path().is_empty()
            && url.query().is_none()
            && url.fragment().is_none(),
        "SOCKS gateway must be host:port or [IPv6]:port, without credentials"
    );
    Ok((
        url.host_str()
            .context("missing SOCKS gateway host")?
            .trim_matches(['[', ']'])
            .into(),
        url.port().context("missing SOCKS gateway port")?,
    ))
}

fn validate_target(host: &str, port: u16) -> Result<()> {
    ensure!(
        port != 0
            && !host.is_empty()
            && host.len() <= 253
            && host.parse::<std::net::IpAddr>().is_err()
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .as_bytes()
                        .first()
                        .is_some_and(u8::is_ascii_alphanumeric)
                    && label
                        .as_bytes()
                        .last()
                        .is_some_and(u8::is_ascii_alphanumeric)
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }),
        "SSH target must be a DNS hostname and a nonzero port"
    );
    Ok(())
}

async fn open(gateway: &(String, u16), host: &str, port: u16) -> Result<TcpStream> {
    let mut socket = TcpStream::connect((gateway.0.as_str(), gateway.1))
        .await
        .map_err(|_| anyhow::anyhow!("cannot connect to SOCKS gateway"))?;
    socket
        .set_nodelay(true)
        .context("cannot configure SOCKS socket")?;
    socket
        .write_all(&[5, 1, 0])
        .await
        .context("cannot send SOCKS greeting")?;
    let mut greeting = [0; 2];
    socket
        .read_exact(&mut greeting)
        .await
        .context("incomplete SOCKS greeting")?;
    ensure!(
        greeting == [5, 0],
        "SOCKS gateway rejected the unauthenticated method; check the gateway source policy and endpoint"
    );
    // ATYP=DOMAIN: only the gateway/upstream resolves the target hostname.
    let mut request = vec![5, 1, 0, 3, host.len() as u8];
    request.extend_from_slice(host.as_bytes());
    request.extend_from_slice(&port.to_be_bytes());
    socket
        .write_all(&request)
        .await
        .context("cannot send SOCKS CONNECT")?;
    let mut head = [0; 4];
    socket
        .read_exact(&mut head)
        .await
        .context("incomplete SOCKS CONNECT reply")?;
    ensure!(head[0] == 5 && head[2] == 0, "invalid SOCKS CONNECT reply");
    match head[1] {
        0 => {}
        2 => bail!("SOCKS gateway denied the target; check client sources, SSH hostname and port"),
        3 => bail!("SOCKS gateway reports network unreachable"),
        4 => bail!("SOCKS gateway reports host unreachable"),
        5 => bail!("SOCKS gateway reports connection refused"),
        6 => bail!("SOCKS gateway reports connection expired"),
        7 => bail!("SOCKS gateway does not support CONNECT"),
        8 => bail!("SOCKS gateway does not support domain targets"),
        _ => bail!(
            "SOCKS gateway could not establish the tunnel; check gateway logs and upstream configuration"
        ),
    }
    let len = match head[3] {
        1 => 4,
        4 => 16,
        3 => {
            let len = socket
                .read_u8()
                .await
                .context("incomplete SOCKS bound hostname")?;
            ensure!(len != 0, "invalid SOCKS bound hostname");
            usize::from(len)
        }
        _ => bail!("invalid SOCKS bound address type"),
    };
    let mut bound = vec![0; len + 2];
    socket
        .read_exact(&mut bound)
        .await
        .context("incomplete SOCKS bound address")?;
    Ok(socket)
}

/// Run as a standalone client command; no gateway configuration or proxy environment is read.
pub fn run(socks: &str, host: &str, port: u16, timeout: Duration) -> Result<()> {
    let gateway = gateway(socks)?;
    validate_target(host, port)?;
    ensure!(!timeout.is_zero(), "connect timeout must be positive");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let socket = runtime
        .block_on(async {
            tokio::time::timeout(timeout, open(&gateway, host, port))
                .await
                .context("SOCKS gateway connection or handshake timed out")?
        })
        .and_then(|socket| socket.into_std().context("cannot transfer SOCKS socket"));
    // A pending OS resolver task must not delay CLI exit after the handshake deadline.
    runtime.shutdown_background();
    let mut socket = socket?;
    socket
        .set_nonblocking(false)
        .context("cannot configure tunnel socket")?;
    let mut writer = socket.try_clone().context("cannot split tunnel socket")?;
    let (sent, received) = mpsc::channel();
    // A dedicated OS thread avoids Tokio stdin's uncancellable blocking read keeping
    // the process alive after remote EOF. It intentionally lives only until CLI exit.
    std::thread::Builder::new()
        .name("ssh-stdin".into())
        .spawn(move || {
            let result = io::copy(&mut io::stdin().lock(), &mut writer);
            let _ = writer.shutdown(if result.is_ok() {
                Shutdown::Write
            } else {
                Shutdown::Both
            });
            let _ = sent.send(result);
        })
        .context("cannot start tunnel input")?;
    let mut output = io::stdout().lock();
    let result = copy_output(&mut socket, &mut output);
    let _ = socket.shutdown(Shutdown::Both);
    result.context("tunnel output failed")?;
    if let Ok(result) = received.try_recv() {
        result.context("tunnel input failed")?;
    }
    Ok(())
}

fn copy_output(input: &mut impl Read, output: &mut impl Write) -> io::Result<()> {
    let mut bytes = [0; 16 * 1024];
    loop {
        let len = match input.read(&mut bytes) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if len == 0 {
            return Ok(());
        }
        output.write_all(&bytes[..len])?;
        // stdout is line buffered; every binary SSH packet must reach OpenSSH promptly.
        output.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn addresses_are_explicit_and_target_dns_stays_remote() {
        assert_eq!(
            gateway("localhost:1080").unwrap(),
            ("localhost".into(), 1080)
        );
        assert_eq!(gateway("[::1]:1080").unwrap(), ("::1".into(), 1080));
        for value in [
            "localhost",
            "localhost:0",
            "user:secret@localhost:1080",
            "localhost:1080/path",
            "http://localhost:1080",
        ] {
            assert!(gateway(value).is_err());
        }
        assert!(validate_target("unresolvable.invalid", 22).is_ok());
        for host in [
            "127.0.0.1",
            "::1",
            "-bad.example",
            "bad.example/other",
            "github.com\n",
            "",
        ] {
            assert!(validate_target(host, 22).is_err());
        }
        assert!(validate_target("github.com", 0).is_err());
    }
}
