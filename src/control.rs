//! Atomic policy snapshots and authenticated, durable configuration edits.
use crate::{
    config::{Config, Repository, SshTarget, Upstream},
    egress::Egress,
};
use anyhow::{Context, Result, ensure};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, Semaphore};

pub struct Snapshot {
    pub config: Arc<Config>,
    pub egress: Arc<Egress>,
    pub http_slots: Arc<Semaphore>,
    pub ssh_slots: Arc<Semaphore>,
}
impl Snapshot {
    fn new(config: Arc<Config>, egress: Arc<Egress>) -> Self {
        Self {
            http_slots: Arc::new(Semaphore::new(config.max_connections)),
            ssh_slots: Arc::new(Semaphore::new(config.max_connections)),
            config,
            egress,
        }
    }
}
struct Store {
    path: PathBuf,
    bytes: Vec<u8>,
    revision: u64,
}
pub struct Control {
    current: RwLock<Arc<Snapshot>>,
    store: Option<Mutex<Store>>,
    token: Option<String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub clients: Vec<IpNet>,
    pub repositories: Vec<Repository>,
    pub ssh_targets: Vec<SshTarget>,
    pub direct: bool,
    pub max_connections: usize,
    pub timeout_seconds: u64,
    pub upstream: Upstream,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub revision: u64,
    pub settings: Settings,
    #[serde(default)]
    pub clear_credentials: Vec<String>,
}
#[derive(Serialize)]
pub struct Document {
    pub revision: u64,
    pub settings: Settings,
    pub proxy_credentials: Vec<String>,
}
impl Settings {
    fn from_config(c: &Config) -> Self {
        Self {
            clients: c.clients.clone(),
            repositories: c.repositories.clone(),
            ssh_targets: c.ssh_targets.clone(),
            direct: c.direct,
            max_connections: c.max_connections,
            timeout_seconds: c.timeout_seconds,
            upstream: c.upstream.clone(),
        }
    }
    fn apply(self, c: &mut Config) {
        c.clients = self.clients;
        c.repositories = self.repositories;
        c.ssh_targets = self.ssh_targets;
        c.direct = self.direct;
        c.max_connections = self.max_connections;
        c.timeout_seconds = self.timeout_seconds;
        c.upstream = self.upstream;
    }
}
fn clean_proxy(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return value.into();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.into()
}
fn proxy_pairs(upstream: &mut Upstream) -> [(&str, &mut Option<String>); 3] {
    [
        ("http_proxy", &mut upstream.http_proxy),
        ("https_proxy", &mut upstream.https_proxy),
        ("all_proxy", &mut upstream.all_proxy),
    ]
}
impl Control {
    pub fn readonly(config: Arc<Config>, egress: Arc<Egress>) -> Arc<Self> {
        Arc::new(Self {
            current: RwLock::new(Arc::new(Snapshot::new(config, egress))),
            store: None,
            token: None,
        })
    }
    pub fn managed(config: Arc<Config>, egress: Arc<Egress>, path: &Path) -> Result<Arc<Self>> {
        let path = path.canonicalize().context("cannot locate configuration")?;
        let bytes = std::fs::read(&path).context("cannot read configuration for editor")?;
        let loaded = Config::load(&path)?;
        ensure!(
            toml::to_string(&loaded)? == toml::to_string(&*config)?,
            "configuration changed during startup; retry startup"
        );
        let token = admin_token(&path)?;
        let mut generation = [0u8; 8];
        getrandom::fill(&mut generation)
            .map_err(|_| anyhow::anyhow!("cannot initialize configuration revision"))?;
        let revision = u64::from_le_bytes(generation) & ((1u64 << 48) - 1);
        Ok(Arc::new(Self {
            current: RwLock::new(Arc::new(Snapshot::new(config, egress))),
            store: Some(Mutex::new(Store {
                path,
                bytes,
                revision,
            })),
            token: Some(token),
        }))
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn enabled(&self) -> bool {
        self.store.is_some()
    }
    pub fn authorized(&self, value: Option<&str>) -> bool {
        let Some(token) = &self.token else {
            return false;
        };
        let Some(candidate) = value.and_then(|s| s.strip_prefix("Bearer ")) else {
            return false;
        };
        bool::from(candidate.as_bytes().ct_eq(token.as_bytes()))
    }
    pub async fn document(&self) -> Option<Document> {
        let store = self.store.as_ref()?.lock().await;
        let mut settings = Settings::from_config(&self.snapshot().config);
        let mut proxy_credentials = Vec::new();
        for (key, value) in proxy_pairs(&mut settings.upstream) {
            if let Some(raw) = value {
                let clean = clean_proxy(raw);
                if url::Url::parse(raw)
                    .is_ok_and(|u| !u.username().is_empty() || u.password().is_some())
                {
                    proxy_credentials.push(key.into());
                }
                *raw = clean;
            }
        }
        Some(Document {
            revision: store.revision,
            settings,
            proxy_credentials,
        })
    }
    pub async fn save(&self, mut update: Update, peer: IpAddr) -> Result<(), SaveError> {
        let Some(store) = &self.store else {
            return Err(SaveError::Disabled);
        };
        let mut store = store.lock().await;
        if update.revision != store.revision {
            return Err(SaveError::Conflict);
        }
        let old = self.snapshot();
        let mut old_upstream = old.config.upstream.clone();
        for ((key, new), (_, old)) in proxy_pairs(&mut update.settings.upstream)
            .into_iter()
            .zip(proxy_pairs(&mut old_upstream))
        {
            if !update.clear_credentials.iter().any(|name| name == key)
                && let (Some(proposed), Some(previous)) = (new.as_ref(), old.as_ref())
                && *proposed == clean_proxy(previous)
            {
                *new = Some(previous.clone());
            }
        }
        let mut config = (*old.config).clone();
        update.settings.apply(&mut config);
        config
            .validate()
            .map_err(|e| SaveError::Invalid(e.to_string()))?;
        if !config.allows_client(peer) {
            return Err(SaveError::Invalid("客户端范围必须保留当前连接来源".into()));
        }
        let egress = Egress::from_config(&config)
            .map_err(|_| SaveError::Invalid("代理配置无效，请检查协议、地址与 NO_PROXY".into()))?;
        let bytes = toml::to_string_pretty(&config)
            .map_err(|_| SaveError::Storage)?
            .into_bytes();
        let path = store.path.clone();
        let expected = store.bytes.clone();
        let output = bytes.clone();
        tokio::task::spawn_blocking(move || {
            if std::fs::read(&path).map_err(|_| SaveError::Storage)? != expected {
                return Err(SaveError::Conflict);
            }
            atomic_write(&path, &output).map_err(|_| SaveError::Storage)
        })
        .await
        .map_err(|_| SaveError::Storage)??;
        let mut snapshot = Snapshot::new(Arc::new(config), Arc::new(egress));
        if snapshot.config.max_connections == old.config.max_connections {
            snapshot.http_slots = old.http_slots.clone();
            snapshot.ssh_slots = old.ssh_slots.clone();
        }
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(snapshot);
        store.bytes = bytes;
        store.revision += 1;
        tracing::info!(event = "configuration_saved", revision = store.revision);
        Ok(())
    }
}
pub enum SaveError {
    Disabled,
    Conflict,
    Invalid(String),
    Storage,
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("configuration has no parent")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|_| anyhow::anyhow!("configuration replacement failed"))?;
    // Directory fsync is best-effort after rename: the visible committed state must stay in sync.
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}
fn admin_token(config: &Path) -> Result<String> {
    if let Ok(token) = std::env::var("GH_PROXY_ADMIN_TOKEN") {
        ensure!(
            token.len() >= 32 && token.len() <= 256 && token.bytes().all(|b| b.is_ascii_graphic()),
            "admin token must contain 32 to 256 printable ASCII characters"
        );
        return Ok(token);
    }
    let path = config.with_extension("admin-token");
    if path.exists() {
        let meta = std::fs::symlink_metadata(&path)?;
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "invalid administrator key file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                meta.permissions().mode() & 0o077 == 0,
                "administrator key file must be private (0600)"
            );
        }
        let token = std::fs::read_to_string(&path)?;
        ensure!(
            token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid administrator key file contents"
        );
        return Ok(token);
    }
    let mut random = [0u8; 32];
    getrandom::fill(&mut random)
        .map_err(|_| anyhow::anyhow!("cannot generate administrator key"))?;
    let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("invalid administrator key path")?)?;
    file.write_all(token.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .map_err(|_| anyhow::anyhow!("cannot store administrator key"))?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Arc<Control>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, include_str!("../config/example.toml")).unwrap();
        let c = Arc::new(Config::load(&path).unwrap());
        let e = Arc::new(
            Egress::from_lookup(|_| None, false, std::time::Duration::from_secs(1)).unwrap(),
        );
        let control = Control::managed(c, e, &path).unwrap();
        (dir, control)
    }
    async fn update(c: &Control) -> Update {
        let d = c.document().await.unwrap();
        Update {
            revision: d.revision,
            settings: d.settings,
            clear_credentials: vec![],
        }
    }
    #[tokio::test]
    async fn saves_atomically_and_publishes_a_live_snapshot() {
        let (dir, c) = fixture();
        let mut u = update(&c).await;
        u.settings.repositories[0].write = true;
        u.settings.timeout_seconds = 42;
        assert!(c.save(u, "127.0.0.1".parse().unwrap()).await.is_ok());
        assert!(c.snapshot().config.repositories[0].write);
        let loaded = Config::load(&dir.path().join("config.toml")).unwrap();
        assert_eq!(loaded.timeout_seconds, 42);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.path().join("config.admin-token"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let c2 = Control::managed(
            Arc::new(loaded),
            c.snapshot().egress.clone(),
            &dir.path().join("config.toml"),
        )
        .unwrap();
        assert_ne!(
            c.document().await.unwrap().revision,
            c2.document().await.unwrap().revision
        );
    }
    #[tokio::test]
    async fn rejects_invalid_stale_external_and_self_locking_updates() {
        let (dir, c) = fixture();
        let original = std::fs::read(dir.path().join("config.toml")).unwrap();
        let mut u = update(&c).await;
        u.settings.max_connections = 0;
        assert!(matches!(
            c.save(u, "127.0.0.1".parse().unwrap()).await,
            Err(SaveError::Invalid(_))
        ));
        assert_eq!(
            std::fs::read(dir.path().join("config.toml")).unwrap(),
            original
        );
        let mut u = update(&c).await;
        u.settings.clients = vec!["10.0.0.0/8".parse().unwrap()];
        assert!(matches!(
            c.save(u, "127.0.0.1".parse().unwrap()).await,
            Err(SaveError::Invalid(_))
        ));
        let stale = update(&c).await;
        let u = update(&c).await;
        assert!(c.save(u, "127.0.0.1".parse().unwrap()).await.is_ok());
        assert!(matches!(
            c.save(stale, "127.0.0.1".parse().unwrap()).await,
            Err(SaveError::Conflict)
        ));
        let u = update(&c).await;
        std::fs::write(dir.path().join("config.toml"), "# external edit").unwrap();
        assert!(matches!(
            c.save(u, "127.0.0.1".parse().unwrap()).await,
            Err(SaveError::Conflict)
        ));
    }
    #[tokio::test]
    async fn hides_preserves_and_explicitly_clears_proxy_credentials() {
        let (_dir, c) = fixture();
        let mut u = update(&c).await;
        u.settings.upstream.https_proxy =
            Some("http://private-user:private-password@127.0.0.1:7890".into());
        assert!(c.save(u, "127.0.0.1".parse().unwrap()).await.is_ok());
        let d = c.document().await.unwrap();
        let encoded = serde_json::to_string(&d).unwrap();
        assert!(!encoded.contains("private-user") && !encoded.contains("private-password"));
        assert_eq!(d.proxy_credentials, vec!["https_proxy"]);
        let u = update(&c).await;
        assert!(c.save(u, "127.0.0.1".parse().unwrap()).await.is_ok());
        assert!(
            c.snapshot()
                .config
                .upstream
                .https_proxy
                .as_ref()
                .unwrap()
                .contains("private-password")
        );
        let mut u = update(&c).await;
        u.clear_credentials.push("https_proxy".into());
        assert!(c.save(u, "127.0.0.1".parse().unwrap()).await.is_ok());
        assert!(
            !c.snapshot()
                .config
                .upstream
                .https_proxy
                .as_ref()
                .unwrap()
                .contains("private-password")
        );
    }

    #[tokio::test]
    async fn management_auth_origin_and_immediate_http_policy() {
        use axum::{
            body::{Body, to_bytes},
            extract::ConnectInfo,
            http::{Request, StatusCode},
        };
        use tower::ServiceExt;
        let (dir, c) = fixture();
        let token = std::fs::read_to_string(dir.path().join("config.admin-token")).unwrap();
        let app = crate::http::App::managed(c.clone());
        async fn call(
            app: crate::http::App,
            method: &str,
            path: &str,
            token: Option<&str>,
            origin: &str,
            body: Vec<u8>,
        ) -> axum::response::Response {
            let mut builder = Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:8080")
                .header("origin", origin)
                .header("content-type", "application/json");
            if let Some(token) = token {
                builder = builder.header("authorization", format!("Bearer {token}"));
            }
            let mut request = builder.body(Body::from(body)).unwrap();
            request.extensions_mut().insert(ConnectInfo(
                "127.0.0.1:1234".parse::<std::net::SocketAddr>().unwrap(),
            ));
            app.router().oneshot(request).await.unwrap()
        }
        assert_eq!(
            call(app.clone(), "GET", "/_ui/config", None, "", vec![])
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let mut u = update(&c).await;
        u.settings.repositories[0].name = "owner/allowed".into();
        let body =
            serde_json::to_vec(&serde_json::json!({"revision":u.revision,"settings":u.settings}))
                .unwrap();
        assert_eq!(
            call(
                app.clone(),
                "PUT",
                "/_ui/config",
                Some(&token),
                "https://attacker.example",
                body.clone()
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                app.clone(),
                "PUT",
                "/_ui/config",
                Some(&token),
                "http://127.0.0.1:8080",
                body
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            call(
                app.clone(),
                "GET",
                "/class-undefined/gh-proxy.git/info/refs?service=git-upload-pack",
                None,
                "",
                vec![]
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                app.clone(),
                "GET",
                "/owner/allowed.git/info/refs?service=git-upload-pack",
                None,
                "",
                vec![]
            )
            .await
            .status(),
            StatusCode::BAD_GATEWAY
        );
        let response = call(app, "GET", "/_ui/status", None, "", vec![]).await;
        let status = String::from_utf8(
            to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(!status.contains(&token));
    }

    #[tokio::test]
    async fn ssh_listener_uses_new_target_policy_without_restart() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::{TcpListener, TcpStream},
            sync::watch,
        };
        let (_dir, c) = fixture();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(crate::ssh::serve_managed(listener, c.clone(), stopped));
        async fn connect(addr: std::net::SocketAddr) -> u8 {
            let mut s = TcpStream::connect(addr).await.unwrap();
            s.write_all(&[5, 1, 0]).await.unwrap();
            let mut greeting = [0; 2];
            s.read_exact(&mut greeting).await.unwrap();
            s.write_all(&[5, 1, 0, 3, 10]).await.unwrap();
            s.write_all(b"github.com").await.unwrap();
            s.write_u16(22).await.unwrap();
            let mut reply = [0; 10];
            s.read_exact(&mut reply).await.unwrap();
            reply[1]
        }
        assert_eq!(connect(addr).await, 1); // Allowed target, unconfigured egress.
        let mut u = update(&c).await;
        u.settings.ssh_targets.clear();
        assert!(c.save(u, "127.0.0.1".parse().unwrap()).await.is_ok());
        assert_eq!(connect(addr).await, 2); // Now rejected by policy before any upstream attempt.
        stop.send(true).unwrap();
        task.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn failed_storage_does_not_publish() {
        let (dir, c) = fixture();
        let mut u = update(&c).await;
        u.settings.timeout_seconds = 99;
        std::fs::remove_file(dir.path().join("config.toml")).unwrap();
        assert!(matches!(
            c.save(u, "127.0.0.1".parse().unwrap()).await,
            Err(SaveError::Storage)
        ));
        assert_eq!(c.snapshot().config.timeout_seconds, 300);
    }
}
