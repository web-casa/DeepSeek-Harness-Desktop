# DeepSeek Harness Desktop

**社区桌面发行层**：把官方 DeepSeek Harness 打包成 Windows / macOS 原生应用，
让 Harness 及其**插件生态**开箱即用。不是 fork——真正的 Harness 完整保留
（Node.js + node_modules + Cordis 插件体系），桌面层只负责生命周期与安全边界：
Tauri 管窗口，Rust sidecar 管 Harness 进程树。

仓库：https://github.com/web-casa/DeepSeek-Harness-Desktop · 下载：https://github.com/web-casa/DeepSeek-Harness-Desktop/releases
文档：[SECURITY.md](SECURITY.md) · [FORKING.md](FORKING.md) · [RELEASING.md](RELEASING.md) · [AGENTS.md](AGENTS.md)
官网：https://dsharness.app · **插件市场：https://cordis.run**
English: [README.en.md](README.en.md)
（v0.2.3：Windows x64 NSIS / macOS arm64 DMG，未签名预览版）

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
│  启动/停止/重启 · readiness · 挂死心跳 · 树清理    │
│  Unix: process group    Windows: Job Object      │
└───────────────┬────────────────────────────────┘
                │
┌───────────────▼────────────────────────────────┐
│ 内置 Node 24 + @deepseek-ai/dsh（固定版本）       │
│   node lib/bin.js web --host 127.0.0.1 --port 0  │
│   插件装进 DSH_HOME/profiles/web，不碰安装目录     │
└────────────────────────────────────────────────┘
```

## ✨ 插件生态

DeepSeek Harness 的原生能力来自 **Cordis 插件体系**：技能、工具、模型路由、
MCP 接入、预设……全部是插件。桌面版把整套生态随包携带，并额外提供安全护栏。

### 发现插件：cordis.run

应用设置页「资源」区一键打开官方**插件市场 [cordis.run](https://cordis.run)**，
浏览、挑选插件与预设，再把包名带回桌面版安装。

### 安装插件（`dsh plugin`）

插件是 npm 包，官方机制是 `dsh plugin --profile web add <pkg>`。桌面版使用
**与发行版完全一致的 pinned CLI** 手动执行（需系统已装 pnpm）：

```bash
# macOS
DSH_HOME="<诊断页的 dshHome>" node "/Applications/DeepSeek Harness Desktop.app/Contents/Resources/runtime/harness/node_modules/@deepseek-ai/dsh/lib/bin.js" plugin --profile web add <插件包名>

# Windows（PowerShell）
$env:DSH_HOME="<诊断页的 dshHome>"; node "<安装目录>\runtime\harness\node_modules\@deepseek-ai\dsh\lib\bin.js" plugin --profile web add <插件包名>
```

- `dshHome` 在应用**诊断页**显示，复制即可；`--profile web` 与桌面版运行的
  profile 一致；
- 插件装入 `<dshHome>/profiles/web/`（用户数据目录，**不写安装目录**）；
- 装完在应用里「重新启动 Harness」即生效。

### 预设（`.dshpreset`）：插件的「配置包」兄弟

预设是一个 zip（`preset.yml` + `agent.cordis.yml`），把一组插件行打包成可分享
的智能体配置。桌面设置页提供**安全导入/导出**：

- 导入前做路径/符号链接/配额（16/32/12 MiB、512 文件）/密钥扫描；
- 两阶段确认 + 原子安装到 `<dshHome>/.agent-presets/`——与上游发现机制一致，
  导入后 Harness 设置页立即可见；
- 导出同样过校验，且拒绝符号链接伪装。

### 信任模型（重要）

**插件与预设运行在 Harness 进程内，与 Agent 同权限**（上游原话："carries the
same trust as shell access"）。桌面的校验拦截的是恶意构造的压缩包，拦不住
「内容本身有害」的可信包——**只从可信来源安装**。详见 [SECURITY.md](SECURITY.md)。

### 桌面层为插件生态加的护栏

- 插件/预设落盘在用户数据目录，**不触碰安装目录**（零符号链接不变量）；
- 预设目录权限对齐上游（0700/0600）；
- 预设导入的「上游可发现性」由 CI e2e 直接驱动上游 `discoverPresets` 验证；
- 更新器保证你拿到的总是与新插件兼容的 Harness 版本。

## 核心原则

1. **不 fork Harness**：`runtime/package.json` 只 pin `@deepseek-ai/dsh`；
   上游发版后改一个版本号 + CI 冒烟即可。
2. **不改 Harness Web UI**：桌面端直接加载原版 Web UI（`--port 0` 自动端口 +
   官方 readiness 行 `dsh web: http://127.0.0.1:<port>` 即握手协议）。
