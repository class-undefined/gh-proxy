# 验证记录

日期：2026-11-05。环境：macOS ARM64，Rust/Cargo 1.94.1。所有“通过”仅指下列范围，不代替目标部署环境验收。

## 本地自动化

`cargo fmt --check`、`cargo clippy --locked --all-targets -- -D warnings`、`cargo test --locked`、`cargo build --locked --release` 均通过，当前共 23 个测试。

| 覆盖范围 | 验证结果 |
| --- | --- |
| 配置校验 | 示例可加载，非安全监听、非法仓库拒绝，默认拒绝直连 |
| 代理变量 | 大小写、空值、按协议隔离、NO_PROXY 域名边界、默认端口与 IPv6 |
| HTTP 代理 | 本地代理实际收到 absolute-form HTTP 请求 |
| HTTPS 代理 | 本地 CONNECT 隧道及 TLS 上游完成 Git/API/附件转发 |
| 代理失效 | 即使 direct=true，已选代理失效仍报错，无直连回退 |
| SSH | SOCKS 握手、远端解析、SSH 字节透传、来源和目标拒绝、仅允许 CONNECT |
| 本地 DNS | SOCKS 本地解析的保留地址拒绝，远程解析发送域名 |
| Git 策略 | 读取允许、只读拒绝服务发现与 RPC 写入、未知仓库与混淆路径拒绝 |
| HTTP/2 | 正常伪头请求执行路径策略，HTTP/1 绝对代理请求拒绝 |
| Release 策略 | 查询、创建、更新、上传、下载、删除分别授权 |
| API 与附件 | 模拟 HTTPS 上游创建/更新，2 MiB 上传，10 MiB 下载，Git RPC 双向数据保持一致 |
| 重定向 | 拒绝未知域名、错误端口/协议、跨仓库目标，跨域附件下载不携带认证头 |
| 响应映射 | API URL、上传模板、浏览器下载 URL 改写，不改写用户正文 |
| 日志与错误 | 代理解析错误不包含凭据；请求头过滤代理认证和逐跳字段 |

Release 模拟测试在本地自签 HTTPS 上游运行，通过测试专用信任根校验证书；该信任根注入代码仅在 `cfg(test)` 中编译，生产构建不包含测试信任根。

## 真实 GitHub 只读验证

命令：

```sh
cargo build --locked
HTTPS_PROXY=http://127.0.0.1:7890 \
ALL_PROXY=socks5h://127.0.0.1:7890 \
NO_PROXY=localhost,127.0.0.1 \
python3 scripts/smoke.py --proxy-env --github-auth --ssh-auth
```

通过项目：

- 临时原生 TLS 网关启动、HTTPS 证书校验、就绪检查。
- 未授权仓库请求返回 403。
- 经网关查询本仓库 Release API，使用现有 gh CLI Token 完成认证；Token 只在内存使用，不写入配置。
- 经网关 HTTPS clone、fetch 本仓库。
- 经 SOCKS 获取 GitHub SSH greeting；客户端现有 SSH 身份由 GitHub 成功认证，保留主机密钥检查。
- SIGTERM 正常退出；日志不含测试使用的 Token。

本机 DNS 将 GitHub 解析为 Fake-IP 保留地址，直连测试被网关按预期拒绝；上述通过结果使用显式代理。匿名 API 请求在当前上游出口触发 GitHub 限流，因此采用已授权身份执行只读查询，没有发布、推送或创建远端业务数据。

## 仍待实际部署验收

- 目标服务器、真实客户端 CIDR、TLS 域名与证书、服务用户和上游地址。
- 目标 Linux 环境的 systemd 启动/重启、证书更新及外部客户端访问。
- 指定测试仓库中的真实 HTTP/SSH push、tag 推送及 pull。
- 指定测试仓库中的真实 Release 创建、更新和附件上传/下载；本次仅模拟这些修改操作。
- 目标环境的上游认证、故障注入、不同 DNS 模式及容量测试。

## 当前范围与限制

- 首期只有 GitHub 适配器，固定 Git、API、上传及附件目标，不提供任意反向代理或通用 SOCKS。
- 仅支持 Git Smart HTTP，不提供 Git LFS、dumb HTTP、任意平台 API 或 GitHub 网页代理。
- SSH 不做仓库或读写过滤，按文档保持远端身份认证。
- URI 严格规范化；首期不支持 tag 或附件 URL 中含编码斜杠的路径。
- API 元数据最大 8 MiB，附件与 Git 数据流式传输；大文件需要按网络带宽调整请求/会话时长。
- HTTP 请求并发和 SSH 会话并发分别限额。HTTP 请求在转发容量耗尽时返回 503 和 `connection_limit_reached`。
- 就绪状态仅表示配置上具备路由，不执行网络探测。网页保存的策略热应用，手工修改配置、启动环境、监听和证书仍需重启。
- 来源按 TCP 对端判断；反向代理后的原始客户端 ACL 必须在反向代理执行。

## 接口依据

实现使用 Tokio 异步 I/O、Axum 0.8、reqwest 0.12、rustls 与 axum-server；依赖精确解析结果记录在 Cargo.lock。

