import { RELEASE_LABELS } from "./view-model.js";
import {
  PROXY_FIELDS,
  proxyMode,
  proxyValue,
  parseClients,
  blankRepository,
  validateSettings,
} from "./settings-model.js";
const $ = (id) => document.getElementById(id);
const state = {
  token: "",
  document: null,
  draft: null,
  dirty: false,
  busy: false,
  clear: new Set(),
};
function el(tag, cls, value) {
  const node = document.createElement(tag);
  node.className = cls || "";
  if (value !== undefined) node.textContent = value;
  return node;
}
function error(id, message) {
  $(id).textContent = message;
  $(id).hidden = !message;
}
function dirty() {
  state.dirty = true;
  $("save-settings").disabled = state.busy;
  $("discard-settings").disabled = state.busy;
  $("settings-state").textContent = "未保存";
}
function labelCheckbox(text, checked, change) {
  const label = el("label", "checkbox-label");
  const input = el("input");
  input.type = "checkbox";
  input.checked = checked;
  input.addEventListener("change", () => {
    change(input.checked);
    dirty();
  });
  label.append(input, document.createTextNode(text));
  return label;
}
function repositories() {
  const rows = state.draft.repositories.map((repo, index) => {
    const row = el("tr");
    const name = el("td");
    const input = el("input", "repo-input");
    input.value = repo.name;
    input.placeholder = "owner/repo";
    input.required = true;
    input.setAttribute("aria-label", `仓库 ${index + 1}`);
    input.addEventListener("input", () => {
      repo.name = input.value.trim();
      dirty();
    });
    name.append(input);
    const permission = el("td");
    const select = el("select");
    select.setAttribute("aria-label", `仓库 ${index + 1} Git 权限`);
    for (const [value, text] of [
      ["read", "只读"],
      ["write", "读写"],
    ]) {
      const option = el("option", "", text);
      option.value = value;
      select.append(option);
    }
    select.value = repo.write ? "write" : "read";
    select.addEventListener("change", () => {
      repo.write = select.value === "write";
      dirty();
    });
    permission.append(select);
    const release = el("td");
    const checks = el("div", "release-checks");
    Object.entries(RELEASE_LABELS).forEach(([key, label]) =>
      checks.append(
        labelCheckbox(label, repo.release[key], (value) => {
          repo.release[key] = value;
        }),
      ),
    );
    release.append(checks);
    const remove = el("td");
    const button = el("button", "text-button remove", "移除");
    button.type = "button";
    button.setAttribute("aria-label", `移除仓库 ${index + 1}`);
    button.addEventListener("click", () => {
      state.draft.repositories.splice(index, 1);
      repositories();
      dirty();
    });
    remove.append(button);
    row.append(name, permission, release, remove);
    return row;
  });
  $("settings-repositories").replaceChildren(...rows);
  $("settings-repositories-empty").hidden = rows.length > 0;
}
function proxies() {
  const rows = Object.entries(PROXY_FIELDS).map(([key, label]) => {
    const row = el("div", "proxy-row");
    const title = el("label", "", label);
    const input = el("input");
    input.id = `proxy-${key}`;
    input.value = state.draft.upstream[key] || "";
    input.autocomplete = "off";
    input.spellcheck = false;
    input.setAttribute("aria-label", `${label} 代理配置`);
    input.placeholder =
      key === "no_proxy" ? "localhost,127.0.0.1" : "http://127.0.0.1:7890";
    title.htmlFor = input.id;
    const mode = el("select");
    mode.setAttribute("aria-label", `${label} 配置来源`);
    for (const [value, name] of [
      ["inherit", "环境变量"],
      ["custom", "自定义"],
      ["disabled", "停用"],
    ]) {
      const option = el("option", "", name);
      option.value = value;
      mode.append(option);
    }
    mode.value = proxyMode(state.draft.upstream[key]);
    input.disabled = mode.value !== "custom";
    mode.addEventListener("change", () => {
      input.disabled = mode.value !== "custom";
      state.draft.upstream[key] = proxyValue(mode.value, input.value);
      dirty();
    });
    input.addEventListener("input", () => {
      state.draft.upstream[key] = proxyValue(mode.value, input.value);
      dirty();
    });
    row.append(title, mode, input);
    if (state.document.proxy_credentials.includes(key)) {
      const clear = el("button", "text-button", "清除凭据");
      clear.type = "button";
      clear.dataset.secretKey = key;
      clear.addEventListener("click", () => {
        state.clear.add(key);
        clear.disabled = true;
        clear.textContent = "待清除";
        dirty();
      });
      row.append(clear);
    }
    return row;
  });
  $("settings-proxies").replaceChildren(...rows);
}
function renderDocument(document) {
  state.document = document;
  state.draft = structuredClone(document.settings);
  state.clear.clear();
  state.dirty = false;
  repositories();
  proxies();
  $("settings-clients").value = state.draft.clients.join("\n");
  $("settings-direct").checked = state.draft.direct;
  $("settings-connections").value = state.draft.max_connections;
  $("settings-timeout").value = state.draft.timeout_seconds;
  $("ssh-github").checked = state.draft.ssh_targets.some(
    (t) => t.host === "github.com",
  );
  $("ssh-443").checked = state.draft.ssh_targets.some(
    (t) => t.host === "ssh.github.com",
  );
  $("save-settings").disabled = true;
  $("discard-settings").disabled = true;
  $("settings-state").textContent = "已保存";
  $("settings-form").hidden = false;
  $("unlock-form").hidden = true;
  $("lock-settings").hidden = false;
  error("settings-error", "");
}
async function request(method, body) {
  const response = await fetch("/_ui/config", {
    method,
    cache: "no-store",
    signal: AbortSignal.timeout(15000),
    headers: {
      Authorization: `Bearer ${state.token}`,
      ...(body ? { "Content-Type": "application/json" } : {}),
    },
    ...(body ? { body: JSON.stringify(body) } : {}),
  });
  const value = await response.json();
  if (!response.ok) {
    const e = new Error(value.message || "操作失败");
    e.status = response.status;
    throw e;
  }
  return value;
}
$("unlock-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const button = event.submitter;
  button.disabled = true;
  $("unlock-form").setAttribute("aria-busy", "true");
  state.token = $("admin-token").value.trim();
  try {
    renderDocument(await request("GET"));
    $("admin-token").value = "";
    error("unlock-error", "");
  } catch (e) {
    state.token = "";
    error("unlock-error", e.message || "无法连接");
  } finally {
    button.disabled = false;
    $("unlock-form").setAttribute("aria-busy", "false");
  }
});
$("lock-settings").addEventListener("click", () => {
  state.token = "";
  state.document = null;
  state.draft = null;
  state.dirty = false;
  $("settings-form").hidden = true;
  $("settings-repositories").replaceChildren();
  $("settings-proxies").replaceChildren();
  $("unlock-form").hidden = false;
  $("lock-settings").hidden = true;
  $("admin-token").focus();
});
$("add-repository").addEventListener("click", () => {
  state.draft.repositories.push(blankRepository());
  repositories();
  dirty();
  $("settings-repositories").lastElementChild.querySelector("input").focus();
});
$("settings-clients").addEventListener("input", () => {
  state.draft.clients = parseClients($("settings-clients").value);
  dirty();
});
$("settings-direct").addEventListener("change", () => {
  state.draft.direct = $("settings-direct").checked;
  dirty();
});
for (const [id, key] of [
  ["settings-connections", "max_connections"],
  ["settings-timeout", "timeout_seconds"],
])
  $(id).addEventListener("input", () => {
    state.draft[key] = Number($(id).value);
    dirty();
  });
