# DSH Desktop

把官方 DeepSeek Harness 打包成 Windows / macOS / Linux 原生应用，让 Harness 及其**插件生态**开箱即用。
不是 fork：Harness 原样随包携带，桌面层只负责生命周期与安全边界。

| 仓库 | [web-casa/DeepSeek-Harness-Desktop](https://github.com/web-casa/DeepSeek-Harness-Desktop) |
| 下载 | [Releases](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases) |
| 官网 | [dsharness.app](https://dsharness.app) |
| 插件市场 | [cordis.run](https://cordis.run) |
| 文档 | [SECURITY](SECURITY.md) · [FORKING](FORKING.md) · [RELEASING](RELEASING.md) · [AGENTS](AGENTS.md) |
| 当前版本 | v0.2.15 · Windows x64/ARM64 EXE/MSI · macOS x64/arm64 DMG · Linux x64/arm64 AppImage/DEB/RPM/Flatpak · macOS 已签名/公证 · [English](README.en.md) |

> **macOS 用户**：v0.2.9 起，DMG 使用 Developer ID Application 签名并经
> Apple 公证、staple 与 Gatekeeper 复验。请只从本仓库 Releases 下载；若官方
> 安装包仍被 Gatekeeper 拦截，请保留诊断信息并通过仓库 issue 报告。

| 平台 | GitHub Release 安装包 |
|---|---|
| Windows | 原生 x64 与 ARM64：双语 NSIS `*-setup.exe`、WiX `.msi` |
| macOS | arm64 与 x64 `.dmg` |
| Linux | x64 与 arm64 `.AppImage`、`.deb`、`.rpm`、`.flatpak` |

每个公开安装包旁均有同名 `.sha256`。Microsoft Store 的 x64/arm64 MSIX
保持为独立、未签名的 Partner Center workflow artifact，不会混入公开 Release；
这类包由商店完成签名，不能作为普通侧载包使用。

WiX MSI 名称最后的 `_en-US` 或 `_zh-CN` **仅代表安装向导语言**，不是
Desktop 控制器的可用语言：两种包内的控制器都支持简体中文和 English。NSIS
则是一份同时含中英文资源的安装器，可按系统语言选择或让用户手动选择。发布
流水线会在各原生架构上验证 MSI 的实际 `ProductLanguage`，避免语言后缀沦为
仅文件名上的声明。

## 特性一览

| | |
|---|---|
| 🔌 **插件生态** | Cordis 插件体系随包携带；插件市场浏览/搜索/一键安装；预设安全导入导出；离线 `.tgz` 侧载 |
| 🔄 **自动更新**（Windows NSIS） | x64/ARM64 更新包各自由内嵌 minisign 公钥校验；MSI、Store、macOS 与 Linux 走其各自安全更新路径 |
| 💓 **挂死自愈** | 心跳检测 Harness「活着但无响应」并自动重启（退避+上限） |
| 🛡️ **安全边界** | Harness 窗口零 IPC 权限；桌面命令仅授权本地窗口；环境消毒 |
| 🔒 **隐私默认值** | 会话遥测默认关闭；子进程环境消毒（NODE_OPTIONS/loader 注入键等） |
| 🧰 **诊断与反馈** | 一键导出诊断 zip（尽力脱敏）、复制诊断、预填 issue 报告；用户显式开启后可为一次复现保留有界的本地 stderr/Desktop 错误证据 |
| 🪟 **桌面体验** | 单实例、窗口状态记忆、崩溃自动恢复、macOS 关闭=隐藏；托盘按 Harness 实时状态安全开放控制器/Harness、启动/重启与停止；控制器、托盘与 macOS 菜单可跟随系统语言或手选简体中文 / English，窗口标题同步但产品名保持 DSH Desktop |

## ✨ 插件生态

Harness 的技能、工具、模型路由、MCP、预设——**全部是 Cordis 插件**。

**发现**：设置页「资源」一键打开插件市场 [cordis.run](https://cordis.run)。

**安装**：设置页「插件（用户安装）」输入 npm 包名即可安装、卸载。安装复用官方 `dsh plugin --profile web add`；卸载先精确 pre-disable 目标，再用随包 pnpm 关闭 lifecycle script 后直接移除，避免上游全局 reconcile 意外启用另一个仍处于 pending 的市场插件。CLI 与 pnpm 均随包携带（**无需系统 pnpm**），带实时日志与取消（整棵 node → dsh/pnpm 进程树一并清理）。

从 cordis.run 插件详情页点「安装到桌面版」会用 `dsharness://plugin/install?v=1&name=<包名>&source=<插件页>` 唤起桌面版；桌面版在 Rust 侧严格校验协议后，仅将链接定位到对应市场 slug，并重新拉取详情、核对当前 `entryRevision` 与嵌套 source，再由用户确认安装（旧版本桌面版或不支持的市场页仍可复制包名粘贴安装）。命令行等价路径（高级用法）：

```bash
# macOS
DSH_HOME="<诊断页 dshHome>" node "/Applications/DSH Desktop.app/Contents/Resources/runtime/harness/node_modules/@deepseek-ai/dsh/lib/bin.js" plugin --profile web add <包名>
# Windows PowerShell
$env:DSH_HOME="<诊断页 dshHome>"; node "<安装目录>\runtime\harness\node_modules\@deepseek-ai\dsh\lib\bin.js" plugin --profile web add <包名>
```

插件装入 `<dshHome>/profiles/web/`（用户数据目录，不写安装目录），装完在应用里「重新启动 Harness」生效。

如果升级中断或手动删除过插件文件，控制器会只读检查「`package.json` 仍声明、
直接包入口却缺失」的 Web profile 漂移。它绝不自动改写用户配置；只有能够证明
为无自定义配置、且属于**未启用包**的简单 `cordis.patch.yml` 条目时，才会展示逐项
预览并要求再次确认后移除该条目。已启用 bundle、符号链接、复杂 YAML 与其他不确定
状态只报告，留给 Harness 的安全恢复流程或用户人工处理。

### 详细诊断（主动开启）

控制器默认只持久化有界的生命周期事实。遇到需要更完整错误线索的问题，可开启**详细诊断**、重启 Harness 后复现。开启期间，Desktop 只在本地记录尽力脱敏且有界的 Harness/插件 **stderr** 与 Desktop 自身错误；不会记录 Harness stdout、会话、提示词或工作区文件，也不会上传任何数据。只有你显式选择导出诊断 zip 时，这些记录才会进入压缩包。问题排查完成后请关闭该模式，并在不再需要导出包时使用「清除详细日志」；stderr 仍可能包含私密信息，分享前请逐项检查。

**预设**（`.dshpreset`）：把一组插件行打包成可分享的智能体配置。设置页提供安全导入/导出/删除——路径/符号链接/配额/密钥扫描 → 两阶段确认 → 原子安装到 `<dshHome>/.agent-presets/`，Harness 设置页立即可见。设置页还逐次复核预设根健康：损坏（`agent.cordis.yml` 缺失/不可读/为空，上游会拒绝挂载）、不安全（符号链接等上游跳过但占用 id 的条目）、缺元数据（`preset.yml`）即时可见并可删除。删除预设前请留意：桌面层直接移除目录，不清理 Harness 的默认预设设置——若删除的是当前默认预设，需在 Harness 设置页改选默认，否则下次会话可能无法启动。

> ⚠️ **信任模型**：插件与预设运行在 Harness 进程内，**与 Agent 同权限**。桌面校验拦截恶意构造的压缩包，拦不住内容有害的可信包——只安装可信来源。详见 [SECURITY.md](SECURITY.md)。

## 架构

```
Tauri 2 壳
 ├─ bootstrap 窗口：本地 Svelte UI · 有限桌面 IPC（ACL 授权）
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
pnpm test:scripts && pnpm test:frontend           # 脚本 + 前端安全逻辑单测（零依赖）
pnpm tauri dev                                    # 桌面开发模式
```

### cordis.run 本地 fixture（市场联调）

真 API 上线前，可用仓库内置 fixture 联调市场功能（数据契约与真实 API 一致：嵌套 `source`、`{zh,en}` 描述、cursor 分页字段 `page.cursor/hasMore/limit` + `count`、ETag/304 与 JSON 404 错误）：

```bash
node tools/cordis-fixture/fixture-server.mjs
# 输出端口后，在另一个终端设置后启动桌面调试构建：
CORDIS_RUN_API=http://127.0.0.1:<port>/api/v1 pnpm tauri dev
```

市场安装只接受可验证的 npm 嵌套 source；Desktop 会重新核对详情修订、
禁用构建脚本、校验 lockfile integrity，并将插件保持为“待激活”。安装不会自动
启用，需在插件列表中显式点击 **Activate**，随后重启 Harness。

### cordis.run 生产发布 smoke（只读）

每次 Cordis 后端部署或 Desktop 发版前，运行以下无 mutation 的验证：

```bash
pnpm verify:cordis-preset
pnpm verify:cordis-market
```

第二个命令固定请求 `platform=desktop`，要求直接 JSON、ETag/304 和 JSON 404。
在有已审核公开条目后，可额外验证它的嵌套 source / integrity / engine wire
shape（仍不安装）：

```bash
CORDIS_MARKET_PROBE_SLUG=<reviewed-public-slug> pnpm verify:cordis-market
```

Microsoft Store 构建要求插件同时通过生产 API 的实时安装门禁与
`src-tauri/store-curated-plugins.json` 本地审核快照；快照不是离线安装授权，
通过生产 probe 也不等于获得 Store allowlist 权限。

> `~/.cargo` 不可写时：`export CARGO_HOME=<repo>/.tmp/cargo-home`。

### 质量门

| 层 | 门禁 |
|---|---|
| 前端 | `tsc --noEmit` + `svelte-check` 0 error |
| Rust | `cargo nextest`（sidecar 与 Tauri，Windows 宿主亦实跑）· llvm-cov ≥50%/55% · `clippy -D warnings` |
| 供应链 | `cargo deny` · `cargo vet --locked`（70 全审 + 2 delta + 豁免基线）· `npm audit`/`pnpm audit` 阻断 high |
| 安全扫描 | CodeQL（rust/js-ts/actions）· Dependency Review（Graph 不可用时自动切换锁文件门禁） |
| 安装包 | 7 种公开格式、16 个公开安装包统一 `verify-bundle` + Windows/macOS `verify-signing`（fail-closed）+ 每包 SHA-256 |
| 发布 | 5 分钟负载 soak · updater 产物与 `latest.json` |

### 目录

```
crates/dsh-sidecar/   Rust 监督器          runtime/    pin + lock + 白名单
scripts/              构建/冒烟/验证脚本     src/       Svelte 前端
src-tauri/            Tauri 壳（状态机/ACL/预设边界）
deny.toml + supply-chain/   供应链策略与审计     .github/workflows/   CI
```

## 发布与版本

- CI：push/PR 跑质量门 + 三平台冒烟；打 `v*` tag 触发六个**原生**构建目标（Windows x64/ARM64、macOS x64/arm64、Linux x64/arm64）及 Store MSIX x64/arm64，再经完整资产清单门禁创建 draft release、生成并校验 `latest.json`，最后才自动公开 Release。每一 lane 都会同时核对 Node、Rust host triple 与目标架构；Windows on ARM 另加 x64 兼容性安装 smoke，不把它误称为原生构建。
- Microsoft Store：`v*` tag 同时构建 x64/arm64 MSIX（`build-msix` job，Store 模式关闭应用内更新并限制插件为 cordis.run 审核列表）。MSIX 产物作为 workflow artifact 下载后上传 Partner Center，不发布到 GitHub Release。
- Harness 升级走 [AGENTS.md](AGENTS.md)「启动契约」清单；发布流程见 [RELEASING.md](RELEASING.md)。
- 当前发布版本：v0.2.15；包含六个原生构建目标、Windows x64/ARM64 安装包与中英文 MSI、macOS Developer ID 签名/公证、Cordis v4 市场契约、诊断韧性与安全插件恢复。历史 v0.2.13 文件名中的 `_en-US` 只表示其安装向导为英文。
- 已知边界：Windows GitHub 安装包尚未配置 Authenticode，可能触发 SmartScreen；应用内更新只接受与当前 CPU 架构及 NSIS 安装方式完全匹配的 payload，MSI 不会自动切换到 NSIS；macOS 仍待“公证后 updater archive + 原生升级 smoke”闭环；Linux 包当前以 SHA-256 保护，尚无独立软件仓库签名。
- 许可：MIT；内置 Harness 及全部依赖的 LICENSE 随包附于 `runtime/harness/licenses/`。
