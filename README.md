# DeepSeek Harness Desktop

**社区桌面发行层**：把官方 DeepSeek Harness 打包成 Windows / macOS 原生应用。
不是 fork——真正的 Harness 完整保留（Node.js + node_modules + Cordis 插件），
桌面层只负责生命周期：Tauri 管窗口，Rust sidecar 管 Harness 进程。

仓库：https://github.com/web-casa/DeepSeek-Harness-Desktop · 下载：https://github.com/web-casa/DeepSeek-Harness-Desktop/releases
文档：[SECURITY.md](SECURITY.md) · [FORKING.md](FORKING.md) · [RELEASING.md](RELEASING.md) · [AGENTS.md](AGENTS.md)
English: [README.en.md](README.en.md)
（v0.2.2：Windows x64 NSIS / macOS arm64 DMG，未签名预览版）

```
┌────────────────────────────────────────────────┐
│ Tauri 2（bootstrap 窗口 + Harness 窗口）         │
│  bootstrap: 本地 Svelte UI，有限的桌面 IPC        │
│  harness:   http://127.0.0.1:<随机端口>          │
│             原版 Harness Web UI · 零 IPC 权限     │
└───────────────┬────────────────────────────────┘
                │ NDJSON (stdin/stdout)
┌───────────────▼────────────────────────────────┐
│ dsh-sidecar（Rust，无重依赖）                     │
│  启动/停止/重启 · readiness 捕获 · 崩溃检测        │
│  Unix: process group    Windows: Job Object      │
└───────────────┬────────────────────────────────┘
                │
┌───────────────▼────────────────────────────────┐
│ 内置 Node 24 + @deepseek-ai/dsh（固定版本）       │
│   node lib/bin.js web --host 127.0.0.1 --port 0  │
│   数据目录 DSH_HOME 与 CLI 的 ~/.dsh 隔离         │
└────────────────────────────────────────────────┘
```

## 核心原则

1. **不 fork Harness**：`runtime/package.json` 只 pin `@deepseek-ai/dsh`；
   上游发版后改一个版本号 + CI 冒烟即可。
2. **不改 Harness Web UI**：桌面端直接加载原版 Web UI（`--port 0` 自动端口 +
   官方 readiness 行 `dsh web: http://127.0.0.1:<port>` 即握手协议）。
3. **安全边界**：Harness 窗口的 capability 为空集——远程内容没有任何
   Tauri IPC 面；桌面自己的命令通过 app ACL 只授予 bootstrap 窗口。
4. **版本单一事实源**：`runtime/runtime-manifest.json` 固定
   desktop / harness / node / sidecar 四个版本。

## 目录结构

```
Cargo.toml                  cargo workspace 根（单 Cargo.lock、release profile）
crates/dsh-sidecar/         Rust 监督器（独立 crate，三平台可编译）
runtime/                    pin 版本 + npm package-lock + 脚本白名单（node-pty/koffi 等）
scripts/                   下载 Node / 准备 runtime / 构建 sidecar / 端到端冒烟
src/                       bootstrap 前端（Svelte 5 + Vite）
src-tauri/                 Tauri 壳：状态机、ACL 化命令、capabilities、打包配置
deny.toml                  cargo-deny 供应链策略（license/advisory/bans/sources）
supply-chain/              cargo-vet 审核记录（audits/config/imports）
.github/workflows/         test / release / codeql / dependency-review
```

## 开发

```bash
pnpm install                       # 前端 + tauri CLI
pnpm check && pnpm check:scripts   # 前端 svelte-check + 脚本 tsc 类型检查
pnpm runtime:all                   # 下载 Node + 准备 Harness runtime + 构建 sidecar
pnpm runtime:verify                # 端到端冒烟（sidecar→node→dsh web→HTTP 200→无孤儿）
pnpm tauri dev                     # 桌面开发模式
pnpm icons                         # 从 icon-source.png 重新生成平台图标
```

> 本机若 `~/.cargo` 不可写，请设置 `CARGO_HOME=<repo>/.tmp/cargo-home` 再构建。