3. **安全边界**：Harness 窗口的 capability 为空集——远程内容没有任何
   Tauri IPC 面；桌面自己的命令通过 app ACL 只授予 bootstrap 窗口。
4. **版本单一事实源**：`runtime/runtime-manifest.json` 固定
   desktop / harness / node / sidecar 四个版本。

## 桌面层特性

- ✅ **Windows 自动更新**：更新包由内嵌 minisign 公钥校验（与代码签名无关），
  「检查更新 / 安装并重启」在设置页；macOS 更新器待签名+公证后启用
- ✅ **挂死自愈**：sidecar 心跳检测 Harness「活着但无响应」，自动重启（退避+上限）
- ✅ **预设导入导出**：见上「插件生态」
- ✅ 单实例锁、窗口状态记忆、崩溃自动恢复、macOS 关闭=隐藏
- ✅ **诊断与反馈**：一键导出诊断 zip（尽力脱敏）、复制诊断、预填 issue 报告
- ✅ 隐私默认值：会话遥测默认关闭（`DSH_TELEMETRY_DISABLED=1`）；子进程环境
  消毒（`NODE_OPTIONS`/`NODE_PATH`/`npm_config_*`/动态链接器注入键不进 Harness）
- ✅ 崩溃视图动作：重试启动 / 导出诊断 / 退出应用

## 目录结构

```
Cargo.toml                  cargo workspace 根（单 Cargo.lock、release profile）
crates/dsh-sidecar/         Rust 监督器（独立 crate，三平台可编译）
runtime/                    pin 版本 + npm package-lock + 脚本白名单
scripts/                   下载 Node / 准备 runtime / 构建 sidecar / 端到端冒烟 / 预设与更新验证
src/                       bootstrap 前端（Svelte 5 + Vite）
src-tauri/                 Tauri 壳：状态机、ACL 化命令、预设安全边界、capabilities
deny.toml                  cargo-deny 供应链策略（license/advisory/bans/sources）
supply-chain/              cargo-vet 审核记录（audits/config/imports）
.github/workflows/         test / release（含 soak）/ codeql / dependency-review
```

## 开发

```bash
pnpm install                       # 前端 + tauri CLI
pnpm check && pnpm check:scripts   # 前端 svelte-check + 脚本 tsc 类型检查
pnpm runtime:all                   # 下载 Node + 准备 Harness runtime + 构建 sidecar
pnpm runtime:verify                # 端到端冒烟（sidecar→node→dsh web→HTTP 200→无孤儿）
pnpm test:scripts                  # node:test 单测（零依赖）
pnpm tauri dev                     # 桌面开发模式
pnpm icons                         # 从 icon-source.png 重新生成平台图标
```

> 本机若 `~/.cargo` 不可写，请设置 `CARGO_HOME=<repo>/.tmp/cargo-home` 再构建。

### 质量门

| 层 | 工具 | 门槛 |
|---|---|---|
| 前端 | `tsc --noEmit` + `svelte-check` | 0 error（含 Node 原生 TS 脚本的独立 `tsconfig.scripts.json`） |
| Rust 测试 | `cargo nextest` | sidecar 35 + Tauri 35（proptest、进程级集成、NDJSON golden、预设攻击面用例；Windows 宿主亦实跑） |
| Rust 覆盖率 | `cargo llvm-cov` | sidecar ≥ 50%、Tauri ≥ 25%（重点覆盖进程生命周期/状态机/预设边界） |
| Rust 静态分析 | `cargo fmt --check` + `clippy -D warnings` | unwrap/expect/panic 一律 deny（测试模块豁免） |
| 供应链 | `cargo-deny` | advisories（yanked=deny）/ licenses / bans / sources |
| 供应链审计 | `cargo-vet --locked` | 70 全审 + 2 delta + 豁免基线；新增 crate 必须审核或豁免 |
| npm 审计 | `npm audit`（runtime）+ `pnpm audit`（根） | `--audit-level=high` 阻断 |
| 安全扫描 | CodeQL | rust + javascript-typescript + actions |
| PR 依赖门禁 | Dependency Review | fail-on-severity low + GPL/AGPL/LGPL 拒绝 |
| 安装包 | `verify-bundle.ts` + `verify-signing.ts` + `checksums.ts` | 内容断言 + 签名状态（fail-closed）+ SHA-256 |

