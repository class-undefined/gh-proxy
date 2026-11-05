import {
  RELEASE_LABELS,
  buildCommands,
  filterRepositories,
  formatUptime,
} from "./view-model.js";

const PANEL_LABELS = Object.freeze({
  overview: "服务概览",
  repositories: "仓库策略",
  access: "访问与出站",
  guide: "接入指南",
});
const byId = (id) => document.getElementById(id);
const state = {
  snapshot: null,
  protocol: "https",
  selected: "",
  command: "",
  pending: false,
};
let toastTimeout;

function text(id, value) {
  const node = byId(id);
  if (node.textContent !== String(value)) node.textContent = value;
}
function element(tag, className, value) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (value !== undefined) node.textContent = value;
  return node;
}
function icon(name) {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("aria-hidden", "true");
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", `#i-${name}`);
  svg.append(use);
  return svg;
}
function notify(message) {
  clearTimeout(toastTimeout);
  text("toast", message);
  byId("toast").hidden = false;
  toastTimeout = setTimeout(() => {
    byId("toast").hidden = true;
  }, 3500);
}
function navigate() {
  const view = location.hash.slice(1) || "overview";
  const active = Object.hasOwn(PANEL_LABELS, view) ? view : "overview";
  document.querySelectorAll("[data-panel]").forEach((panel) => {
    panel.hidden = panel.dataset.panel !== active;
  });
  document.querySelectorAll("[data-nav]").forEach((link) => {
    const selected = link.dataset.nav === active;
    link.classList.toggle("active", selected);
    if (selected) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  });
  text("breadcrumb", PANEL_LABELS[active]);
  document.title = `${PANEL_LABELS[active]} · gh-proxy`;
}
function statusTag(node, ready, disabled = false) {
  node.classList.remove("neutral", "good", "warn");
  node.classList.add(disabled ? "neutral" : ready ? "good" : "warn");
  node.textContent = disabled ? "未启用" : ready ? "配置就绪" : "待配置出站";
}
function renderRepositories() {
  if (!state.snapshot) return;
  const all = state.snapshot.repositories;
  const repositories = filterRepositories(
    all,
    byId("repository-search").value,
    byId("repository-filter").value,
  );
  const rows = repositories.map((repository) => {
    const row = element("tr");
    const nameCell = element("td");
    const name = element("span", "repository-name");
    name.append(icon("repo"), document.createTextNode(repository.name));
    nameCell.append(name);
    const accessCell = element("td");
    accessCell.append(
      element(
        "span",
        `tag ${repository.write ? "warn" : "good"}`,
        repository.write ? "读写" : "只读",
      ),
    );
    const releaseCell = element("td");
    const tags = element("div", "permission-tags");
    Object.entries(RELEASE_LABELS).forEach(([key, label]) => {
      if (repository.release[key])
        tags.append(element("span", "tag neutral", label));
    });
    if (!tags.childElementCount)
      tags.append(element("span", "tag neutral", "未开放"));
    releaseCell.append(tags);
    const actionCell = element("td");
    const action = element("button", "repo-use", "接入 ↗");
    action.type = "button";
    action.setAttribute("aria-label", `查看 ${repository.name} 的接入命令`);
    action.addEventListener("click", () => {
      state.selected = repository.name;
      byId("guide-repository").value = state.selected;
      renderGuide();
      location.hash = "guide";
    });
    actionCell.append(action);
    row.append(nameCell, accessCell, releaseCell, actionCell);
    return row;
  });
  byId("repository-table").replaceChildren(...rows);
  byId("repository-empty").hidden = repositories.length > 0;
  text(
    "repository-empty",
    all.length
      ? "没有找到匹配的仓库，请调整搜索或权限筛选。"
      : "尚未配置授权仓库。请添加 repositories 配置并重启服务。",
  );
  text(
    "repository-count",
    `显示 ${repositories.length} / ${all.length} 个仓库`,
  );
}
function renderGuide() {
  if (!state.snapshot) return;
  const repository = state.snapshot.repositories.find(
    (repo) => repo.name === state.selected,
  );
  const model = buildCommands(state.snapshot, repository, state.protocol);
  state.command = model.command;
  text("command-title", model.title);
  text("client-command", model.command || "# 暂无可用命令");
  text("guide-description", model.description);
  text("guide-warning", model.warning);
  byId("guide-warning").hidden = !model.warning;
  text("command-footnote", model.footnote);
  byId("copy-command").disabled = !model.command;
  byId("command-panel").setAttribute(
    "aria-labelledby",
    `tab-${state.protocol}`,
  );
}
function render(snapshot) {
  const repositoriesChanged =
    JSON.stringify(state.snapshot?.repositories) !==
    JSON.stringify(snapshot.repositories);
  state.snapshot = snapshot;
  const number = (value) => new Intl.NumberFormat("zh-CN").format(value);
  text("top-version", `v${snapshot.version}`);
  text("sidebar-version", `v${snapshot.version}`);
  text("nav-repo-count", snapshot.repositories.length);
  text("gateway-origin", snapshot.public_url);
  statusTag(byId("gateway-badge"), snapshot.ready);
  byId("gateway-dot").className = `dot ${snapshot.ready ? "good" : "warn"}`;
  text(
    "gateway-status",
    snapshot.ready ? "出站路由配置就绪" : "等待配置出站路由",
  );
  text("hero-uptime", `运行 ${formatUptime(snapshot.uptime_seconds)}`);
  text("metric-repositories", number(snapshot.repositories.length));
  text(
    "metric-write",
    `${snapshot.repositories.filter((repository) => repository.write).length} 个允许写入 · 仅 HTTP/API`,
  );
  text("metric-requests", number(snapshot.requests));
  text("metric-denied", number(snapshot.denied));
  text("metric-failed", number(snapshot.failed));
  document
    .querySelectorAll(".route-http")
    .forEach((node) => statusTag(node, snapshot.http_ready));
  document
    .querySelectorAll(".route-ssh")
    .forEach((node) =>
      statusTag(node, snapshot.ssh_ready, !snapshot.ssh_targets.length),
    );
  byId("client-ranges").replaceChildren(
    ...snapshot.clients.map((range) => element("span", "chip", range)),
  );
  byId("ssh-targets").replaceChildren(
    ...(snapshot.ssh_targets.length
      ? snapshot.ssh_targets.map((target) =>
          element("span", "chip", `${target.host}:${target.port}`),
        )
      : [element("span", "chip", "未启用 SSH 目标")]),
  );
  text("flow-egress", snapshot.direct ? "代理 / 允许直连" : "仅显式上游代理");
  text(
    "http-route",
    snapshot.http_ready ? "配置就绪（未探测连通性）" : "尚未配置可用出站",
  );
  text(
    "ssh-route",
    snapshot.ssh_targets.length
      ? snapshot.ssh_ready
        ? "配置就绪（未探测连通性）"
        : "尚未配置可用出站"
      : "未启用 SSH 目标",
  );
  text("direct-rule", snapshot.direct ? "显式允许" : "拒绝直连");
  text("tls-rule", snapshot.tls ? "HTTPS" : "HTTP · 仅 loopback 开发入口");
  text("connection-limit", snapshot.max_connections);
  text("timeout-rule", `${number(snapshot.timeout_seconds)} 秒`);
  const selector = byId("guide-repository");
  if (repositoriesChanged) {
    selector.replaceChildren(
      ...(snapshot.repositories.length
        ? snapshot.repositories.map((repository) => {
            const option = element("option", "", repository.name);
            option.value = repository.name;
            return option;
          })
        : [element("option", "", "暂无授权仓库")]),
    );
  }
  if (!snapshot.repositories.some((repo) => repo.name === state.selected))
    state.selected = snapshot.repositories[0]?.name || "";
  selector.value = state.selected;
  selector.disabled = !snapshot.repositories.length;
  if (repositoriesChanged) renderRepositories();
  renderGuide();
}
async function refresh(manual = false) {
  if (state.pending) return;
  state.pending = true;
  document.querySelectorAll(".refresh").forEach((button) => {
    button.disabled = true;
  });
  try {
    const response = await fetch("/_ui/status", {
      cache: "no-store",
      signal: AbortSignal.timeout(10000),
      headers: { Accept: "application/json" },
    });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    render(await response.json());
    byId("connection-error").hidden = true;
    text(
      "last-updated",
      `更新于 ${new Date().toLocaleTimeString("zh-CN", { hour12: false })} · 每 15 秒刷新`,
    );
    if (manual) notify("已更新当前服务状态");
  } catch {
    text(
      "connection-error",
      "无法获取服务状态。当前显示的数据可能已过期，请检查连接或客户端访问权限后重试。",
    );
    byId("connection-error").hidden = false;
    text("last-updated", "连接中断 · 当前数据未更新");
    text("gateway-status", "状态暂不可用");
    byId("gateway-dot").className = "dot warn";
    byId("gateway-badge").className = "tag warn";
    text("gateway-badge", "连接中断");
    document.querySelectorAll(".route-http, .route-ssh").forEach((node) => {
      node.classList.remove("neutral", "good");
      node.classList.add("warn");
      node.textContent = "状态未更新";
    });
  } finally {
    state.pending = false;
    document.querySelectorAll(".refresh").forEach((button) => {
      button.disabled = false;
    });
  }
}
function selectProtocol(protocol) {
  state.protocol = protocol;
  document.querySelectorAll("[data-protocol]").forEach((button) => {
    const selected = button.dataset.protocol === protocol;
    button.setAttribute("aria-selected", String(selected));
    button.tabIndex = selected ? 0 : -1;
  });
  renderGuide();
}
async function copyCommand() {
  if (!state.command) return;
  try {
    await navigator.clipboard.writeText(state.command);
    notify("命令已复制");
  } catch {
    const selection = window.getSelection();
    const range = document.createRange();
    range.selectNodeContents(byId("client-command"));
    selection.removeAllRanges();
    selection.addRange(range);
    notify("浏览器未允许复制，已选中命令，请手动复制");
  }
}
document.querySelector(".skip-link").addEventListener("click", (event) => {
  event.preventDefault();
  byId("main").focus();
  byId("main").scrollIntoView();
});
window.addEventListener("hashchange", navigate);
document
  .querySelectorAll(".refresh")
  .forEach((button) => button.addEventListener("click", () => refresh(true)));
byId("repository-search").addEventListener("input", renderRepositories);
byId("repository-filter").addEventListener("change", renderRepositories);
byId("guide-repository").addEventListener("change", (event) => {
  state.selected = event.target.value;
  renderGuide();
});
byId("copy-command").addEventListener("click", copyCommand);
document.querySelectorAll("[data-protocol]").forEach((button) => {
  button.addEventListener("click", () =>
    selectProtocol(button.dataset.protocol),
  );
  button.addEventListener("keydown", (event) => {
    const tabs = [...document.querySelectorAll("[data-protocol]")];
    const index = tabs.indexOf(button);
    const next = {
      ArrowRight: (index + 1) % tabs.length,
      ArrowLeft: (index + tabs.length - 1) % tabs.length,
      Home: 0,
      End: tabs.length - 1,
    }[event.key];
    if (next !== undefined) {
      event.preventDefault();
      selectProtocol(tabs[next].dataset.protocol);
      tabs[next].focus();
    }
  });
});
navigate();
refresh();
setInterval(() => {
  if (!document.hidden) refresh();
}, 15000);
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) refresh();
});
