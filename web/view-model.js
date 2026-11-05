export const RELEASE_LABELS = Object.freeze({
  query: "查询",
  create: "创建",
  update: "更新",
  upload: "上传",
  download: "下载",
  delete: "删除",
});

export function filterRepositories(
  repositories,
  query = "",
  permission = "all",
) {
  const normalized = query.trim().toLowerCase();
  return repositories.filter(
    (repository) =>
      repository.name.toLowerCase().includes(normalized) &&
      (permission === "all" ||
        (permission === "write" ? repository.write : !repository.write)),
  );
}

export function formatUptime(seconds) {
  if (seconds < 60) return "不足 1 分钟";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} 分钟`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时 ${minutes % 60} 分钟`;
  return `${Math.floor(hours / 24)} 天 ${hours % 24} 小时`;
}

export function shellQuote(value) {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

export function buildCommands(status, repository, protocol) {
  if (!repository)
    return {
      title: "CLIENT / SETUP",
      command: "",
      description:
        "尚未配置 HTTP/API 授权仓库。请先在 TOML 中添加仓库策略并重启服务。",
      warning: "",
      footnote: "SSH 目标与仓库策略分别管理",
    };
  const base = new URL(status.public_url);
  const name = repository.name;
  if (protocol === "ssh") {
    const hostname = base.hostname;
    const target =
      status.ssh_targets.find(
        (target) => target.host === "github.com" && target.port === 22,
      ) ??
      status.ssh_targets.find(
        (target) => target.host === "ssh.github.com" && target.port === 443,
      );
    if (!target)
      return {
        title: "CLIENT / SSH",
        command: "",
        description: "当前未开放 SSH 目标。",
        warning: "请先配置 ssh_targets，或选择其他入口。",
        footnote: "不开放任意 SOCKS 目标",
      };
    const settings = [
      "# 添加到 ~/.ssh/config",
      "Host github.com",
      ...(target.host === "ssh.github.com"
        ? ["    HostName ssh.github.com", "    Port 443"]
        : []),
      "    User git",
      `    ProxyCommand connect-proxy -S ${hostname}:${status.socks_port} %h %p`,
      "",
      "# 沿用原有 GitHub SSH 地址",
      `git clone ${shellQuote(`git@github.com:${name}.git`)}`,
    ];
    return {
      title: "CLIENT / SSH",
      command: settings.join("\n"),
      description:
        "安装 connect-proxy，并添加 SSH 配置；GitHub 继续验证你的现有公钥。",
      warning: status.ssh_ready
        ? "SSH 仓库权限由 GitHub 决定，不受 HTTP 仓库白名单限制。"
        : "SSH 出站尚未配置，请先检查 ALL_PROXY 或直连策略。",
      footnote: "保持 SSH 主机密钥校验，不要禁用校验",
    };
  }
  if (protocol === "api") {
    const endpoint = `${base}api/github/repos/${name}/releases`;
    const commands = [];
    if (repository.release.query)
      commands.push(
        "# 查询 Release",
        `curl --fail-with-body \\`,
        `  -H "Authorization: Bearer $GITHUB_TOKEN" \\`,
        `  ${shellQuote(endpoint)}`,
      );
    if (repository.release.create)
      commands.push(
        "",
        "# 创建 Release（准备 release.json）",
        "curl --fail-with-body -X POST \\",
        '  -H "Authorization: Bearer $GITHUB_TOKEN" \\',
        "  -H 'Content-Type: application/json' \\",
        "  --data-binary @release.json \\",
        `  ${shellQuote(endpoint)}`,
      );
    if (repository.release.download)
      commands.push(
        "",
        "# 下载附件（替换 ASSET_ID）",
        "curl --fail-with-body \\",
        '  -H "Authorization: Bearer $GITHUB_TOKEN" \\',
        "  -H 'Accept: application/octet-stream' \\",
        "  -o artifact.bin \\",
        `  ${shellQuote(`${endpoint}/assets/ASSET_ID`)}`,
      );
    return {
      title: "CLIENT / RELEASE API",
      command: base.protocol === "https:" ? commands.join("\n").trim() : "",
      description:
        "使用你自己的 GitHub Token，通过仓库路径访问 Release API。命令只列出已允许的示例操作。",
      warning:
        base.protocol !== "https:"
          ? "当前是本机 HTTP 开发入口。携带 Token 前，请先配置 HTTPS。"
          : !status.http_ready
            ? "HTTP/API 出站尚未配置，请检查 HTTPS_PROXY 或直连策略。"
            : "",
      footnote: commands.length
        ? "GITHUB_TOKEN 仅保存在客户端环境中"
        : "此仓库未开放查询、创建或下载示例操作；完整权限见仓库策略",
    };
  }
  return {
    title: "CLIENT / GIT HTTP(S)",
    command: [
      "# 克隆仓库",
      `git clone ${shellQuote(`${base}${name}.git`)}`,
      "",
      "# 获取远端更新",
      `git -C ${shellQuote(name.split("/")[1])} fetch`,
      ...(repository.write
        ? [
            "",
            "# 推送分支和 tag",
            `git -C ${shellQuote(name.split("/")[1])} push`,
            `git -C ${shellQuote(name.split("/")[1])} push --tags`,
          ]
        : []),
    ].join("\n"),
    description:
      "保留原生仓库路径，使用自己的 GitHub 凭据。也可以在 Git 中设置 URL 改写。",
    warning:
      base.protocol !== "https:"
        ? "当前为本机 HTTP 开发入口；传输凭据前请使用 HTTPS。"
        : !status.http_ready
          ? "HTTP 出站尚未配置，请先检查 HTTPS_PROXY 或直连策略。"
          : "",
    footnote: repository.write
      ? "网关允许写入，GitHub 仍会校验你的仓库权限"
      : "当前仓库为只读；push 和 tag 推送会被拒绝",
  };
}