GitHub Release 下载的二进制 API 既可能直接返回内容，也可能重定向，网关覆盖两种响应，并保持仓库授权上下文。接口依据：[GitHub Release assets](https://docs.github.com/en/rest/releases/assets)、[GitHub Releases](https://docs.github.com/en/rest/releases/releases)。

SSH 443 入口使用 `ssh.github.com:443`，按目标逐项授权。依据：[GitHub SSH over HTTPS port](https://docs.github.com/en/authentication/troubleshooting-ssh/using-ssh-over-the-https-port)。


## Web 控制台验证（2026-11-05）

新增内置控制台后，Rust 测试共 25 个通过，Node.js 前端测试共 6 个通过；格式、Clippy 和 release 构建通过。

新增自动化覆盖：页面/资源/状态接口共用来源 ACL，公开 Host 校验、GET/HEAD 方法限制、关闭开关、缓存及 CSP 响应头、状态字段不泄露代理凭据，以及控制台轮询不增加代理请求计数。前端覆盖搜索与权限筛选、只读/读写命令差异、SSH 443 配置、Release 示例授权、Shell 引号和续行、空仓库及 HTTP 入口不生成 Token 命令。

通过本机 Google Chrome 进行可视化及交互检查：桌面布局、400px 移动视口的概览与接入指南、仓库搜索及空结果、HTTPS/SSH 切换、命令复制成功提示、服务中断的旧数据提示和恢复后刷新。没有在生产环境开放控制台，也未通过控制台修改网关策略。

设计参考来自用户指定的 [OpenDesign Skills](https://open-design.ai/zh/skills/)，采用 [工业排版界面](https://open-design.ai/zh/skills/industrial-brutalist-ui/) 的网格与功能性色彩方向；资源为仓库原创 HTML/CSS/JS/SVG，没有安装或复制第三方 skill 代码。


## 配置管理与界面精简验证（2026-11-05）

新增配置管理后，Rust 测试 31 个、前端测试 9 个通过。覆盖管理密钥认证、Origin 拒绝、原子保存及重读、失败不发布、旧版本和外部文件冲突、自锁来源拒绝、代理凭据脱敏/保留/清除，以及 HTTP 与 SSH 新策略无需重启生效。配置修订号使用每次启动随机生成的安全整数，旧进程页面不能复用同一个版本号。

在独立临时配置和仅本机的测试服务中，通过 Chrome 解锁设置、添加 `owner/ui-test`、开启 Release 查询、将并发改为 64 并保存。页面提示已应用，仓库列表即时变为两个仓库；从 TOML 读取确认新仓库与并发值已持久化。使用公开的临时测试密钥，不修改实际部署配置。Chrome 控制台无脚本错误；400px 视口下仓库行转为卡片式编辑，权限开关和底部保存栏可见。

界面移除了英文副标题、装饰编号、营销段落及重复说明。管理密钥位置折叠在按需展开区域，表单仅保留字段名、分组和必要的错误/保存状态。

本次没有实际执行 Linux 部署。systemd 示例已将可写配置与密钥放入 `/var/lib/gh-proxy`，与 `/etc/gh-proxy` 下的只读代理环境、证书分离；需要目标主机管理员按运行文档安装或迁移。

## HeroUI 参考样式验证（2026-11-05）

控制台按 HeroUI 的语义主题与组件方向更新为中性灰背景、蓝色主操作、圆角卡片、填充字段、开关及胶囊状态。当前设计规则记录于 [界面说明](design.md)。保留精简导航及分组布局，不恢复英文副标题、装饰编号或重复说明。

Rust 31 个测试、前端 9 个测试、格式、Clippy 和 release 构建通过。Chrome 桌面与 400px 手机视口检查了概览和设置布局；使用独立临时配置解锁、关闭 SSH 443 开关并保存，界面提示已应用，重读 TOML 确认目标移除。浏览器控制台未出现脚本错误，开关向辅助技术暴露 switch 角色与开关状态。测试未修改实际部署配置。


## 2026-11-05：认证与出站诊断

本次故障由 `direct=false` 且未配置有效出站代理触发，尚未到达 GitHub 的认证阶段。通过管理接口保留现有策略并设置已经实测可用的 HTTP CONNECT 与 socks5h 上游后，匿名 Git 请求得到 GitHub 的 401 和 Basic challenge。业务运行配置保留在本地，未提交到公开仓库。

验证结果：38 个 Rust 单元测试、1 个真实 Git 客户端认证集成测试、9 个前端测试通过；格式检查、Clippy 和 release 构建通过。

新增覆盖 Git HTTP 匿名/错误凭据/有效凭据、多个 WWW-Authenticate 保留、非 JSON 的 API 401/403 与 SSO 提示透传、HTTP CONNECT 407 的脱敏错误码、SOCKS 用户名密码百分号解码及认证失败不回退、缺失出站配置的 503，以及环境变量大写空值优先级。客户端 helper 集成测试使用隔离的 HOME、模拟 gh 凭据和真实 git-http-backend，确认 Git 接收 401 后取得凭据并成功读取，其他 origin 无凭据输出。

实际私有仓库经现有网关分别完成 HTTP/SSH `ls-remote` 和浅克隆：HTTP 使用客户端已有 GitHub 登录；SSH 使用已有客户端密钥且保留主机密钥检查。临时克隆随后清理，没有执行 push、tag 或 Release 写入。新构建另在独立临时 TLS 网关完成公开仓库 clone/fetch、带客户端 Token 的 API 查询、SSH 平台身份认证、未授权仓库拒绝及 SIGTERM 退出；测试日志未出现 Token。

运行中的业务进程只热应用了上游配置；新的错误诊断和控制台界面需重启该进程加载。认证角色、客户端 helper 配置和错误码见 [认证文档](authentication.md)。
