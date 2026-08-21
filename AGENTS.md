# AGENTS.md — DSH Desktop

面向 AI 编码代理（codex/claude 等）的项目上下文基线。先读本文，再读 README.md。

## 项目本质

Tauri 2 壳 + Rust `dsh-sidecar` 监督器 + 内置 Node 24 运行官方 DeepSeek
Harness（`@deepseek-ai/dsh`，**不 fork**，pin 版本）。桌面层只做生命周期：
Tauri 管窗口/托盘，sidecar 管 Harness 进程树，Harness Web UI 原样加载。

## 不可违背的安全不变量

- harness 窗口 capability 为空集；远程 webview **零 IPC 面**，且导航只允许
  就绪时捕获的 origin（`same_origin`，见 `harness/mod.rs`）。
- 桌面命令经 app ACL（`src-tauri/build.rs` AppManifest）只授权 bootstrap。
- 外部 deep link（`dsharness://plugin/install`）只产生「待确认安装请求」，
  绝不静默安装；协议/包名/来源在 Rust 侧全量重校验后才可进入 UI。
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
| `runtime/runtime-manifest.json` | desktop/harness/node/sidecar 版本 + nodeSha256（6 平台） |
| `package.json`（根） | desktop 版本 |
| `src-tauri/Cargo.toml` / `tauri.conf.json` | desktop 版本 + `dsh-sidecar` 依赖的 version pin（与 crates/dsh-sidecar 同步；cargo-deny wildcards=deny 要求非空版本） |
| `crates/dsh-sidecar/Cargo.toml` | sidecar 版本 |
| `runtime/package.json` + `package-lock.json` | harness pin（`npm install` 刷新锁） |
| `.nvmrc` | CI 脚本 Node（== manifest.nodeVersion） |

`scripts/lib/node-distribution.ts` 是由 manifest 生成的下载路径白名单，不是第二
个版本事实源；Node bump 时执行 `pnpm runtime:node:generate` 并提交结果。
`release:preflight` 会拒绝未同步的生成文件。

发布前必跑 `pnpm release:preflight`（tag 推送时 CI 亦跑并做 tag 绑定）。

## Harness 升级启动契约（bump pin 前必读）

DSH CLI 的启动契约随版本演进，社区同类项目已实证踩坑（如较新版本要求
`--expose-internals`；README 可能滞后于实现，勿尽信）。升级 harness 时：

1. bump `runtime/package.json` pin + `npm install` 刷新 lock +
   manifest.harnessVersion；
2. 对照新版 `dsh --help` / `dsh web --help` 复核契约并记录变化（当前钉
   0.1.0-rc.8 的基线：`web` = `--profile web` 别名；flag 仅
   `--host`/`--port`/`--trusted-host`/`--no-open`（Desktop 启动必须显式
   `--no-open`，避免上游再拉起系统浏览器）；**无** `--expose-internals`/
   `--use-system-ca`；就绪行 MARKER 为 `dsh web: http://127.0.0.1:`；
   `DSH_TELEMETRY_DISABLED` 任意非空即关闭 session 遥测）；
3. 若契约变化，同步三端：sidecar `extract_local_url` MARKER、
   Tauri `start_harness` args、`verify-runtime.ts` 冒烟断言；
4. 全量冒烟（`verify-runtime` + `verify-heartbeat`）+ golden 测试复核。

同一清单还须复核**插件与预设子命令契约**（升级后）：

- 对照新版复核 `dsh plugin --profile <name> <args...>`（requiredOption
  --profile、参数原样转发 pnpm、`spawnSync("pnpm", {shell: win32,
  cwd: profiles/<name>})`、initProfile 产物 package.json/cordis.patch.yml/
  pnpm-workspace.yaml(nodeLinker:hoisted)、reconcilePlugins 的
  `dsh.profile.bundles` 语义）。变化时同步 `src-tauri/src/plugins.rs` 注释、
  `scripts/verify-plugins.ts` 断言与 `verify-bundle.ts` 必含文件；
- 上游若新增 `.dshpreset` 归档导入/导出入口（或变更 `.agent-presets`
  根语义），必须复核壳层预设边界（`src-tauri/src/preset.rs`）——当前
  rc.8 无归档入口，壳层导入/导出是预设根的唯一写入路径，壳层健康复核
  （validate_user_presets）覆盖该根的全部来源。
  已知语义差异（有意为之，随上游演进复核）：壳层 Broken 只探测
  agent.cordis.yml 缺失/不可读/为空（不重实现 YAML 解析，可读但畸形仍由
  上游 roster 标 broken）；壳层删除预设是直接移除目录，不清理上游
  `settings.default`/standing mount——UI 已提示改选默认预设；删除/导出/
  导入拒绝对 `.agent-presets` 根为符号链接的路径操作。

Dependabot 对 harness 的 ignore 不作用于 security updates：若收到
`@deepseek-ai/dsh` 的 security PR（只改 pin+lock、不动 manifest），必须按本
清单补全（manifest + lock + 全量冒烟）后再合并——CI 的 version-drift 断言会
自动挡住不完整合入。

## 前端依赖升级门槛（typescript 等）

- **typescript 只允许人工跨 major**：TS 7（tsgo）超出 svelte-check 支持矩阵
  （peer `^5 || ^6`）；Dependabot 已按 `semver-major` ignore，升级前先核
  svelte-check 的 peer 声明。security update 不受 ignore 约束，收到 TS
  的 security PR 按本门槛人工评估。
- **TS ≥ 6 依赖 `src/vite-env.d.ts`**（`/// <reference types="vite/client" />`）：
  `noUncheckedSideEffectImports` 自 TS 6.0 默认开启，缺失会让 CSS 副作用导入
  报 TS2882（PR #1 事件根因）。
- 升级后必跑：`pnpm check && pnpm check:scripts && pnpm build`。

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
- `src-tauri/src/deep_link.rs` — `dsharness://plugin/install` 协议解析、
  待确认请求槽、事件分发（冷/热启动双路径）
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
4. 新 deep-link 变化是否同步 `deep_link.rs` 协议版本、市场侧契约与测试？
5. 版本字段是否全部同步？
6. 供应链：新依赖（Rust/npm）是否过 deny/vet/白名单？
7. 安装包内容断言是否需要更新（新增必需文件/禁止链接范围）？