for (const id of ["ssh-github", "ssh-443"])
  $(id).addEventListener("change", () => {
    state.draft.ssh_targets = [
      ...($("ssh-github").checked ? [{ host: "github.com", port: 22 }] : []),
      ...($("ssh-443").checked ? [{ host: "ssh.github.com", port: 443 }] : []),
    ];
    dirty();
  });
$("discard-settings").addEventListener("click", async () => {
  state.busy = true;
  $("lock-settings").disabled = true;
  $("save-settings").disabled = true;
  $("discard-settings").disabled = true;
  try {
    renderDocument(await request("GET"));
  } catch (e) {
    error("settings-error", e.message);
  } finally {
    state.busy = false;
    $("lock-settings").disabled = false;
    $("save-settings").disabled = !state.dirty;
    $("discard-settings").disabled = !state.dirty;
  }
});
$("settings-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const message = validateSettings(state.draft);
  if (message) {
    error("settings-error", message);
    return;
  }
  state.busy = true;
  $("lock-settings").disabled = true;
  $("save-settings").disabled = true;
  $("discard-settings").disabled = true;
  $("settings-state").textContent = "保存中…";
  $("settings-form").setAttribute("aria-busy", "true");
  const fieldsets = [
    ...$("settings-form").querySelectorAll("input,select,textarea,button"),
  ];
  fieldsets.forEach((n) => (n.disabled = true));
  try {
    const result = await request("PUT", {
      revision: state.document.revision,
      settings: state.draft,
      clear_credentials: [...state.clear],
    });
    renderDocument(result);
    window.dispatchEvent(new Event("configuration-saved"));
  } catch (e) {
    error("settings-error", e.message);
    $("settings-state").textContent = "未保存";
  } finally {
    state.busy = false;
    $("lock-settings").disabled = false;
    $("settings-form").setAttribute("aria-busy", "false");
    fieldsets.forEach((n) => {
      if (n.isConnected) n.disabled = false;
    });
    document.querySelectorAll(".proxy-row").forEach((row) => {
      row.querySelector("input").disabled =
        row.querySelector("select").value !== "custom";
      const clear = row.querySelector("[data-secret-key]");
      if (clear) clear.disabled = state.clear.has(clear.dataset.secretKey);
    });
    $("save-settings").disabled = !state.dirty;
    $("discard-settings").disabled = !state.dirty;
  }
});
