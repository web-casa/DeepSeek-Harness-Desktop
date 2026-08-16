# DeepSeek Harness Desktop

把官方 DeepSeek Harness 打包成 Windows / macOS 原生应用，让 Harness 及其**插件生态**开箱即用。
不是 fork：Harness 原样随包携带，桌面层只负责生命周期与安全边界。

| 仓库 | [web-casa/DeepSeek-Harness-Desktop](https://github.com/web-casa/DeepSeek-Harness-Desktop) |
| 下载 | [Releases](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases) |
| 官网 | [dsharness.app](https://dsharness.app) |
| 插件市场 | [cordis.run](https://cordis.run) |
| 文档 | [SECURITY](SECURITY.md) · [FORKING](FORKING.md) · [RELEASING](RELEASING.md) · [AGENTS](AGENTS.md) |
| 版本 | v0.2.6 · Windows x64 NSIS / macOS arm64 DMG · 未签名预览版 · [English](README.en.md) |

> ⚠️ **macOS 用户**：Apple 开发者证书仍在申请中，应用尚未签名。首次打开若被
> Gatekeeper 拦截，请执行：
> ```bash
> xattr -dr com.apple.quarantine "/Applications/DeepSeek Harness Desktop.app"
> ```
> （如提示权限不足，在命令前加 `sudo`。）预计下一版本完成签名+公证后不再需要此步骤。

## 特性一览

| | |
|---|---|
| 🔌 **插件生态** | Cordis 插件体系随包携带；设置页一键安装/卸载插件；预设安全导入导出 |
| 🔄 **自动更新**（Windows） | 更新包由内嵌 minisign 公钥校验；macOS 待签名+公证后启用 |
| 💓 **挂死自愈** | 心跳检测 Harness「活着但无响应」并自动重启（退避+上限） |
| 🛡️ **安全边界** | Harness 窗口零 IPC 权限；桌面命令仅授权本地窗口；环境消毒 |
| 🔒 **隐私默认值** | 会话遥测默认关闭；子进程环境消毒（NODE_OPTIONS/loader 注入键等） |
| 🧰 **诊断与反馈** | 一键导出诊断 zip（尽力脱敏）、复制诊断、预填 issue 报告 |
| 🪟 **桌面体验** | 单实例、窗口状态记忆、崩溃自动恢复、macOS 关闭=隐藏 |

## ✨ 插件生态

Harness 的技能、工具、模型路由、MCP、预设——**全部是 Cordis 插件**。

**发现**：设置页「资源」一键打开插件市场 [cordis.run](https://cordis.run)。

**安装**：设置页「插件（用户安装）」输入 npm 包名即装即卸——桌面层调用官方 `dsh plugin --profile web add/remove`，pinned CLI 与 pnpm 均随包携带（**无需系统 pnpm**），带实时安装日志与取消（整棵 node → dsh → pnpm 进程树一并清理）。

从 cordis.run 插件详情页点「安装到桌面版」会用 `dsharness://plugin/install?v=1&name=<包名>&source=<插件页>` 唤起桌面版；桌面版在 Rust 侧严格校验协议后弹出确认框，用户确认后才开始安装（旧版本桌面版或不支持的市场页仍可复制包名粘贴安装）。命令行等价路径（高级用法）：

```bash
# macOS
DSH_HOME="<诊断页 dshHome>" node "/Applications/DeepSeek Harness Desktop.app/Contents/Resources/runtime/harness/node_modules/@deepseek-ai/dsh/lib/bin.js" plugin --profile web add <包名>
# Windows PowerShell
$env:DSH_HOME="<诊断页 dshHome>"; node "<安装目录>\runtime\harness\node_modules\@deepseek-ai\dsh\lib\bin.js" plugin --profile web add <包名>
```

插件装入 `<dshHome>/profiles/web/`（用户数据目录，不写安装目录），装完在应用里「重新启动 Harness」生效。

**预设**（`.dshpreset`）：把一组插件行打包成可分享的智能体配置。设置页提供安全导入/导出/删除——路径/符号链接/配额/密钥扫描 → 两阶段确认 → 原子安装到 `<dshHome>/.agent-presets/`，Harness 设置页立即可见。设置页还逐次复核预设根健康：损坏（`agent.cordis.yml` 缺失/不可读/为空，上游会拒绝挂载）、不安全（符号链接等上游跳过但占用 id 的条目）、缺元数据（`preset.yml`）即时可见并可删除。删除预设前请留意：桌面层直接移除目录，不清理 Harness 的默认预设设置——若删除的是当前默认预设，需在 Harness 设置页改选默认，否则下次会话可能无法启动。

> ⚠️ **信任模型**：插件与预设运行在 Harness 进程内，**与 Agent 同权限**。桌面校验拦截恶意构造的压缩包，拦不住内容有害的可信包——只安装可信来源。详见 [SECURITY.md](SECURITY.md)。

## 架构

```
Tauri 2 壳
 ├─ bootstrap 窗口：本地 Svelte UI · 有限桌面 IPC（22 命令，ACL 授权）
 └─ harness 窗口：http://127.0.0.1:<随机端口> · 原版 Harness Web UI · 零 IPC
        │ NDJSON (stdin/stdout)
dsh-sidecar（Rust，无重依赖）
 ├─ 启动/停止/重启 · readiness · 挂死心跳 · 树清理
 └─ Unix process group / Windows Job Object
        │
内置 Node 24 + @deepseek-ai/dsh（pin 版本）
 └─ node lib/bin.js web --host 127.0.0.1 --port 0 · DSH_HOME 与 CLI 隔离
```

## 核心原则

1. **不 fork**：只 pin `@deepseek-ai/dsh`，上游发版 = 改版本号 + CI 冒烟。
2. **不改 Web UI**：直接加载原版 UI，`--port 0` + readiness 行即握手协议。
3. **安全边界**：harness 窗口 capability 为空集；桌面命令经 app ACL 仅授 bootstrap。
4. **版本单一事实源**：`runtime/runtime-manifest.json`。

## 开发

```bash
pnpm install && pnpm check && pnpm check:scripts   # 依赖 + 类型检查
pnpm runtime:all && pnpm runtime:verify           # 准备运行时 + 端到端冒烟
pnpm test:scripts                                 # node:test 单测（零依赖）
pnpm tauri dev                                    # 桌面开发模式
```

> `~/.cargo` 不可写时：`export CARGO_HOME=<repo>/.tmp/cargo-home`。

### 质量门

| 层 | 门禁 |
|---|---|
| 前端 | `tsc --noEmit` + `svelte-check` 0 error |
| Rust | `cargo nextest`（sidecar 35 + Tauri 35，Windows 宿主亦实跑）· llvm-cov ≥50%/25% · `clippy -D warnings` |
| 供应链 | `cargo deny` · `cargo vet --locked`（70 全审 + 2 delta + 豁免基线）· `npm audit`/`pnpm audit` 阻断 high |
| 安全扫描 | CodeQL（rust/js-ts/actions）· Dependency Review |
| 安装包 | `verify-bundle` + `verify-signing`（fail-closed）+ SHA-256 |
| 发布 | 5 分钟负载 soak · updater 产物与 `latest.json` |

### 目录

```
crates/dsh-sidecar/   Rust 监督器          runtime/    pin + lock + 白名单
scripts/              构建/冒烟/验证脚本     src/       Svelte 前端
src-tauri/            Tauri 壳（状态机/ACL/预设边界）
deny.toml + supply-chain/   供应链策略与审计     .github/workflows/   CI
```

## 发布与版本

- CI：push/PR 跑质量门 + 三平台冒烟；打 `v*` tag 触发完整发布（质量门 → soak → 打包 → 内容/签名断言 → draft release + `latest.json`）。
- Harness 升级走 [AGENTS.md](AGENTS.md)「启动契约」清单；发布流程见 [RELEASING.md](RELEASING.md)。
- 当前状态：v0.2.6 开发中（新增 cordis.run deep-link 一键安装确认；v0.2.4 为当前发布，v0.2.3 draft 作废不发布）。
- 已知边界：未签名（SmartScreen/Gatekeeper 手动放行）；macOS 更新器待签名；Linux 仅开发用。
- 许可：MIT；内置 Harness 及全部依赖的 LICENSE 随包附于 `runtime/harness/licenses/`。
