# Security Policy

DeepSeek Harness Desktop 是社区桌面打包层：Tauri 2 壳 + Rust `dsh-sidecar`
监督器 + 内置 Node 24，运行官方 `@deepseek-ai/dsh`（pin 版本，Web UI 原样
加载、不做任何修改）。本文记录威胁模型、默认安全姿态与报告渠道。

## 威胁模型（H1–H3 为何免疫）

社区同类项目（Electron 套壳）公开过的 RCE 链通常分三级——本项目逐级免疫：

- **H1 渲染进程逃逸 → 本机代码执行**：harness 窗口 capability 为**空集**
  （`capabilities/` 中无任何权限条目），webview 内无 `window.__TAURI__`、
  无 IPC 桥、无文件/命令触达面。
- **H2 导航逃逸 → 伪造 UI / 窃取会话**：webview 仅允许就绪时捕获的
  origin（`same_origin` 导航锁）；sidecar 的就绪解析器在**语法层**只接受
  `dsh web: http://127.0.0.1:<port>` 字面形态，被植入的远程 URL 无法通过。
- **H3 回环端口被本机其他进程抢占/冒充**：端口由 OS 随机分配（`--port 0`），
  窗口仅绑定 `127.0.0.1`；桌面层无 token，安全边界是「回环 + 随机端口 +
  渲染器零 Node」，与官方及主流同类实现一致。

## 默认安全姿态

- **会话遥测默认关闭**：对子进程注入 `DSH_TELEMETRY_DISABLED=1`（上游 dsh
  语义：任意非空值即禁用 session-telemetry）。如需开启，修改
  `src-tauri/src/harness/mod.rs` 的 `start_harness` env 并重新构建。
- **子进程环境消毒**：sidecar 在 spawn 前剥离继承环境中的 `NODE_OPTIONS`、
  `NODE_PATH`、`ELECTRON_RUN_AS_NODE` 与 `npm_config_*`（unix 走
  `env_clear` + 白名单快照；Windows 在 UTF-16 环境块层过滤，无编码往返）。
- **进程树保证**：unix 进程组 + sigaction；Windows Job Object
  （KILL_ON_JOB_CLOSE）+ 私有隐藏控制台。sidecar 消失（任何原因）即整树
  消失；存活性心跳在进程挂死（活着但无响应）时杀树并交由壳按退避上限重启。
- **DSH_HOME 0700** 且拒绝符号链接；安装包内 harness 子树零符号链接
  （构建期断言 + 安装包验证）。
- **供应链**：Node 下载 SHA-256 钉死（官方 SHASUMS256 核对）；npm 安装脚本
  白名单（strict-allow-scripts）；cargo-vet（社区审计集 + 本仓库审计）与
  cargo-deny 为发布闸门。
- **CSP 与桌面 IPC**：`withGlobalTauri: false`；7 个桌面命令经 AppManifest
  ACL 仅授权 bootstrap 窗口。

## 已知边界（请如实预期）

- **未签名分发**：当前发布未做代码签名/公证。Windows SmartScreen 与 macOS
  Gatekeeper 会提示并需要用户手动放行；macOS 首次启动可能需
  `xattr -cr` 或系统设置放行（见 README）。签名/公证接入后，
  `verify-signing.ts` 会在 CI 强制验签（fail-closed）。
- **存活心跳的语义边界**：默认连续 4 次探针无响应（约 40 秒）判定挂死并
  自动重启；极端长同步任务阻塞事件循环可能触发。旋钮：
  `DSH_HEARTBEAT_INTERVAL_MS`（0=禁用）/`DSH_HEARTBEAT_FAIL_LIMIT`/
  `DSH_HEARTBEAT_READ_TIMEOUT_MS`。
- **无自动更新**：暂未接入更新器；未来接入时更新源与产物校验必须经 SHA-256
  + 签名验证，且社区重打包版本不得指向任何官方渠道（见 FORKING.md）。

## 报告漏洞

- 首选：GitHub Security Advisory（本仓库 Security 标签页 → Report a
  vulnerability），私密披露。
- 或邮件至仓库维护者。请附复现步骤、平台/版本与影响评估；我们会在
  90 天内响应并发布修复版本与公告。

## 支持版本

仅支持最新发布版（当前 v0.2.0）。旧版本不提供安全修复；发现漏洞请先
升级到最新版再验证是否仍可复现。
