# Microsoft Store Submission — DSH Desktop (Community)

This file is the Partner Center submission runbook. Package identity values are
committed in `src-tauri/gen/windows/`; do not edit them in Partner Center.

## Product identity

| Field | Value |
|---|---|
| Product type | MSIX or PWA App |
| Product name | `DSH Desktop (Community)` |
| Package Identity Name | `53660AlanM.DSHDesktopCommunity` |
| Package Identity Publisher | `CN=84AC3716-04E0-4D67-8951-0D3E51674CA0` |
| PublisherDisplayName | `AlanM.` |
| Package Family Name | `53660AlanM.DSHDesktopCommunity_909n0052ampem` |
| Store ID | `9NPC8QH171WF` |

## Listing copy

### English

**Name:** DSH Desktop (Community)

**Short description:**
Run DeepSeek Harness as a supervised native Windows app. Bundled Node.js, pinned
Harness runtime, local web UI and the Cordis plugin ecosystem — no terminal or
Node.js setup.

**Description:**
DSH Desktop is a community desktop packaging of the official DeepSeek Harness.
A Tauri 2 shell and a Rust sidecar start the unmodified Harness web UI on a
random loopback port with a bundled Node.js runtime.

Features:

- One-click native app: no terminal, no system Node.js installation.
- Supervised lifecycle: crash detection, bounded restarts, heartbeat recovery.
- Privacy defaults: session telemetry disabled, sanitized child environment.
- Zero desktop IPC in the Harness window.
- Cordis plugin ecosystem: install reviewed plugins from cordis.run.
- Presets, diagnostics export and issue reporting.

The Microsoft Store build is updated through the Microsoft Store. Plugins
installed in the Store build are restricted to the reviewed cordis.run list.

**Category:** Developer Tools > Development Tools / Productivity

**Search terms:** deepseek harness, dsh, cordis, ai agent, desktop harness

**Support contact:** https://dsharness.app/support/
**Website:** https://dsharness.app/windows/
**Privacy policy:** https://dsharness.app/privacy/

### 简体中文

**名称：** DSH Desktop (Community)

**简短说明：**
以受监督的原生 Windows 应用运行 DeepSeek Harness。内置 Node.js、固定版本
Harness 运行时、本地 Web UI 与 Cordis 插件生态——无需终端或 Node.js。

**说明：**
DSH Desktop 是官方 DeepSeek Harness 的社区桌面打包版。Tauri 2 壳与 Rust
sidecar 在随机回环端口上启动未修改的 Harness Web UI，并内置 Node.js 运行时。

功能：

- 一键原生应用：无需终端，无需系统 Node.js。
- 受监督生命周期：崩溃检测、有上限自动重启、心跳恢复。
- 隐私默认值：会话遥测默认关闭，子进程环境消毒。
- Harness 窗口零桌面 IPC。
- Cordis 插件生态：从 cordis.run 安装已审核插件。
- 预设、诊断导出与问题报告。

Microsoft Store 版通过 Microsoft Store 更新；插件安装仅允许 cordis.run
已审核列表。

**类别：** 开发人员工具 / 生产力
**搜索词：** deepseek harness, dsh, cordis, ai agent, desktop harness

## Product declarations

- [x] This product incorporates generative AI features
- [ ] Non-Microsoft drivers or NT services

## Notes for certification

- The app starts the bundled Node.js runtime and the official Harness web UI
  automatically; no login is required to reach the settings/status UI.
- To test model features, configure an API endpoint in the Harness settings.
  If a test endpoint is unavailable, certification can verify launch, local
  server readiness, plugin listing, diagnostics export and clean uninstall.
- `dsharness://plugin/install` is registered by the MSIX manifest. Opening a
  valid link shows a confirmation dialog and never installs silently.
- No drivers or NT services are installed.

## Package upload

1. Verify the public Store URLs return HTML successfully:
   - `https://dsharness.app/windows/`
   - `https://dsharness.app/privacy/`
   - `https://dsharness.app/support/`
2. Run the `Release` workflow (`workflow_dispatch` or `v*` tag).
3. Download artifacts:
   - `dsh-desktop-store-msix-x64`
   - `dsh-desktop-store-msix-arm64`
4. Extract and upload the two `.msix` packages on the Packages page.
5. Do **not** upload the MSIX files to GitHub Releases; they are Store-only.

## Post-publish updates

- Bump the desktop version in the normal version files and tag a new release.
- Upload the new `.msix` packages to a new Partner Center submission.
- Do not enable the in-app updater in Store builds.
