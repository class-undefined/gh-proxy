import assert from "node:assert/strict";
import test from "node:test";
import {
  proxyMode,
  proxyValue,
  proxyPlaceholder,
  parseClients,
  blankRepository,
  validateSettings,
} from "../settings-model.js";
test("proxy source distinguishes inheritance, disabled, and explicit values", () => {
  assert.equal(proxyMode(null), "inherit");
  assert.equal(proxyMode(""), "disabled");
  assert.equal(proxyMode("http://localhost:9"), "custom");
  assert.equal(proxyValue("inherit", "old"), null);
  assert.equal(proxyValue("disabled", "old"), "");
  assert.equal(
    proxyPlaceholder("https_proxy", "inherit", {}),
    "未设置 HTTPS_PROXY",
  );
  assert.equal(
    proxyPlaceholder("all_proxy", "inherit", { all_proxy: true }),
    "已由 ALL_PROXY 提供",
  );
  assert.match(proxyPlaceholder("all_proxy", "custom"), /^socks5h:/);
  assert.equal(
    proxyValue("custom", " http://localhost:9 "),
    "http://localhost:9",
  );
});
test("repository drafts are independent and reject duplicate or ambiguous names", () => {
  const a = blankRepository(),
    b = blankRepository();
  a.release.query = true;
  assert.equal(b.release.query, false);
  const settings = {
    clients: ["127.0.0.1/32"],
    repositories: [{ ...a, name: "owner/repo" }],
    max_connections: 128,
    timeout_seconds: 300,
  };
  assert.equal(validateSettings(settings), "");
  settings.repositories.push({ ...b, name: "Owner/Repo" });
  assert.match(validateSettings(settings), /重复/);
  settings.repositories = [{ ...a, name: "owner/repo.git" }];
  assert.match(validateSettings(settings), /格式/);
  settings.repositories = [{ ...a, name: "../repo" }];
  assert.match(validateSettings(settings), /格式/);
});
test("client parsing and service limits preserve useful validation", () => {
  assert.deepEqual(parseClients("127.0.0.1/32\n ::1/128,10.0.0.0/8\n"), [
    "127.0.0.1/32",
    "::1/128",
    "10.0.0.0/8",
  ]);
  const settings = {
    clients: [],
    repositories: [],
    max_connections: 1,
    timeout_seconds: 1,
  };
  assert.match(validateSettings(settings), /CIDR/);
  settings.clients = ["127.0.0.1/32"];
  settings.max_connections = 0;
  assert.match(validateSettings(settings), /并发/);
  settings.max_connections = 1;
  settings.timeout_seconds = 86401;
  assert.match(validateSettings(settings), /时限/);
});
