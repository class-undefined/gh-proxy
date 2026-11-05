# 构建、配置与运行

## 构建与本机试用

需要 Rust 工具链管理器 `rustup`；仓库固定 Rust 1.94.1。执行：

```sh
cargo build --locked --release
cp config/example.toml config.local.toml
HTTPS_PROXY=http://127.0.0.1:7890 \
ALL_PROXY=socks5h://127.0.0.1:7891 \
NO_PROXY=localhost,127.0.0.1 \
./target/release/gh-proxy --config config.local.toml --check
```

将上述代理改为实际可用地址；HTTP 与 SOCKS 可以使用同一个支持混合协议的端口。`--check` 只校验配置、代理变量和 TLS 文件，不探测上游连通性。确认后以相同环境去掉 `--check` 启动。

示例只允许本机访问和 `class-undefined/gh-proxy` 仓库读取，不允许 Git 写入和 Release 修改。默认 `direct=false`；无匹配代理时出站被拒绝。若明确需要直连，将配置改为 `direct=true`，并检查 `NO_PROXY`。

本机不携带凭据的公开仓库试用：

```sh
git clone http://127.0.0.1:8080/class-undefined/gh-proxy.git
curl --fail http://127.0.0.1:8080/health/live
curl --fail http://127.0.0.1:8080/health/ready
```

HTTP 明文仅允许绑定 loopback，用于开发或本地 TLS 反向代理。携带 Token、密码的客户端应使用 HTTPS。外部 HTTP 监听必须配置原生 TLS，不能通过转发头声称请求已加密。

## Web 控制台

启动网关后，访问配置中的 `public_url` 根路径或 `/ui`：示例为 `http://127.0.0.1:8080/`。

控制台按「概览、仓库、设置、接入」组织。设置页支持添加/移除仓库、逐项编辑 Git/Release 权限、客户端 CIDR、SSH 目标、出站代理、直连开关、并发及请求时限；保存后更新配置文件，并热应用于后续 HTTP 请求和新建 SSH 连接。已建立的请求/隧道继续使用原策略，修改并发上限不会中断现有连接。

首次启动在配置文件同目录创建同名 `.admin-token`，权限为 `0600`。例如使用 `config.local.toml` 时，在服务主机读取：

```sh
cat config.local.admin-token
```

将密钥输入设置页解锁。也可通过 `GH_PROXY_ADMIN_TOKEN` 指定 32–256 个可打印 ASCII 字符的密钥，此时不生成密钥文件。解锁状态仅保留于当前页面内存；锁定或刷新后需重新输入，浏览器不由应用主动持久化密钥。生产管理入口必须使用 HTTPS；HTTP 仅限本机开发。

`GET /_ui/config` 和 `PUT /_ui/config` 使用 `Authorization: Bearer <管理密钥>`。写入还要求匹配 public_url 的 Origin 和 JSON 内容类型，最大 1 MiB。保存先校验内容与当前客户端仍被允许，再按版本检查冲突、原子替换文件并发布策略快照。新文件权限为 `0600`。TOML 会规范化重写，原注释不保留；配置文件及其父目录均需允许服务用户写入。旧页面版本返回 409，重新加载配置后再提交；检测到外部直接改文件则拒绝覆盖，需要重启服务读取外部配置。校验或存储失败不改变运行中的策略。

管理读取不返回已保存代理的用户名/密码；原地址未改动时保留凭据，可显式清除或输入包含新凭据的代理 URL 来替换。管理密钥、代理 URL 和请求体不进入应用日志。配置文件可能包含代理凭据，请放在私有目录，不要提交到 Git。

监听地址、public_url、TLS 文件和 web_enabled 仍属于启动配置，不通过页面修改。直接改文件、环境变量或证书后需要重启；网页保存的运行策略无需重启。设置 `web_enabled=false` 并重启可关闭控制台和管理接口。

页面与资源全部嵌入 Rust 二进制，不依赖 CDN、外部字体或生产 Node.js。编辑 `web/` 后重新构建；前端测试使用 Node.js 20+：`node --test web/tests/*.test.mjs`。

