# 多平台构建与发布

## 持续集成

分支 push、Pull Request 和手动 CI 运行都会检查格式、Clippy、前端测试、发布脚本测试和 workflow 语法，并在六个原生目标运行 Rust 测试、release 构建、`--version` 与 `connect --help`。CI 通过 `rust-toolchain.toml` 安装固定工具链；构建使用 `Cargo.lock`，第三方 Actions 固定到提交 SHA。

| Rust target | GitHub runner | 产物 |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | `ubuntu-22.04` | `.tar.gz` |
| `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` | `.tar.gz` |
| `x86_64-apple-darwin` | `macos-15-intel` | `.tar.gz` |
| `aarch64-apple-darwin` | `macos-15` | `.tar.gz` |
| `x86_64-pc-windows-msvc` | `windows-2022` | `.zip` |
| `aarch64-pc-windows-msvc` | `windows-11-arm` | `.zip` |

运行器依据 [GitHub 官方列表](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)。Linux 使用 glibc，不提供 Alpine/musl 包；macOS 设置 `MACOSX_DEPLOYMENT_TARGET=11.0`；Windows 设置静态 CRT，ARM64 使用 clang-cl 满足 [AWS-LC 构建要求](https://aws.github.io/aws-lc-rs/requirements/windows.html)。Unix 专用 credential helper 进程测试只在 Linux/macOS 执行，其余 Rust 测试在所有目标运行。

每个目标上传 `binary-<target>` Actions artifact，保留 7 天。Actions artifact 内部包含版本化的发行压缩包，Unix 可执行权限由 tar 文件保留。发行包只包含一个可执行文件，不打包配置、Token、管理密钥或本机运行文件。

## 发布版本

推送 `v<版本>` tag 自动触发 Release workflow。tag 必须与该提交 `Cargo.toml` 中的 package.version 完全一致；例如当前 `0.1.0` 对应 `v0.1.0`。该 workflow 自己执行全部 CI 检查，无需依赖另一次分支 CI 的完成状态。

项目已通过以下命令发布 [v0.1.0](https://github.com/class-undefined/gh-proxy/releases/tag/v0.1.0)，并完成六平台产物及校验和验证。以下为首次 tag 触发示例；该 tag 已存在，后续发布请按下一段递增版本号：

```sh
git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0
```

后续版本先修改 `Cargo.toml`，同步 `Cargo.lock` 并提交，再创建 tag。例如发布 0.2.0：

```sh
# 先将 Cargo.toml 的 version 改为 0.2.0。
cargo check
git add Cargo.toml Cargo.lock
git commit -m "chore: release v0.2.0"
git tag -a v0.2.0 -m "Release v0.2.0"
git push origin main
git push origin v0.2.0
```

预发布使用 `0.2.0-rc.1` / `v0.2.0-rc.1` 等匹配版本，自动标记为 prerelease，不替换稳定版 latest。

流程在六个平台全部成功后汇总产物，验证文件名、数量和包内内容，生成 `SHA256SUMS`。只有汇总成功才创建草稿 Release，上传全部六个包与校验文件，再公开发布并生成发行说明。创建与发布使用内置 `GITHUB_TOKEN`，无需额外 PAT；只有 publish job 获得 `contents: write`。依据：[GitHub CLI release create](https://cli.github.com/manual/gh_release_create)。

若构建失败，不会发布部分平台的 Release。若上传失败，保留草稿供修复后重跑；同一 tag 的流程串行运行，已公开的 Release 不会被自动覆盖。不要移动已发布 tag，需要修复已发布二进制时增加版本号。

## 手动验证流程

在 Actions 页面运行 Release，或使用：

```sh
gh workflow run release.yml --ref main
```

手动运行会执行相同的检查、六平台构建和产物汇总，提供包含六个压缩包及 `SHA256SUMS` 的 `release-bundle` artifact。手动运行不创建或发布 GitHub Release，即使选择 tag ref 也是如此。下载验证包：

```sh
gh run list --workflow release.yml
gh run download <RUN_ID> --name release-bundle --dir release-download
```

## 本地验证打包脚本

需要 Python 3.10+，仅使用标准库。以下命令不发布远端版本：

```sh
python3 -m unittest discover -s scripts/tests -v
python3 scripts/release.py version --tag v0.1.0

# 以 Apple Silicon 为例；其他平台替换为表中的 native target。
rustup target add aarch64-apple-darwin
cargo build --locked --release --target aarch64-apple-darwin
python3 scripts/release.py package --target aarch64-apple-darwin --version 0.1.0
```

准备齐六个平台的同版本压缩包后，执行 `python3 scripts/release.py checksums --version 0.1.0`。缺少目标、多出文件或包内出现配置文件时校验失败。默认输出 `dist/`，已加入 Git 忽略规则。

## 旧版 Linux 的静态构建

对于 CentOS 7 等较旧的 glibc 环境，可手动运行 Portable Linux workflow，构建不依赖系统动态加载器或共享库的 musl 二进制：

```sh
gh workflow run portable.yml --ref main -f target=x86_64-unknown-linux-musl
# ARM64 可将 target 改为 aarch64-unknown-linux-musl。
gh run list --workflow portable.yml
gh run download <RUN_ID> --name portable-x86_64-unknown-linux-musl --dir portable-download
cd portable-download
sha256sum -c SHA256SUMS
chmod 755 gh-proxy
./gh-proxy --version
```

该流程运行对应 target 的 Rust 测试与 release 构建，检查 ELF 不含动态解释器及共享库依赖，提供二进制、SHA256SUMS 和源码版本信息，artifact 保留 30 天。安装到旧系统前仍应在目标内核上执行运行检查。此构建入口不修改已有正式 Release 的资产。
