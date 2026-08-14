# AGENTS.md — DeepSeek Harness Desktop

面向 AI 编码代理（codex/claude 等）的项目上下文基线。先读本文，再读 README.md。

## 项目本质

Tauri 2 壳 + Rust `dsh-sidecar` 监督器 + 内置 Node 24 运行官方 DeepSeek
Harness（`@deepseek-ai/dsh`，**不 fork**，pin 版本）。桌面层只做生命周期：
Tauri 管窗口/托盘，sidecar 管 Harness 进程树，Harness Web UI 原样加载。

## 不可违背的安全不变量

- harness 窗口 capability 为空集；远程 webview **零 IPC 面**，且导航只允许
  就绪时捕获的 origin（`same_origin`，见 `harness/mod.rs`）。
- 桌面命令经 app ACL（`src-tauri/build.rs` AppManifest）只授权 bootstrap。
- `DSH_HOME` 0700、拒绝符号链接；`withGlobalTauri: false` + CSP。
- 进程树保证：unix 进程组 + sigaction；Windows Job Object + 隐藏控制台
  CTRL_C 优雅关闭（`platform.rs`）。任何改动不得弱化。
- **零符号链接不变量限定范围**：`src-tauri/resources/runtime/harness` 与安装包内
  `runtime/harness` 子树必须完全物化（`materialize.ts` + `check-runtime-links.ts`
  断言）。macOS .app 框架自身的系统级链接不在其列。
- NDJSON 协议契约与 golden 测试（`ndjson_golden_events`）同改：改事件结构
  必须同时更新 sidecar、Tauri `apply_state_event`、`verify-runtime.ts` 三端。

## 版本单一事实源（改动版本时必须同时改全部）

| 文件 | 内容 |
|---|---|
| `runtime/runtime-manifest.json` | desktop/harness/node/sidecar 版本 + nodeSha256（5 平台） |
| `package.json`（根） | desktop 版本 |
| `src-tauri/Cargo.toml` / `tauri.conf.json` | desktop 版本 |
| `crates/dsh-sidecar/Cargo.toml` | sidecar 版本 |
| `runtime/package.json` + `package-lock.json` | harness pin（`npm install` 刷新锁） |
| `.nvmrc` | CI 脚本 Node（== manifest.nodeVersion） |

发布前必跑 `pnpm release:preflight`（tag 推送时 CI 亦跑并做 tag 绑定）。

## 禁区清单

- **不改 Harness Web UI / 上游代码**；只 pin npm 包。
- `scripts/` 零 npm 依赖（Node 24 原生 TS），新脚本先过
  `pnpm check:scripts`（`tsconfig.scripts.json`，strict）。
- 不绕过 `runtime/.npmrc` 的 `strict-allow-scripts` 白名单；新依赖带安装
  脚本时必须先审再入名单。
- 下载类脚本必须有 SHA-256 校验（`download-node.ts` 模式）。
- 不做 symlink 依赖的复制；一律走 `materialize`。
- 生产代码禁用 unwrap/expect/panic（clippy deny；测试模块豁免）。

## 关键文件地图

- `crates/dsh-sidecar/src/main.rs` — 监督循环、NDJSON、信号、行截断
- `crates/dsh-sidecar/src/platform.rs` — unix 进程组 / Windows Job+console
- `src-tauri/src/harness/mod.rs` — 状态机（纯函数 `apply_state_event`）、
  sidecar 生命周期、`publish_snapshot` 统一发布通道、`request_restart`
- `src-tauri/src/tray.rs` — 托盘（`tray_available` 两级策略）
- `src-tauri/src/commands.rs` — ACL 化 IPC 命令
- `scripts/lib/materialize.ts` — 物化器（硬链接 + 根约束）
- `scripts/verify-bundle.ts` / `checksums.ts` — 安装包内容与哈希断言

## 验证命令（改动后至少跑对应的）

```bash
pnpm check && pnpm check:scripts          # 前端 + 脚本类型
cargo nextest run --manifest-path crates/dsh-sidecar/Cargo.toml
cargo nextest run --manifest-path src-tauri/Cargo.toml
cargo llvm-cov nextest --manifest-path crates/dsh-sidecar/Cargo.toml --fail-under-lines 50
cargo llvm-cov nextest --manifest-path src-tauri/Cargo.toml --fail-under-lines 25
cargo fmt --check --manifest-path <crate> && cargo clippy --manifest-path <crate> --all-targets -- -D warnings
cargo deny --manifest-path <crate> check && cargo vet --locked
node scripts/verify-runtime.ts            # 冒烟（含 --runtime-dir 重定位）
node scripts/verify-bundle.ts --self-test
pnpm release:preflight
```

## Review 检查清单

1. 新状态变更是否经过 `publish_snapshot`（托盘/UI 不能陈旧）？
2. 新进程操作是否保持树清理保证（EOF/信号/Job Object 三路径）？
3. 新 IPC/命令是否在 ACL + capability 中显式授权？
4. 版本字段是否全部同步？
5. 供应链：新依赖（Rust/npm）是否过 deny/vet/白名单？
6. 安装包内容断言是否需要更新（新增必需文件/禁止链接范围）？