`GET /_ui/status` 提供脱敏状态，不返回代理 URL、管理密钥、证书或私钥路径。所有 UI 入口共用 clients 白名单，并校验 Host 与 public_url 的主机、端口；反向代理应保留公开 Host。除管理配置接口外，页面与状态只接受 GET/HEAD。

页面每 15 秒刷新，后台暂停轮询，编辑中的草稿不被轮询覆盖。入口的「已配置」只表示配置上具备路由，不代表已探测 GitHub 连通性。计数从进程启动累计，只统计来源校验后的 HTTP 代理请求，不含页面、健康检查或 SSH；转发失败对应网关自身的转发错误（502/503/504）及响应体传输失败，不含直接透传的上游 4xx/5xx。

## 配置语义

配置使用严格 TOML，未知字段报错。完整字段见 [示例](../config/example.toml)。

| 字段 | 含义 |
| --- | --- |
| `http_listen`、`socks_listen` | 两个不同的监听地址 |
| `public_url` | 对外根地址，无路径前缀、查询或凭据；用于重写 API URL |
| `clients` | TCP 对端 IP 的 CIDR 白名单，支持 IPv4/IPv6 |
| `web_enabled` | 是否开放内置控制台，默认 true |
| `direct` | 是否允许无代理或 NO_PROXY 命中时直连，默认 false |
| `max_connections` | HTTP 活跃转发和 SSH 隧道分别使用的上限，默认 128 |
| `timeout_seconds` | HTTP 完整出站请求与 SSH 转发会话的时长上限，默认 300，最大 86400 |
| `tls.cert`、`tls.key` | PEM 证书链和私钥文件 |
| `repositories[].name` | 精确 `owner/repo`，不含 `.git`，不区分大小写 |
| `repositories[].write` | 是否允许 receive-pack，包括分支和 tag 推送 |
| `repositories[].release` | query/create/update/upload/download/delete 分别授权，默认 false |
| `upstream.http_proxy` / `https_proxy` / `all_proxy` / `no_proxy` | 可选代理覆盖，未设置时继承环境变量，空字符串停用对应项 |
| `ssh_targets` | 首期只接受 `github.com:22`、`ssh.github.com:443`，需逐项配置 |

来源始终按 TCP 对端判定，不信任 `X-Forwarded-For`。部署在反向代理之后时，原始客户端来源 ACL 必须在反向代理执行，网关只允许该代理对端；原生 TLS 部署可直接使用网关客户端 ACL。

SSH 握手上限 10 秒，建立出站连接上限 15 秒；HTTP 建连上限 15 秒，整个请求受配置时长限制。`max_connections` 在 HTTP 中覆盖请求上传与响应下载，SSH 中覆盖完整隧道。大仓库或大附件可适当提高时长。

## 上游选路

| 流量 | 变量 |
| --- | --- |
| HTTP | HTTP_PROXY / http_proxy |
| HTTPS（GitHub Git/API/附件） | HTTPS_PROXY / https_proxy |
| SSH | ALL_PROXY / all_proxy，限定 socks5 或 socks5h |
| HTTP(S) 绕过 | NO_PROXY / no_proxy |

控制台或 `[upstream]` 显式配置优先于环境变量：未设置字段时继承环境变量，空字符串表示停用，非空值替代对应变量。继承环境时，大写存在始终优先，包括空值。空值表示未配置；非法 URL 直接报错，错误不回显 URL。代理 URL 支持用户名和密码，特殊字符使用 URL 百分号编码，显式指定 SOCKS 端口。HTTP(S) 不继承 ALL_PROXY 或其他协议的代理；SSH 不应用 NO_PROXY。

NO_PROXY 支持逗号分隔的 `*`、域名及子域名、前导点域名后缀、IP、可选端口；IPv6 带端口使用 `[::1]:443`。不支持 CIDR 或 URL，配置这类规则会报错。域名在标签边界匹配，`github.com` 不匹配 `evil-github.com`。

NO_PROXY 命中后仍要求 `direct=true`。代理一旦选中，失败即返回错误，不回退直连。`socks5` 在网关解析，`socks5h` 在上游解析。HTTP CONNECT 和远程 SOCKS DNS 依赖可信上游解析，网关限制原始目标和跳转目标；本地直接解析会拒绝内网、保留和文档地址。使用 Fake-IP DNS 时应使用显式 HTTP CONNECT 或 `socks5h` 上游，不能通过放宽保留地址限制绕过。