### 核心脚本（Node ≥ 24 原生跑 TS，零依赖）

| 脚本 | 作用 |
|---|---|
| `download-node.ts` | 按 manifest 下载官方 Node 二进制（SHA-256 校验） |
| `prepare-harness.ts` | `npm ci`（扁平布局）→ 物化（零符号链接）→ 收集全部依赖 LICENSE |
| `build-sidecar.ts` | `cargo build --release` 并暂存到 resources/runtime |
| `verify-runtime.ts` | 全链冒烟：boot → readiness → HTTP 200 → restart → shutdown → 无孤儿 |
| `verify-heartbeat.ts` | 挂死检测 e2e（hang 用例 + 健康负向用例） |
| `verify-preset.ts` | 驱动**真实上游 discoverPresets** 验证预设落盘可发现 |
| `load-soak.ts` | 负载浸泡（CPU 燃烧 + 探针延迟，生产默认心跳旋钮） |
| `updater-manifest.ts` | 发布后生成 `latest.json`（绝对资产 URL + minisign 签名） |
| `verify-bundle.ts` / `verify-signing.ts` | 安装包内容 / 签名状态断言（各有 `--self-test`） |

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

超时可用 `DSH_READY_TIMEOUT_MS`（默认 120s）/ `DSH_SHUTDOWN_GRACE_MS`（默认 10s）/
心跳旋钮 `DSH_HEARTBEAT_INTERVAL_MS`（0=禁用）/ `DSH_HEARTBEAT_FAIL_LIMIT`/
`DSH_HEARTBEAT_READ_TIMEOUT_MS` 调整。

### 进程树清理保证

| 场景 | 清理路径 |
|---|---|
| 正常退出 / 关闭应用 | Tauri `RunEvent::Exit` → shutdown → 优雅停止（unix: SIGTERM 进程组；win: CTRL_C）→ 超时后强杀 |
| 应用崩溃（无信号波及 sidecar） | sidecar stdin EOF 检测 → 强杀整棵树 → exit 0 |
| 整组信号（Ctrl+C / `timeout`） | sidecar 信号处理器 → 清理后 exit 0 |
| Windows 强杀兜底 | Job Object `KILL_ON_JOB_CLOSE`；优雅失败时 `TerminateJobObject` |

### 许可与第三方归属

- 本项目代码：MIT（见 `LICENSE`）。
- 安装包内置原版 DeepSeek Harness（MIT，随包附其 LICENSE/README）与 npm 依赖树；
  `prepare-harness` 收集每个依赖的 LICENSE 到包内 `runtime/harness/licenses/`。

## CI

- **test.yml**：三平台（ubuntu/windows/macos-14）完整 runtime 冒烟 + 心跳 +
  预设发现 e2e；Linux 质量门（单测/覆盖率/clippy/deny/vet/npm 审计）；
  Windows 宿主实跑 workspace 测试。
- **release.yml**：tag 触发 → 质量门 → **5 分钟负载 soak** → 打包
  （NSIS/DMG）→ 安装包内容/签名状态断言 → draft release + `latest.json`。
  `workflow_dispatch` 为只构建+验证的测试通道（不发布）。

## 版本升级流程

```
上游 @deepseek-ai/dsh rc.x → rc.y
  → 按 AGENTS.md「Harness 升级启动契约」复核 CLI 契约
  → 改 runtime/package.json + runtime-manifest.json
  → cd runtime && npm install（新安装脚本需先加 .npmrc 白名单）
  → CI 三平台冒烟全绿
  → tag v* 发版（新 Node/新 Harness 随 Desktop 一起更新）
```

## 当前状态与已知边界（v0.2.3）

- ✅ CI 全绿（质量门 + 三平台冒烟 + release 流水线含 soak 与 updater 产物）
- ✅ v0.2.3 draft 就绪（Windows updater + 预设导入导出 + 官网/插件市场入口）
- ✅ Windows 自动更新（minisign 公钥校验；macOS 更新器待签名+公证后启用）
- ⏳ 未接入：代码签名 / 公证（macOS 更新器依赖）、应用内插件安装 UI
  （当前走 CLI，见「插件生态」）
- 未签名构建：Windows SmartScreen、macOS Gatekeeper 需要用户手动放行
- Linux 仅为开发环境，不作为发行目标；node-pty 在 Linux dev 下无 prebuild
  （Web UI 启动不受影响；Windows/macOS 发行包自带对应 prebuild）
