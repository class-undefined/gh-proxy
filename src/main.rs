use anyhow::{Context, Result};
use clap::Parser;
use gh_proxy::{config::Config, egress::Egress, http::App, ssh};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::watch};
use tracing_subscriber::{filter::Targets, prelude::*};

#[derive(Parser)]
#[command(version, about = "Policy-controlled Git and GitHub Release gateway")]
struct Args {
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,
    /// Validate configuration and proxy environment without binding sockets.
    #[arg(long)]
    check: bool,
}
async fn signal() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
        } else {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    // Only application event fields are logged; library debug logs may expose URLs.
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().json())
        .with(Targets::new().with_target("gh_proxy", tracing::Level::INFO))
        .init();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let args = Args::parse();
    let config = Arc::new(Config::load(&args.config)?);
    let egress = Arc::new(Egress::from_config(&config)?);
    let tls = if let Some(tls) = &config.tls {
        Some(
            axum_server::tls_rustls::RustlsConfig::from_pem_file(&tls.cert, &tls.key)
                .await
                .map_err(|_| anyhow::anyhow!("cannot load TLS certificate or key"))?,
        )
    } else {
        None
    };
    if args.check {
        println!(
            "configuration valid (http_route={}, ssh_route={}; upstream reachability not probed)",
            egress.http_available(),
            egress.ssh_available()
        );
        return Ok(());
    }
    let control = if config.web_enabled {
        gh_proxy::control::Control::managed(config.clone(), egress.clone(), &args.config)?
    } else {
        gh_proxy::control::Control::readonly(config.clone(), egress.clone())
    };
    let http = TcpListener::bind(config.http_listen)
        .await
        .context("cannot bind HTTP listener")?
        .into_std()?;
    let socks = TcpListener::bind(config.socks_listen)
        .await
        .context("cannot bind SOCKS listener")?;
    let router = App::managed(control.clone())
        .router()
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    let handle = axum_server::Handle::new();
    let (stop, stopped) = watch::channel(false);
    let shutdown_handle = handle.clone();
    let http_task = tokio::spawn(async move {
        if let Some(tls) = tls {
            axum_server::from_tcp_rustls(http, tls)?
                .handle(handle)
                .serve(router)
                .await
        } else {
            axum_server::from_tcp(http)?
                .handle(handle)
                .serve(router)
                .await
        }
    });
    let ssh_task = tokio::spawn(ssh::serve_managed(socks, control, stopped));
    tracing::info!(http=%config.http_listen,socks=%config.socks_listen,event="started");
    let mut http_task = http_task;
    let mut ssh_task = ssh_task;
    tokio::select! {
        _ = signal() => {
            tracing::info!(event="stopping");
            let _ = stop.send(true);
            shutdown_handle.graceful_shutdown(Some(Duration::from_secs(10)));
            http_task.await.context("HTTP task failed")??;
            ssh_task.await.context("SSH task failed")??;
        }
        result = &mut http_task => {
            let _ = stop.send(true); let _ = ssh_task.await;
            result.context("HTTP task failed")??;
            anyhow::bail!("HTTP server stopped unexpectedly");
        }
        result = &mut ssh_task => {
            shutdown_handle.graceful_shutdown(Some(Duration::from_secs(10))); let _ = http_task.await;
            result.context("SSH task failed")??;
            anyhow::bail!("SSH server stopped unexpectedly");
        }
    }
    Ok(())
}