## Git HTTP(S) 客户端

部署 TLS 后可直接使用：

```sh
git clone https://git-gateway.example/owner/repo.git
git -C repo fetch
git -C repo push
git -C repo push --tags
```

需先在配置中允许对应仓库，并按需设置 `write=true`。网关转发客户端认证头，由 GitHub 验证凭据。不要将 Token 嵌入仓库 URL。私有仓库的凭据复用、客户端 helper 和 SSH 排查见 [认证文档](authentication.md)。

可在单个客户端选择配置 URL 改写：

```sh
git config --global url."https://git-gateway.example/".insteadOf https://github.com/
```

此配置影响该客户端的全部 GitHub HTTPS 仓库；移除命令为 `git config --global --unset-all url."https://git-gateway.example/".insteadOf`。

只实现 Smart HTTP，不提供 dumb HTTP、Git LFS 或任意 GitHub 网页代理。只读同时限制服务发现及实际 receive-pack；不提供逐分支、逐 tag 权限。

## SSH 客户端

客户端安装 `connect-proxy`，配置：

```sshconfig
Host github.com
    User git
    ProxyCommand connect-proxy -S git-gateway.example:1080 %h %p
```

然后继续使用 `git@github.com:owner/repo.git`。使用 OpenBSD nc 时可替换为 `ProxyCommand nc -X 5 -x git-gateway.example:1080 %h %p`。

若使用 SSH 443 入口，设置 `HostName ssh.github.com`、`Port 443`，并在网关允许该目标。不要禁用 SSH 主机密钥校验。SOCKS 入口只支持域名形式的 CONNECT，不支持 IP 字面量、BIND、UDP、任意端口或任意域名。

网关不终止 SSH，不持有客户端 SSH 私钥。客户端原有公钥由 GitHub 验证，仓库与读写权限由 GitHub 决定，HTTP 仓库白名单不限制 SSH 仓库访问。

## Release API 与附件

网关入口映射：

| 网关前缀 | 固定上游 |
| --- | --- |
| `/api/github/` | `https://api.github.com/` |
| `/uploads/github/` | `https://uploads.github.com/` |
| `/owner/repo/releases/download/tag/file` | GitHub 仓库 Release 下载路径 |

只接受已知 Release 路径及操作。API 查询允许 `page`、`per_page`，附件上传允许 `name`、`label`，不提供任意 API 转发。路径编码采取严格策略，含编码斜杠、二次编码、路径穿越或空段的路径拒绝；首期含斜杠的 tag/附件路径暂不支持。

以下命令在已配置该仓库和操作权限的 HTTPS 网关执行，Token 来自本地环境。不要将真实 Token 写入文档、仓库或日志：

```sh
# 查询 Release
curl --fail-with-body \
  -H "Authorization: Bearer $GITHUB_TOKEN" \
  https://git-gateway.example/api/github/repos/owner/repo/releases

# 创建 Release；release.json 例如 {"tag_name":"v1.0.0","name":"v1.0.0"}
curl --fail-with-body -X POST \
  -H "Authorization: Bearer $GITHUB_TOKEN" \
  -H 'Content-Type: application/json' --data-binary @release.json \
  https://git-gateway.example/api/github/repos/owner/repo/releases

# 更新 Release
curl --fail-with-body -X PATCH \
  -H "Authorization: Bearer $GITHUB_TOKEN" \
  -H 'Content-Type: application/json' --data-binary @release.json \
  https://git-gateway.example/api/github/repos/owner/repo/releases/RELEASE_ID

# 上传附件
curl --fail-with-body -X POST \
  -H "Authorization: Bearer $GITHUB_TOKEN" \
  -H 'Content-Type: application/octet-stream' --data-binary @artifact.zip \
  'https://git-gateway.example/uploads/github/repos/owner/repo/releases/RELEASE_ID/assets?name=artifact.zip'

# 按授权仓库内的 asset_id 下载，网关内部处理受控跳转
curl --fail-with-body \
  -H "Authorization: Bearer $GITHUB_TOKEN" \
  -H 'Accept: application/octet-stream' -o artifact.zip \
  https://git-gateway.example/api/github/repos/owner/repo/releases/assets/ASSET_ID
```

