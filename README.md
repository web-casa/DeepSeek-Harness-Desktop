# DeepSeek Harness Desktop

**社区桌面发行层**：把官方 DeepSeek Harness 打包成 Windows / macOS 原生应用。
不是 fork——真正的 Harness 完整保留（Node.js + node_modules + Cordis 插件），
桌面层只负责生命周期：Tauri 管窗口，Rust sidecar 管 Harness 进程。

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
crates/dsh-sidecar/        Rust 监督器（独立 crate，三平台可编译）
runtime/                   pin 版本 + pnpm lock + allowBuilds（node-pty/koffi 等）
scripts/                   下载 Node / 准备 runtime / 构建 sidecar / 端到端冒烟
src/                       bootstrap 前端（Svelte 5 + Vite）
src-tauri/                 Tauri 壳：状态机、ACL 化命令、capabilities、打包配置
.github/workflows/         test.yml（三平台冒烟）· release.yml（NSIS/DMG 打包）
```

## 开发

```bash
pnpm install                       # 前端 + tauri CLI
pnpm runtime:all                   # 下载 Node + 准备 Harness runtime + 构建 sidecar
pnpm runtime:verify                # 端到端冒烟（sidecar→node→dsh web→HTTP 200→无孤儿）
pnpm tauri dev                     # 桌面开发模式
pnpm icons                         # 从 icon-source.png 重新生成平台图标
```

> 本机若 `~/.cargo` 不可写，请设置 `CARGO_HOME=<repo>/.tmp/cargo-home` 再构建。

### 脚本说明（Node ≥ 24 原生跑 TS，零依赖）

| 脚本 | 作用 |
|---|---|
| `download-node.ts` | 按 manifest 版本下载官方 Node 二进制（平台/架构自动映射） |
| `prepare-harness.ts` | runtime/ 下 `pnpm install` 后复制 node_modules + 署名文件到 bundle resources；交叉校验 manifest 与安装版本 |
| `build-sidecar.ts` | `cargo build --release --target <host-triple>` 并暂存到 resources/runtime |
| `verify-runtime.ts` | 冒烟：boot → readiness → HTTP 200 → status → restart → HTTP 200 → shutdown → 孤儿进程检查 → sidecar 随父退出 |

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

## CI

- **test.yml**：ubuntu 上 sidecar 单测 + 三 target 交叉编译检查 + 前端构建；
  三平台（ubuntu/windows/macos-14）各跑一遍完整 runtime 冒烟。
- **release.yml**：tag 触发，在 windows-latest / macos-14 上先冒烟后打包
  （NSIS / DMG），产物上传为 GitHub Release draft。签名/公证/updater 的
  secrets 已在 workflow 中留位（P3 接入）。

## 版本升级流程

```
Dependabot/Renovate 提议 @deepseek-ai/dsh rc.x → rc.y
  → 改 runtime/package.json + runtime-manifest.json
  → pnpm install（锁文件更新）
  → CI 三平台冒烟全绿
  → tag v* 发版（新 Node/新 Harness 随 Desktop 一起更新）
```

## 当前状态与已知边界（P0）

- ✅ sidecar 三平台编译通过；Linux 本机端到端冒烟全绿
- ⏳ Windows/macOS 冒烟与打包由 CI 验证（本仓库尚未推送远端）
- ⏳ 未接入：代码签名 / 公证 / 自动更新 / 插件安装（bundled pnpm）/ 单实例锁
- 未签名构建：Windows SmartScreen、macOS Gatekeeper 需要用户手动放行
- Linux 仅为开发环境，不作为发行目标；node-pty 在 Linux dev 下无 prebuild
  （Web UI 启动不受影响；Windows/macOS 发行包自带对应 prebuild）