### 质量门

| 层 | 工具 | 门槛 |
|---|---|---|
| 前端 | `tsc --noEmit` + `svelte-check` | 0 error（含 Node 原生 TS 构建脚本的独立 `tsconfig.scripts.json`） |
| Rust 测试 | `cargo nextest` | sidecar 25 + Tauri 状态机 10（含 proptest 性质测试、平台集成测试、NDJSON golden 契约） |
| Rust 覆盖率 | `cargo llvm-cov` | sidecar ≥ 50%（platform.rs 96.7%）、Tauri ≥ 25%（重点覆盖进程生命周期/状态机，不追全局 KPI） |
| Rust 静态分析 | `cargo fmt --check` + `clippy -D warnings` | unwrap/expect/panic/todo/unimplemented/dbg_macro 一律 deny（测试模块豁免） |
| 供应链 | `cargo-deny` | advisories（yanked=deny）/ licenses / bans / sources（unknown=deny） |
| 供应链审计 | `cargo-vet --locked` | 490 个第三方 crate 显式豁免基线；新增 crate 必须审核或豁免 |
| 安全扫描 | CodeQL | rust + javascript-typescript + actions（push/PR/周 cron） |
| PR 依赖门禁 | Dependency Review | fail-on-severity low + GPL/AGPL/LGPL 拒绝（依赖图启用后生效） |
| 安装包 | `verify-bundle.ts` + `checksums.ts` | 7z/hdiutil 内容断言 + 内置 manifest 与仓库版本一致 + SHA-256 产物 |

### 脚本说明（Node ≥ 24 原生跑 TS，零依赖）

| 脚本 | 作用 |
|---|---|
| `download-node.ts` | 按 manifest 版本下载官方 Node 二进制（SHA-256 校验，平台/架构自动映射） |
| `prepare-harness.ts` | runtime/ 下 `npm ci`（扁平布局）后物化（零符号链接）复制到 bundle resources；交叉校验 manifest 与安装版本 |
| `build-sidecar.ts` | `cargo build --release --target <host-triple>` 并暂存到 resources/runtime |
| `verify-runtime.ts` | 冒烟：boot → readiness → HTTP 200 → status → restart → HTTP 200 → shutdown → 孤儿进程检查 → sidecar 随父退出；支持 `--runtime-dir`（重定位验证） |
| `check-runtime-links.ts` | 断言 staged harness 树零符号链接 |
| `relocate-runtime.ts` | 物化复制 runtime 到 `.tmp` 供重定位冒烟 |
| `verify-bundle.ts` | 安装包内容断言：NSIS 用 7z、DMG 用 hdiutil；校验主二进制类型、runtime 全树、平台 node-pty prebuild、零符号链接；`--self-test` 可在任意平台跑解析器测试 |
| `lib/materialize.ts` | 物化器：递归展开符号链接/junction，文件硬链接零额外空间 |

## 预设与插件

- **预设（Agent Presets）**：`.dshpreset` 压缩包（`preset.yml` + `agent.cordis.yml`）。
  Harness Web UI 自带预设管理；桌面设置页另提供**安全导入/导出**：导入前
  做路径/符号链接/配额校验与密钥扫描，两阶段确认后原子安装到
  `<dshHome>/.agent-presets/`（与上游发现机制一致）。**预设与 Agent 同权限
  运行，仅导入可信来源。**
- **插件（pnpm 线）**：`dsh plugin --profile web add <pkg>` 走系统 pnpm，
  工作区在 `<dshHome>/profiles/web/`。桌面版需手动执行（用诊断页显示的
  dshHome + 内置 CLI，见 SECURITY.md 信任模型）。

## dsh-sidecar 协议（NDJSON，stdin 命令 / stdout 事件）

```text
→ {"id":1,"command":"start","node":"…","script":"…/lib/bin.js",
   "args":["web","--host","127.0.0.1","--port","0"],
   "cwd":"…/harness","env":{"DSH_HOME":"…"}}
← {"type":"ack","id":1,"ok":true} → {"type":"starting"} → {"type":"ready","url":"…"}
→ {"command":"restart" | "shutdown" | "status"}
← {"type":"stopped","code":0} · {"type":"crashed","code":1} · {"type":"status",…}
[stdin EOF]  → 树优雅退出，sidecar exit 0
```

