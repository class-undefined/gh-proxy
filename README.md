# gh-proxy

使用 **Rust** 实现的 Git 访问网关，首期支持 GitHub：Git HTTP(S)、SSH 透明转发、Release API 与附件传输，共用来源策略和显式出站代理配置。

## 功能

- Git Smart HTTP：保留 `/owner/repo.git`，按仓库白名单控制读取与写入，覆盖 clone、fetch、pull、push 和 tag 推送。
- SSH：受限 SOCKS5 CONNECT，保留端到端 SSH 身份认证，限制客户端、GitHub 目标及端口；内置 `connect` 客户端，无需 connect-proxy 或 nc。
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

## 客户端使用方案

SSH 客户端安装同一个二进制即可，无需启动本地网关服务或复制服务端配置：

```sh
cargo install --locked --path .
gh-proxy connect --help
```

也可直接使用构建产物的绝对路径。以下 SSH 示例假定 `gh-proxy` 在客户端 PATH 中，网关 SOCKS 入口为 `127.0.0.1:1080`；远程部署时替换为实际网关地址，并在服务端允许客户端来源及目标。Git 和 OpenSSH 仍使用客户端已有密钥，网关管理密钥不参与 Git 认证。

| 方案 | 影响范围 | 仓库地址 |
| --- | --- | --- |
| Git HTTP(S) | 使用网关 URL 的请求 | 网关域名下的原生仓库路径 |
| SSH 原生地址 | 当前用户匹配 github.com 的 SSH 连接 | 保留 `git@github.com:owner/repo.git` |
| SSH 别名 | 使用别名的 SSH 连接 | 使用 `git@github-gateway:owner/repo.git` |
| SSH 单次命令 | 当前 Git 命令 | 保留原生 SSH 地址 |
| SSH 仓库配置 | 指定本地仓库的 SSH 远端 | 保留原生 SSH 地址 |

### Git HTTP(S)

直接使用网关仓库地址，或通过 URL 改写保留原地址：

```sh
git clone https://git-gateway.example/owner/repo.git

# 可选：将该客户端的 GitHub HTTPS 地址统一改写到网关。
git config --global url."https://git-gateway.example/".insteadOf https://github.com/
git clone https://github.com/owner/repo.git
```

私有仓库可为明确的网关配置客户端凭据助手，复用本机 gh / Git 的 GitHub 登录：

```sh
git config --global credential."https://git-gateway.example".helper \
  '!gh-proxy credential --gateway https://git-gateway.example'
```

HTTP 仓库及读写权限由网关白名单和 GitHub 共同约束。Token、SSO 和移除配置的方法见 [认证文档](docs/authentication.md)。

### SSH 保留原生地址

在 `~/.ssh/config` 添加：

```sshconfig
Host github.com
    User git
    ProxyCommand gh-proxy connect --socks 127.0.0.1:1080 %h %p
```

```sh
git clone git@github.com:owner/repo.git
git -C repo fetch
git -C repo push
git -C repo push --tags
```

此配置适用于当前用户匹配该 Host 的 SSH 连接，不改写 HTTPS 地址或系统 DNS。SSH 专用规则应放在通用 `Host *` 规则之前。[OpenSSH 配置规则](https://man.openbsd.org/ssh_config#Host)

### SSH 别名

需要同时保留直连和网关路径时，可单独增加别名：

```sshconfig
Host github-gateway
    HostName github.com
    User git
    ProxyCommand gh-proxy connect --socks 127.0.0.1:1080 %h %p
```

```sh
git clone git@github-gateway:owner/repo.git
# 已有仓库也可只修改其远端地址。
git -C repo remote set-url origin git@github-gateway:owner/repo.git
```

原有 `github.com` 连接继续按其他 SSH 配置处理。

### SSH 单次命令

无需修改 SSH 配置：

```sh
GIT_SSH_COMMAND='ssh -o "ProxyCommand=gh-proxy connect --socks 127.0.0.1:1080 %h %p"' \
  git clone git@github.com:owner/repo.git
```

### SSH 仅用于某个仓库

对已有本地仓库设置，之后正常使用 fetch、pull、push：

```sh
git -C repo config core.sshCommand \
  'ssh -o "ProxyCommand=gh-proxy connect --socks 127.0.0.1:1080 %h %p"'
git -C repo fetch
```

移除该设置：`git -C repo config --unset core.sshCommand`。以上环境变量及命令引号示例适用于 POSIX shell。

### SSH 443 与连接参数

当网络限制 22 端口时，可将原生地址方案改为：

```sshconfig
Host github.com
    HostName ssh.github.com
    Port 443
    User git
    ProxyCommand gh-proxy connect --socks 127.0.0.1:1080 %h %p
```

网关须允许 `ssh.github.com:443`，原生 Git SSH 地址保持不变。使用 `ssh -T git@github.com` 检查身份，再用 `git ls-remote git@github.com:owner/repo.git` 检查仓库权限；GitHub 的 `ssh -T` 认证成功通常退出码为 1。

`connect --socks <host:port> <目标域名> <端口>` 只向指定 SOCKS 网关建连，目标以域名交给网关处理。IPv6 网关写作 `[::1]:1080`。默认连接及 SOCKS 握手时限为 15 秒，可用 `--connect-timeout 30` 调整；该参数不限制后续 SSH 会话时长，服务端仍执行自己的会话时限。失败返回非零退出码，错误写 stderr，stdout 只传输隧道字节；不会继承客户端代理变量或失败后回退直连。

SSH 公钥认证和仓库权限始终由 GitHub 校验；保持主机密钥校验，网关的 HTTP 仓库白名单不约束 SSH。

## 文档与验证

- [构建、配置、安装与运行](docs/operations.md)
- [HTTP / SSH 认证与故障排查](docs/authentication.md)
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