JSON 响应中的 API URL、upload_url 模板、browser_download_url 会改写为配置的 public_url。查询分页 Link 同样改写。普通正文中的用户文字保持原样。API 元数据上限 8 MiB；Git 数据和附件流式传输。上传不自动重试，避免重复创建附件。

下载上下文必须起自已授权仓库路径。未知附件直链拒绝。最多跟随 5 次跳转，要求 HTTPS、443、精确的附件主机及规定路径；跨主机移除认证头。非下载操作的跳转拒绝。客户端用上述路径或返回的改写链接访问，不能将此入口直接当作 `gh` CLI 的通用 API 主机。

## Linux systemd 部署

在目标 Linux 主机构建 release 二进制；macOS 构建产物不能直接运行于 Linux。以下为管理员在实际部署主机执行的示例，本次实现没有自动执行这些部署操作：

```sh
sudo useradd --system --no-create-home --shell /usr/sbin/nologin gh-proxy
sudo install -m 0755 target/release/gh-proxy /usr/local/bin/gh-proxy
sudo install -d -o root -g gh-proxy -m 0750 /etc/gh-proxy
sudo install -d -o gh-proxy -g gh-proxy -m 0700 /var/lib/gh-proxy
sudo install -o gh-proxy -g gh-proxy -m 0600 config/example.toml /var/lib/gh-proxy/config.toml
sudo install -o root -g root -m 0600 deploy/proxy.env.example /etc/gh-proxy/proxy.env
sudo install -m 0644 deploy/gh-proxy.service /etc/systemd/system/gh-proxy.service
```

启动前修改配置和代理地址，设置真实仓库、客户端 CIDR、HTTPS public_url，并为外部监听配置 `[tls]`。证书链和私钥需允许服务用户读取，私钥建议 `root:gh-proxy`、`0640`，不要提交到仓库。

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now gh-proxy
sudo systemctl status gh-proxy
sudo journalctl -u gh-proxy -f
sudo systemctl restart gh-proxy
```

首次启动后通过 `sudo cat /var/lib/gh-proxy/config.admin-token` 获取管理密钥。已有部署升级时，应先将原配置迁移到 `/var/lib/gh-proxy/config.toml` 并设置为服务用户所有、0600；不要用示例覆盖业务配置。网页保存会即时应用运行策略；直接改配置文件、启动环境、监听或 TLS 证书后仍需重启。SIGINT/SIGTERM 停止接收连接，最多等待 10 秒完成活跃请求或隧道。

`/health/live` 表示进程可服务，`/health/ready` 表示所启用入口有配置上的出站路由，不代表代理或 GitHub 当前可达；它们也受客户端白名单限制。使用授权公开仓库读取进行外部连通性监测，避免探针触发写入。

GitHub 的 401/403 和认证挑战透传；网关出站配置缺失或容量耗尽返回 503，超时返回 504，连接、代理认证、响应或跳转错误返回 502，并携带固定 `X-Gh-Proxy-Error` 错误码。具体处理见 [认证与排查](authentication.md)。结构化日志包含请求 ID、仓库、操作、状态和响应头到达耗时，不记录请求体、认证头、完整查询、原始错误链或代理 URL；流完成后的错误以连接中断体现。不要通过扩展日志字段记录签名 URL 或 Token。

## 验证

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
node --test web/tests/*.test.mjs
cargo build --locked
# 可选：需要 Git、Python 3、OpenSSL 和真实 GitHub 网络连接
python3 scripts/smoke.py
# 使用已配置代理；--github-auth 只在内存读取 gh CLI Token，避免匿名 API 限流
HTTPS_PROXY=http://127.0.0.1:7890 ALL_PROXY=socks5h://127.0.0.1:7890 \
NO_PROXY=localhost,127.0.0.1 \
python3 scripts/smoke.py --proxy-env --github-auth --ssh-auth
```

Smoke 测试生成临时证书并验证 HTTPS，不修改系统信任，使用公开仓库 clone/fetch 和 API 查询，不推送代码、不创建 Release。`--ssh-auth` 需要已安装 OpenBSD nc、有可用的现有 SSH 身份及已信任的 GitHub 主机密钥，不自动接受未知主机密钥。
