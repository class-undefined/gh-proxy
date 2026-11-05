export const PROXY_FIELDS = Object.freeze({
  http_proxy: "HTTP",
  https_proxy: "HTTPS",
  all_proxy: "SSH",
  no_proxy: "NO_PROXY",
});
export function proxyMode(value) {
  return value === null || value === undefined
    ? "inherit"
    : value === ""
      ? "disabled"
      : "custom";
}
export function proxyValue(mode, value) {
  return mode === "inherit" ? null : mode === "disabled" ? "" : value.trim();
}
export function parseClients(value) {
  return value
    .split(/[\n,]+/)
    .map((v) => v.trim())
    .filter(Boolean);
}
export function blankRepository() {
  return {
    name: "",
    write: false,
    release: {
      query: false,
      create: false,
      update: false,
      upload: false,
      download: false,
      delete: false,
    },
  };
}
export function validateSettings(settings) {
  if (!settings.clients.length) return "请添加客户端 CIDR";
  const names = new Set();
  for (const repo of settings.repositories) {
    if (
      !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repo.name) ||
      repo.name.endsWith(".git") ||
      repo.name.split("/").some((p) => p === "." || p === "..")
    )
      return "仓库格式应为 owner/repo，不含 .git";
    const name = repo.name.toLowerCase();
    if (names.has(name)) return "仓库不能重复";
    names.add(name);
  }
  if (
    !Number.isInteger(settings.max_connections) ||
    settings.max_connections < 1 ||
    settings.max_connections > 65536
  )
    return "并发上限为 1–65536";
  if (
    !Number.isInteger(settings.timeout_seconds) ||
    settings.timeout_seconds < 1 ||
    settings.timeout_seconds > 86400
  )
    return "时限为 1–86400 秒";
  return "";
}
