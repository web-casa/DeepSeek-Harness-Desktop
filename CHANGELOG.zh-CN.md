# 更新日志

本文记录 DSH Desktop 面向用户的变更。每个版本的不可变安装包、校验和和完整
技术记录，请查看对应的 GitHub Release。

English edition: [CHANGELOG.md](CHANGELOG.md).

## [0.2.18] — 2026-08-27

### AppImage 桌面运行时兼容性

- 不再把构建主机的 Wayland、GLib、GIO 和 nghttp2 ABI 库打入 AppImage，改用
  目标 Linux 系统提供的兼容桌面库。
- 打包内嵌 WebView 所需的 GStreamer 插件。
- 保留用户显式选择的 GTK 后端，同时继续以 X11 作为兼容性更广的默认回退。
- 打包官方、未修改的 DeepSeek Harness `0.1.1-rc.2`、Node.js `24.19.0` 和
  `dsh-sidecar` `0.2.7`。

**发布：** [v0.2.18](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/tag/v0.2.18)

## [0.2.17] — 2026-08-24

### 恢复严格 Snap 的桌面门户访问

- 恢复严格受限 Snap 中内置 Harness 所需的桌面门户访问，使其无需削弱沙箱即可
  与桌面会话协作。
- 修复控制器的原生下拉选项对比度，包括语言选择项。
- 打包官方、未修改的 DeepSeek Harness `0.1.1-rc.2`、Node.js `24.19.0` 和
  `dsh-sidecar` `0.2.6`。

**发布：** [v0.2.17](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/tag/v0.2.17)

## [0.2.16] — 2026-08-24

### 严格 Snap Store 发行支持

- 引入严格 Snap 包定义及其经审查的桌面接口策略。
- 为 Snap 发布链路新增包、runner 和产物验证。
- 补充 Windows 运行时前置条件说明，并更新产品截图。

**发布：** [v0.2.16](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/tag/v0.2.16)

## [0.2.15] — 2026-08-23

### 公开校验和审计

- 当公开 GitHub 产物名称或其返回的 SHA-256 摘要与已审查产物不一致时，发布
  流程将失败关闭。
- 防止校验和侧车文件以无法校验已下载产物的名称发布。

> v0.2.15 已取代 v0.2.14 的下载推荐；校验文件时请使用本版本或更新版本。

**发布：** [v0.2.15](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/tag/v0.2.15)

## [0.2.14] — 2026-08-23

### 原生多架构发布线

- 新增 Windows x64/arm64、macOS x64/arm64 和 Linux x64/arm64 原生发布目标，
  并校验内置 Node.js 与 `node-pty` 的架构。
- 修复 Windows 安装器运行时和 deep-link 冒烟覆盖。
- 将内置官方 Harness 升级到 `0.1.1-rc.2`，并加强安装仲裁和 CodeQL 覆盖。

> 安装包字节和摘要有效，但 GitHub 规范化文件名使 v0.2.14 校验和侧车文件难以
> 直接使用；请改用 v0.2.15 或更新版本。

**发布：** [v0.2.14](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/tag/v0.2.14)

## [0.2.13] — 2026-08-22

### 控制器本地化与更安全的托盘快捷项

- 新增简体中文和英文词典、系统语言检测、手动选择与语言偏好持久化。
- 让控制器标题、托盘和 macOS 原生菜单与所选语言同步。
- 新增受状态门禁保护的“打开控制器或 Harness、启动或重启、停止、退出”快捷项。
- 更新已审计的 `pnpm`、`getrandom`、`zip` 和 `reqwest` 依赖，同时保留 Rust
  `ring` TLS 提供者。

**发布：** [v0.2.13](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/tag/v0.2.13)

## [0.2.12] — 2026-08-22

### 更安全的插件生命周期恢复

- 修复 Windows PATH 处理问题：内置 `pnpm` shim 前置后，仍会保留每一个路径段。
- 加强插件安装、激活、卸载、进程清理和恢复行为。
- 规范化 Node 的 verbatim 启动路径，使内置运行时能从 Windows 安装路径可靠启动。

**发布：** [v0.2.12](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/tag/v0.2.12)
