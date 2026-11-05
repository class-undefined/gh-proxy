# gh-proxy

使用 **Rust** 实现的 Git 访问网关，首期支持 GitHub：Git HTTP(S)、SSH 透明转发、Release API 与附件传输，共用来源策略和显式出站代理配置。

## 功能

- Git Smart HTTP：保留 `/owner/repo.git`，按仓库白名单控制读取与写入，覆盖 clone、fetch、pull、push 和 tag 推送。
- SSH：受限 SOCKS5 CONNECT，保留端到端 SSH 身份认证，限制客户端、GitHub 目标及端口。
- Release：按仓库及操作控制查询、创建、更新、删除、上传和下载，重写 API 链接并逐跳限制附件重定向。
- 出站：支持大小写代理环境变量及 NO_PROXY，默认拒绝无代理直连，代理故障不回退直连。
- Web 控制台：状态概览、仓库搜索、接入命令，以及带管理密钥的配置编辑；保存后热应用仓库、来源、SSH、代理和连接参数。
- 服务：原生 TLS、配置检查、存活/就绪端点、脱敏结构化日志、优雅退出和 systemd 配置。

SSH 加密内容对网关不可见。SSH 仓库和读写权限由 GitHub 控制，HTTP 仓库白名单不适用于 SSH。

## 快速开始

需要 `rustup`，仓库固定 Rust 1.94.1：

```sh
cargo build --locked --release
cp config/example.toml config.local.toml
HTTPS_PROXY=http://127.0.0.1:7890 \
ALL_PROXY=socks5h://127.0.0.1:7891 \
NO_PROXY=localhost,127.0.0.1 \
./target/release/gh-proxy --config config.local.toml
```

启动后打开 [http://127.0.0.1:8080/](http://127.0.0.1:8080/) 查看 Web 控制台。页面也支持手机浏览；使用配置中的 `public_url` 域名及端口访问。设置 `web_enabled=false` 可关闭控制台。

首次启动会在配置文件旁创建私有管理密钥。进入「设置」解锁后可直接编辑并保存配置：

```sh
cat config.local.admin-token
```

也可通过 `GH_PROXY_ADMIN_TOKEN` 提供至少 32 字符的管理密钥。密钥不写入日志，控制台只在当前页面内存中保存它。

代理地址需要按实际环境修改。示例仅允许本机与本仓库的只读访问，Release 允许查询和下载。未配置可用上游时出站拒绝；明确需要直连时设置 `direct=true`。生产客户端 CIDR、仓库权限、域名和证书均需自行配置。

```sh
git clone http://127.0.0.1:8080/class-undefined/gh-proxy.git
curl --fail http://127.0.0.1:8080/health/ready
```

明文只允许绑定 loopback；携带凭据应使用 HTTPS。部署、TLS、SSH 客户端和 Release 命令见运行文档。

## 文档与验证

- [构建、配置、安装与运行](docs/operations.md)
- [界面与组件设计](docs/design.md)
- [需求规格](docs/requirements.md)
- [架构与安全边界](docs/architecture.md)
- [实施计划与验收清单](docs/implementation-plan.md)
- [验证记录与已知限制](docs/verification.md)

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
# 前端测试需要 Node.js 20+；生产运行不需要 Node.js
node --test web/tests/*.test.mjs
```

已完成本地自动化测试及真实 GitHub 只读连通性验证。实际服务器部署、真实仓库写入和 Release 发布仍需在指定环境验收。每完成一项独立工作创建一次原子提交。
