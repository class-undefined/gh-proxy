# 客户端认证与故障排查

网关先检查来源、目标和操作，再把客户端凭据交给 GitHub 验证。管理密钥只用于控制台设置，不用于 clone、push、SSH 或 GitHub API。

| 凭据 | 由谁验证 | 配置位置 |
| --- | --- | --- |
| GitHub HTTPS 凭据 / PAT | GitHub | 客户端 Git credential helper |
| SSH 私钥 | GitHub 验证对应公钥 | 客户端 SSH / ssh-agent |
| 上游代理用户名和密码 | 上游 HTTP / SOCKS 代理 | 控制台出站代理或代理环境变量 |
| 管理密钥 | 网关管理接口 | 私有 `.admin-token` 文件或 `GH_PROXY_ADMIN_TOKEN` |

## HTTP 私有仓库

匿名请求收到 GitHub 的 `401` 和 `WWW-Authenticate` 后，Git 才获取凭据并重试。用户名填写 GitHub 用户名，密码位置使用有仓库权限的 PAT；GitHub 账户登录密码不能用于 Git HTTPS。[GitHub 认证说明](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/about-authentication-to-github)

Git 按协议和主机区分凭据。`github.com`、`127.0.0.1:8080` 和部署后的网关域名属于不同上下文，URL 改写不会自动复用原主机凭据。[Git credential contexts](https://git-scm.com/docs/gitcredentials)

本项目提供**客户端** credential helper，将明确授权的一个网关 origin 映射到 `https://github.com`。它先查询本机 `gh auth git-credential`，再查询本机 Git credential helper；未取得凭据时由外层 Git 正常提示输入。安装该二进制的客户端可先用单次配置验证：

```sh
# 在 gh-proxy 仓库目录执行；helper 路径必须指向客户端上的二进制。
git -c "credential.http://127.0.0.1:8080.helper=!'$PWD/target/release/gh-proxy' credential --gateway http://127.0.0.1:8080" \
  clone http://127.0.0.1:8080/owner/repo.git
```

长期使用时显式配置一个网关地址，以下以安装到 `/usr/local/bin` 为例：

```sh
git config --global credential."https://git-gateway.example".helper \
  '!/usr/local/bin/gh-proxy credential --gateway https://git-gateway.example'
git clone https://git-gateway.example/owner/repo.git
```

本机开发把这两处 origin 都改为 `http://127.0.0.1:8080`。远程网关必须使用 HTTPS；helper 只允许 loopback 使用 HTTP，并且只响应与 `--gateway` 完全匹配的 origin。不要注册一个任意目标通用的凭据映射。helper 不需要网关配置文件或管理密钥，不把 Token 写到网关配置；store/erase 转交本机 GitHub 凭据管理器。`gh auth login --hostname github.com` 可用于在客户端建立登录，请勿在网关上集中登录所有用户身份。

移除示例配置：

```sh
git config --global --unset-all credential."https://git-gateway.example".helper
```

网关保留 GitHub 的 401、403、认证挑战和 `X-GitHub-SSO`。凭据有效但仓库不可见时，也可能返回 404；请检查 PAT 权限、组织 SSO 授权及该用户的仓库权限。Release API 使用客户端提供的 Authorization，附件跨域跳转移除认证头。

## SSH

客户端保留自己的私钥、ssh-agent 和 GitHub 主机密钥校验。客户端安装 `gh-proxy` 后，在 `~/.ssh/config` 配置：

```sshconfig
Host github.com
    User git
    ProxyCommand gh-proxy connect --socks 127.0.0.1:1080 %h %p
```

此命令只做客户端 SOCKS 隧道，不需要管理密钥或本地网关配置文件。多种接入方案见 [README](../README.md#客户端使用方案)。服务端必须配置可用的 `ALL_PROXY`（例如 `socks5h://127.0.0.1:7890`）或明确允许直连，并允许相应 SSH 目标。

```sh
ssh -T git@github.com
git ls-remote git@github.com:owner/repo.git
git clone git@github.com:owner/repo.git
```

`ssh -T` 成功时 GitHub 提示已认证但不提供 shell，退出码为 1，这是正常行为；仓库权限仍需用 `git ls-remote` 验证。[GitHub SSH 验证说明](https://docs.github.com/en/authentication/connecting-to-github-with-ssh/testing-your-ssh-connection)

`Permission denied (publickey)` 表示已到达 SSH 平台，但客户端提供的公钥未获授权；检查 `ssh-add -l`、`IdentityFile` 和 GitHub 公钥设置。仓库拒绝则检查 GitHub 用户/部署密钥的仓库权限。网关无法读取加密认证过程，也不将“隧道建立”报告为“用户认证成功”。

SOCKS 入口依赖来源白名单，不要求客户端提交网关管理密钥；此处的 SOCKS 上游用户名/密码属于网关到上游代理的 RFC 1929 认证，与 GitHub SSH 公钥无关。

## 出站故障

先访问 `/health/ready` 或查看控制台的入口状态。路由就绪只说明配置存在，仍需实际请求检验连通性。继承环境变量时，控制台会显示变量是否存在，默认占位地址不会作为真实代理配置。

网关生成的 HTTP 错误带 `X-Gh-Proxy-Error`；GitHub 的认证状态直接透传，不加该错误头。日志只包含固定错误码，不输出底层错误链或带凭据的代理 URL。

| HTTP 状态 | 错误码或来源 | 处理 |
| --- | --- | --- |
| 503 | `egress_not_configured` | 配置适用的 HTTPS_PROXY / ALL_PROXY，或明确允许直连 |
| 503 | `direct_bypass_denied` | NO_PROXY 命中但禁止直连，检查绕过规则 |
| 503 | `connection_limit_reached` | 等待活跃传输释放容量 |
| 502 | `upstream_proxy_auth_required` | 检查 HTTP 407 或 SOCKS 上游用户名/密码 |
| 502 | `upstream_connect_failed` | 检查代理监听地址、端口和连通性 |
| 502 | `target_dns_rejected` | 检查 DNS；Fake-IP 场景使用 HTTP CONNECT 或 socks5h |
| 504 | `upstream_timeout` | 检查代理/平台可达性与传输时限 |
| 401 | GitHub | 客户端提供 GitHub 凭据 |
| 403 | 网关策略或 GitHub | 检查来源、仓库、操作，以及用户权限/SSO |

SSH 错误采用相同的静态错误码记录于 `tunnel_failed` 日志。上游认证拒绝后不会尝试匿名重连，也不会回退直连。
