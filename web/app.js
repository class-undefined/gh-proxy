import {
  RELEASE_LABELS,
  buildCommands,
  filterRepositories,
  formatUptime,
} from "./view-model.js";
import "./settings.js";
const LABELS = {
  overview: "概览",
  repositories: "仓库",
  settings: "设置",
  guide: "接入",
};
const $ = (id) => document.getElementById(id);
const state = {
  snapshot: null,
  protocol: "https",
  selected: "",
  command: "",
  pending: false,
};
let toastTimeout;
function text(id, value) {
  const node = $(id);
  if (node.textContent !== String(value)) node.textContent = value;
}
function el(tag, cls, value) {
  const n = document.createElement(tag);
  n.className = cls || "";
  if (value !== undefined) n.textContent = value;
  return n;
}
function notify(message) {
  clearTimeout(toastTimeout);
  text("toast", message);
  $("toast").hidden = false;
  toastTimeout = setTimeout(() => {
    $("toast").hidden = true;
  }, 3000);
}
function navigate() {
  const view = Object.hasOwn(LABELS, location.hash.slice(1))
    ? location.hash.slice(1)
    : "overview";
  document.querySelectorAll("[data-panel]").forEach((p) => {
    p.hidden = p.dataset.panel !== view;
  });
  document.querySelectorAll("[data-nav]").forEach((a) => {
    a.classList.toggle("active", a.dataset.nav === view);
    if (a.dataset.nav === view) a.setAttribute("aria-current", "page");
    else a.removeAttribute("aria-current");
  });
  document.title = `${LABELS[view]} · gh-proxy`;
}
function tag(node, ready, disabled = false) {
  node.classList.remove("good", "warn", "neutral");
  node.classList.add(disabled ? "neutral" : ready ? "good" : "warn");
  node.textContent = disabled ? "未启用" : ready ? "已配置" : "未配置";
}
function renderRepositories() {
  if (!state.snapshot) return;
  const all = state.snapshot.repositories;
  const found = filterRepositories(
    all,
    $("repository-search").value,
    $("repository-filter").value,
  );
  $("repository-table").replaceChildren(
    ...found.map((repo) => {
      const row = el("tr");
      row.append(el("td", "repository-name", repo.name));
      const access = el("td");
      access.append(
        el(
          "span",
          `tag ${repo.write ? "warn" : "neutral"}`,
          repo.write ? "读写" : "只读",
        ),
      );
      const release = el("td");
      const tags = el("div", "permission-tags");
      Object.entries(RELEASE_LABELS).forEach(([key, label]) => {
        if (repo.release[key])
          tags.append(el("span", "permission-label", label));
      });
      if (!tags.childElementCount) tags.append(el("span", "muted", "—"));
      release.append(tags);
      const action = el("td");
      const button = el("button", "text-button", "接入 ↗");
      button.type = "button";
      button.setAttribute("aria-label", `${repo.name} 接入命令`);
      button.addEventListener("click", () => {
        state.selected = repo.name;
        $("guide-repository").value = repo.name;
        renderGuide();
        location.hash = "guide";
      });
      action.append(button);
      row.append(access, release, action);
      return row;
    }),
  );
  $("repository-empty").hidden = found.length > 0;
  text("repository-empty", all.length ? "没有匹配的仓库" : "尚未添加仓库");
  text("repository-count", `${found.length} / ${all.length}`);
}
function renderGuide() {
  if (!state.snapshot) return;
  const repo = state.snapshot.repositories.find(
    (r) => r.name === state.selected,
  );
  const model = buildCommands(state.snapshot, repo, state.protocol);
  state.command = model.command;
  text("client-command", model.command || "# 暂无可用命令");
  text(
    "command-title",
    state.protocol === "api" ? "Release" : state.protocol.toUpperCase(),
  );
  text("guide-warning", model.warning);
  $("guide-warning").hidden = !model.warning;
  $("copy-command").disabled = !model.command;
  $("command-panel").setAttribute("aria-labelledby", `tab-${state.protocol}`);
}
function render(snapshot) {
  const changed =
    JSON.stringify(state.snapshot?.repositories) !==
    JSON.stringify(snapshot.repositories);
  state.snapshot = snapshot;
  text("sidebar-version", `v${snapshot.version}`);
  text("nav-repo-count", snapshot.repositories.length);
  text("gateway-origin", snapshot.public_url);
  text("hero-uptime", `运行 ${formatUptime(snapshot.uptime_seconds)}`);
  [
    ["repositories", snapshot.repositories.length],
    ["requests", snapshot.requests],
    ["denied", snapshot.denied],
    ["failed", snapshot.failed],
  ].forEach(([key, n]) =>
    text(`metric-${key}`, new Intl.NumberFormat("zh-CN").format(n)),
  );
  document
    .querySelectorAll(".route-http")
    .forEach((n) => tag(n, snapshot.http_ready));
  document
    .querySelectorAll(".route-ssh")
    .forEach((n) => tag(n, snapshot.ssh_ready, !snapshot.ssh_targets.length));
  const select = $("guide-repository");
  if (changed) {
    select.replaceChildren(
      ...(snapshot.repositories.length
        ? snapshot.repositories.map((r) => {
            const option = el("option", "", r.name);
            option.value = r.name;
            return option;
          })
        : [el("option", "", "暂无仓库")]),
    );
    renderRepositories();
  }
  if (!snapshot.repositories.some((r) => r.name === state.selected))
    state.selected = snapshot.repositories[0]?.name || "";
  select.value = state.selected;
  select.disabled = !snapshot.repositories.length;
  renderGuide();
}
async function refresh(manual = false) {
  if (state.pending) return;
  state.pending = true;
  document.querySelectorAll(".refresh").forEach((b) => (b.disabled = true));
  try {
    const response = await fetch("/_ui/status", {
      cache: "no-store",
      signal: AbortSignal.timeout(10000),
    });
    if (!response.ok) throw new Error();
    render(await response.json());
    $("connection-error").hidden = true;
    text(
      "last-updated",
      `更新于 ${new Date().toLocaleTimeString("zh-CN", { hour12: false })}`,
    );
    if (manual) notify("已刷新");
  } catch {
    text("connection-error", "连接中断，显示最近一次数据。");
    $("connection-error").hidden = false;
    text("last-updated", "未连接");
    document.querySelectorAll(".route-http,.route-ssh").forEach((n) => {
      tag(n, false);
      n.textContent = "未更新";
    });
  } finally {
    state.pending = false;
    document.querySelectorAll(".refresh").forEach((b) => (b.disabled = false));
  }
}
function protocol(value) {
  state.protocol = value;
  document.querySelectorAll("[data-protocol]").forEach((b) => {
    const selected = b.dataset.protocol === value;
    b.setAttribute("aria-selected", String(selected));
    b.tabIndex = selected ? 0 : -1;
  });
  renderGuide();
}
$("copy-command").addEventListener("click", async () => {
  if (!state.command) return;
  try {
    await navigator.clipboard.writeText(state.command);
    notify("已复制");
  } catch {
    const selection = window.getSelection();
    const range = document.createRange();
    range.selectNodeContents($("client-command"));
    selection.removeAllRanges();
    selection.addRange(range);
    notify("已选中，请手动复制");
  }
});
$("repository-search").addEventListener("input", renderRepositories);
$("repository-filter").addEventListener("change", renderRepositories);
$("guide-repository").addEventListener("change", (event) => {
  state.selected = event.target.value;
  renderGuide();
});
document
  .querySelectorAll(".refresh")
  .forEach((b) => b.addEventListener("click", () => refresh(true)));
document.querySelectorAll("[data-protocol]").forEach((b) => {
  b.addEventListener("click", () => protocol(b.dataset.protocol));
  b.addEventListener("keydown", (event) => {
    const tabs = [...document.querySelectorAll("[data-protocol]")];
    const index = tabs.indexOf(b);
    const next = {
      ArrowRight: (index + 1) % tabs.length,
      ArrowLeft: (index + tabs.length - 1) % tabs.length,
      Home: 0,
      End: tabs.length - 1,
    }[event.key];
    if (next !== undefined) {
      event.preventDefault();
      protocol(tabs[next].dataset.protocol);
      tabs[next].focus();
    }
  });
});
document.querySelector(".skip-link").addEventListener("click", (event) => {
  event.preventDefault();
  $("main").focus();
  $("main").scrollIntoView();
});
window.addEventListener("hashchange", navigate);
window.addEventListener("configuration-saved", () => {
  refresh();
  notify("配置已应用");
});
navigate();
refresh();
setInterval(() => {
  if (!document.hidden) refresh();
}, 15000);
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) refresh();
});
