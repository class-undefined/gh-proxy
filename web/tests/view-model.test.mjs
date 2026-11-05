import assert from "node:assert/strict";
import test from "node:test";
import {
  buildCommands,
  filterRepositories,
  formatUptime,
  shellQuote,
} from "../view-model.js";
const read = {
  name: "owner/repo",
  write: false,
  release: { query: true, download: true },
};
const write = { ...read, name: "owner/write", write: true };
const status = {
  public_url: "https://gateway.example/",
  socks_port: 1080,
  http_ready: true,
  ssh_ready: true,
  ssh_targets: [{ host: "github.com", port: 22 }],
};
test("repository filtering intersects case-insensitive search and permissions", () => {
  assert.deepEqual(filterRepositories([read, write], "OWNER/", "read"), [read]);
  assert.deepEqual(filterRepositories([read, write], "write", "write"), [
    write,
  ]);
  assert.deepEqual(filterRepositories([read, write], "absent"), []);
});
test("read-only Git commands omit pushes and point at the configured gateway", () => {
  const model = buildCommands(status, read, "https");
  assert.match(model.command, /https:\/\/gateway.example\/owner\/repo.git/);
  assert.doesNotMatch(model.command, /push/);
  assert.match(buildCommands(status, write, "https").command, /push --tags/);
  assert.match(
    buildCommands(
      { ...status, public_url: "http://127.0.0.1:8080/" },
      read,
      "https",
    ).warning,
    /凭据/,
  );
});
test("SSH keeps the original repository address and handles the 443 target", () => {
  assert.match(
    buildCommands(status, read, "ssh").command,
    /connect-proxy -S gateway.example:1080 %h %p/,
  );
  assert.match(
    buildCommands(status, read, "ssh").command,
    /git@github.com:owner\/repo.git/,
  );
  const model = buildCommands(
    { ...status, ssh_targets: [{ host: "ssh.github.com", port: 443 }] },
    read,
    "ssh",
  );
  assert.match(model.command, /HostName ssh.github.com\n    Port 443/);
  assert.equal(
    buildCommands({ ...status, ssh_targets: [] }, read, "ssh").command,
    "",
  );
});
test("Release samples only include permitted actions and never embed tokens", () => {
  const model = buildCommands(status, read, "api");
  assert.match(model.command, /api\/github\/repos\/owner\/repo\/releases/);
  assert.match(model.command, /\$GITHUB_TOKEN/);
  assert.doesNotMatch(model.command, /-X POST/);
  assert.match(
    buildCommands(status, { ...read, release: { create: true } }, "api")
      .command,
    /-X POST/,
  );
  assert.equal(
    buildCommands(status, { ...read, release: {} }, "api").command,
    "",
  );
});
test("empty state and shell escaping remain usable", () => {
  assert.equal(buildCommands(status, null, "https").command, "");
  assert.equal(shellQuote("a'b"), "'a'\\''b'");
  assert.equal(formatUptime(12), "不足 1 分钟");
  assert.equal(formatUptime(90061), "1 天 1 小时");
});

test("API samples use a single shell continuation and require HTTPS for credentials", () => {
  const command = buildCommands(status, read, "api").command;
  for (const line of command
    .split("\n")
    .filter((line) => line.includes("\\"))) {
    assert.equal(line.endsWith("\\"), true);
    assert.equal(line.endsWith("\\\\"), false);
  }
  assert.equal(
    buildCommands(
      { ...status, public_url: "http://127.0.0.1:8080/" },
      read,
      "api",
    ).command,
    "",
  );
});