超时可用 `DSH_READY_TIMEOUT_MS`（默认 120s）/ `DSH_SHUTDOWN_GRACE_MS`（默认 10s）调。

### 进程树清理保证

| 场景 | 清理路径 |
|---|---|
| 正常退出 / 关闭应用 | Tauri `RunEvent::Exit` → shutdown 命令 → 优雅停止（unix: SIGTERM 进程组；win: CTRL_C → node SIGINT → dsh 优雅退出）→ 超时后强杀 |
| 应用崩溃（无信号波及 sidecar） | sidecar stdin EOF 检测 → 强杀整棵树 → exit 0 |
| 整组信号（如终端 Ctrl+C / `timeout`） | sidecar 的 SIGTERM/SIGINT/SIGHUP 处理器 → 清理后 exit 0 |
| Windows 强杀兜底 | Job Object `KILL_ON_JOB_CLOSE`；优雅失败时 2 秒后 `TerminateJobObject` |
| 备注 | Windows 的优雅路径依赖 sidecar 启动时分配的隐藏控制台 + 子进程继承（`platform.rs`）；无控制台时优雅不可达，自动落入强杀兜底 |

### 许可与第三方归属

- 本项目代码：MIT（见 `LICENSE`）。
- 安装包内置原版 DeepSeek Harness（MIT，随包附其 LICENSE/README）与 npm 依赖树；
  `prepare-harness` 会把每个顶层依赖的 LICENSE 收集到包内 `runtime/harness/licenses/`。

## CI

- **test.yml**：ubuntu 上 sidecar 单测 + 三 target 交叉编译检查 + 前端构建 +
  bundle 校验器自测；三平台（ubuntu/windows/macos-14）各跑一遍完整 runtime 冒烟。
- **release.yml**：tag 触发 → 冒烟 → 打包（NSIS/DMG）→ **安装包内容断言**
  （`verify-bundle.ts`）→ 产物上传 → draft release。签名/公证 secrets 已留位（P3）。
  `workflow_dispatch` 为只构建+验证的测试通道（不发布）。

## 版本升级流程

```
Dependabot/Renovate 提议 @deepseek-ai/dsh rc.x → rc.y
  → 改 runtime/package.json + runtime-manifest.json
  → cd runtime && npm install（刷新 package-lock.json；新脚本需先加 .npmrc 白名单）
  → CI 三平台冒烟全绿
  → tag v* 发版（新 Node/新 Harness 随 Desktop 一起更新）
```

## 当前状态与已知边界（v0.2.2）

- ✅ CI 四平台全绿（ubuntu / windows-latest / macos-14 + Linux 单测），Windows
  冒烟含 boot → readiness → HTTP 200 → restart → shutdown → 无孤儿 + 重定位冒烟
- ✅ v0.2.2 draft 就绪（Windows updater 首发，见 releases 页）；v0.2.1 已发布；v0.2.0 为旧构建
- ✅ 单实例锁、窗口状态记忆、崩溃自动恢复、diagnostics、macOS 关闭=隐藏
- ✅ 隐私默认值：会话遥测默认关闭（`DSH_TELEMETRY_DISABLED=1`，详见 SECURITY.md）；
  子进程环境消毒（`NODE_OPTIONS`/`NODE_PATH`/`npm_config_*` 不会进入 Harness）
- ✅ Windows 自动更新（更新包由内嵌 minisign 公钥校验；macOS 更新器待签名+公证后启用）
- ⏳ 未接入：代码签名 / 公证 / 插件安装（bundled pnpm）
- 未签名构建：Windows SmartScreen、macOS Gatekeeper 需要用户手动放行
- Linux 仅为开发环境，不作为发行目标；node-pty 在 Linux dev 下无 prebuild
  （Web UI 启动不受影响；Windows/macOS 发行包自带对应 prebuild）
