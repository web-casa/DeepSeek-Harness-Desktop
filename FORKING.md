# FORKING.md — 重分发边界

本项目（MIT）是官方 DeepSeek Harness（`@deepseek-ai/dsh`）的社区桌面打包层。
你可以 fork、改名、改代码、重新分发，但请遵守以下边界——它们保护**你**的
用户，也保护上游与本项目的声誉。

## 必须做

1. **不冒充官方/本仓库**：明确声明「非 DeepSeek 官方产品，也非
   deepseek-harness-desktop 官方构建」。不要使用官方/本仓库的图标、名称
   与更新渠道。
2. **改身份三件套**：`appId`（tauri.conf.json → `identifier`）、产品名
   （`productName`）、用户数据目录/协议名——否则你的构建会与官方构建
   共享配置目录并互相覆盖。
3. **更新器不得指向本仓库的发布**：若接入自动更新，更新源必须是你自己的
   发布渠道，且产物校验（SHA-256 + 签名）不得弱于本仓库（见
   `scripts/verify-signing.ts` 的 fail-closed 原则）。
4. **保留第三方归属**：安装包必须携带 `licenses/` 下的第三方许可证
   （本仓库构建期断言 scoped 包许可证存在）。不得在打包时删除
   `LICENSE*` 文件（上游 MIT 要求保留版权与许可文本）。
5. **保持供应链闸门**：Node 下载 SHA-256、npm 安装脚本白名单、cargo
   vet/deny——至少维持同等强度后再发版。

## 建议做

6. **默认关闭遥测**：保持 `DSH_TELEMETRY_DISABLED=1`（隐私默认值）；若
   你移除它，请在 README 显式告知用户。
7. **未签名发布须如实披露**：Windows SmartScreen / macOS Gatekeeper 的
   放行方式要写进你的 README（本仓库 SECURITY.md 有现成措辞）。
8. **安全文档同步**：如果你改动安全边界（capability、导航锁、环境清洗、
   心跳阈值），同步更新你 fork 的 SECURITY.md——不要沿用本仓库已不适用
   的威胁模型声明。

## 合法且鼓励的用法

- 学习 Tauri 2 + sidecar 监督 + 捆绑 Node 的打包模式；
- 适配其他 Harness 版本/平台（对照 AGENTS.md「Harness 升级启动契约」清单）；
- 把你的改进以 PR 形式回馈本仓库。
