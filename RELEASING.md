# RELEASING.md — 发布手册

发布流程全自动：在 `main` 上打 `v*` tag 即触发完整流水线（质量门 → 六个原生目标
构建 → 内容/签名验证 → draft release → `latest.json` 验证 → Publish）。草稿在
全部公开资产与更新清单通过精确校验后才会自动公开；中途失败会保留为未公开草稿。
人工职责是版本对齐、打 tag，并在发布后抽查公开资产。

## 1. 版本对齐（发布前本地完成）

版本单一事实源见 AGENTS.md「版本单一事实源」表。桌面与 sidecar 版本必须
同步 bump，harness pin 若同时升级必须走「Harness 升级启动契约」清单。
改动后本地跑：

```bash
pnpm release:preflight     # 版本对齐 + lock + checksum 表 + npm 边界 + tag 绑定演练
pnpm check && pnpm check:scripts
CORDIS_PRESET_SLUG=code pnpm verify:cordis-preset
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace  # 或按 crate 分别跑
cargo vet --locked && cargo deny check
node scripts/verify-runtime.ts && node scripts/verify-heartbeat.ts
node scripts/verify-bundle.ts --self-test && node scripts/verify-signing.ts --self-test
```

## 2. 打 tag（只在 main 上）

```bash
git checkout main && git pull
git tag -a v0.2.1 -m "DSH Desktop 0.2.1"
git push origin v0.2.1
```

**语义**：tag 必须打在 `main` 的提交上（合入之后）。在 feature 分支上打 tag
再 squash-merge、或 main 被 force-push 重写，都会被流水线的 tag-ancestry
闸门拒绝（前置 `tag-gate` job，秒级失败，不浪费构建）。

## 3. 流水线做什么（无需人工）

1. `tag-gate`：tag 提交必须是 `origin/main` 祖先。
2. `quality`（reusable）：fmt / clippy（含 Windows 宿主）/ nextest /
   llvm-cov / deny / vet / 脚本类型与 self-test。
3. `build` ×6（Windows x64/ARM64 各一份双语 NSIS EXE + 英文/简体中文
   WiX MSI、macOS x64/arm64 DMG、Linux x64/arm64
   AppImage+DEB+RPM+Flatpak）：下载 Node（SHA-256）
   → prepare-harness（441+ 许可证 + 零链接断言）→ 构建 sidecar → 冒烟 →
   `tauri build`（macOS 只产生已签名 `.app`，DMG 由无 Finder 自动化的
   有界 `hdiutil -srcfolder` 路径生成；仅对已知的 DiskImages 瞬态故障最多
   重试 3 次，确定性错误立即失败；Flatpak 从同一 DEB 导入）→
   **verify-bundle**（包元数据、
   二进制架构、必需文件、scoped 许可证
   绊线、零符号链接、执行位、无 quarantine）→ **verify-signing**（见下）→
   SHA-256 checksum → 上传制品。每个构建起始处都将 `process.arch`、`rustc -vV`
   host triple 与该 matrix 行交叉核对；不允许交叉编译或通过模拟器伪装原生构建。
   Windows on ARM 另运行 x64 安装兼容性 smoke，但明确不属于原生构建证据。
4. 签名 macOS 构建把“签名/构建”和“公证等待”分离：`build` 只提交一次，
   将 Submission ID、DMG SHA-256 与未 staple 的 DMG 保存为私有 handoff artifact；
   `notarize-macos` ×2 只查询该 ID（每次最多 20 分钟），Accepted 后 staple、
   Gatekeeper/内容/签名复验并上传最终公开 artifact。Apple 长时间 In Progress
   不会重新上传，也不会让构建 runner 无限等待。
5. `build-msix` ×2：原生 Windows x64/arm64 Store 包、静态内容检查与 SHA-256；
   只保留为 Partner Center workflow artifact。
6. `release`：tag 绑定 preflight（`--expect-tag`）→ 只下载
   `deepseek-harness-desktop-*` 公开制品 → 校验 16 个安装包、16 个 checksum、
   2 个 Windows NSIS updater signature 且没有 MSIX/未知文件 → 创建 **draft** GitHub
   Release → 上传并校验 `latest.json` → 自动 Publish。若最后两步失败，草稿保持
   不公开，绝不会让客户端读取半成品更新清单。

发布矩阵只使用 GitHub 标准 hosted runner：`windows-latest` / `windows-11-arm`、
`macos-15-intel` / `macos-15`、`ubuntu-22.04` / `ubuntu-22.04-arm`。公开仓库的
这些 runner 不计入 Actions 分钟费用；流水线在仓库变为 private 时会在 matrix
之前 fail-closed，要求维护者先重新评审计费策略。所有临时 bundle artifact 采用
7 天保留，MSIX 与公证 handoff 采用 14 天保留，防止存储无限累积。

### 3a. cordis.run preset 直返 ZIP 契约

`preset-download-contract` 在每次 tag 发布和手动演练时，对
`https://cordis.run/api/presets/<slug>/download` 发起不跟随重定向的请求。
它只接受 **HTTP 200**、无 `Location`、且 `Content-Type` 为
`application/zip`（允许 MIME 参数）；3xx/CDN 跳转、HTML 错页或其他内容类型
都会失败。该 job 是 tag 发布门禁，但刻意不作为原生构建的依赖：后端回归时仍可
用 `workflow_dispatch` 留存 Windows/MSIX 的构建和安装验证证据。

探针不写入或安装任何归档；真正的 Desktop 下载仍需经过 Rust 侧的无重定向、大小、
归档检查与用户确认。默认公开样本是 `code`；在手动演练中可通过
`preset_slug` 输入（或本地 `CORDIS_PRESET_SLUG`）选择另一个合法 slug。

### 3b. Microsoft Store MSIX 构建

`build-msix` job 与六目标 native build 并行，仅发布/演练时运行：

- `STORE_BUILD=1`：Store 版关闭应用内更新，插件安装只允许
  `src-tauri/store-curated-plugins.json` 中的 cordis.run 审核列表。
- x64（`windows-latest`）与 arm64（`windows-11-arm`）分别在原生宿主上下载
  对应 Node、prepare-harness、构建 sidecar，再运行
  `pnpm tauri:windows:build`；`scripts/verify-msix.ts` 校验包身份、协议与
  运行时内容。
- 产物按架构上传为 workflow artifact `dsh-desktop-store-msix-x64` /
  `dsh-desktop-store-msix-arm64`（各带 `.sha256`），**不发布到 GitHub
  Release**；维护者下载后在 Partner Center 上传 `.msix` 包提交 Store。
- Store 产品身份固定在
  `src-tauri/gen/windows/AppxManifest.xml.template` 与 `bundle.config.json`；
  如 Partner Center 重建产品，必须同步这两处。

### 3c. npm 发布所有权边界

根 `package.json` 必须保持 `private: true`，它是 Desktop 构建工作区，不是 npm
发布通道；`release:preflight` 会拒绝移除此保护。已确认的组织决策是：若将来确有
独立的公开 npm 制品，所有权必须归 `web-casa`，并使用一个经单独评审的
`@web-casa/<package>` 名称。

首次发布前，负责人必须完成并记录以下事项：确认准确的 scoped 名称与 npm 组织
成员关系；为发布者启用 npm 2FA；将 GitHub 仓库配置为 npm Trusted Publishing
（OIDC，优先）或使用仅限该包、短期且最小权限的 granular token；以实际发布身份
复核 `npm owner ls`/包访问权限。Trusted Publishing 的专用发布 workflow 必须在
GitHub-hosted runner 上运行、申请 `id-token: write`，且其仓库和 workflow 文件名
与 npm 设置精确一致。不得假定当前未 scoped 的根包名可用或可转移，更不得把本
Desktop 工作区直接改为公开发布。

## 4. 签名状态（verify-signing 语义）

- 未配置 `APPLE_CERTIFICATE` / `WINDOWS_CERTIFICATE` secrets → Windows/macOS 构建为
  **有意未签名**，流水线断言「确实未签名/未公证」（warn 级，防状态漂移），
  照常发布；README/SECURITY 已如实披露放行方式。
- Windows Authenticode 需同时配置 Base64 PFX `WINDOWS_CERTIFICATE`、
  `WINDOWS_CERTIFICATE_PASSWORD` 与证书机构提供的 `WINDOWS_TIMESTAMP_URL`；缺少
  后两项会在构建前失败。macOS 始终需要 `APPLE_CERTIFICATE`、
  `APPLE_CERTIFICATE_PASSWORD`、`APPLE_TEAM_ID`；公证认证优先使用 App Store
  Connect API Key（`ASC_KEY_ID`、`ASC_ISSUER_ID`、`ASC_PRIVATE_KEY_B64`），缺少时
  兼容现有 `APPLE_ID` + app-specific `APPLE_PASSWORD`。任一凭据组部分配置都会
  fail-closed。ASC CLI 固定为 4.6.0 的 macOS x64/arm64 release asset，并在执行前
  通过仓库内 SHA-256 白名单验证，不运行远程安装脚本。
- 配置了证书 secrets → **强制验签**（macOS: codesign --verify --deep --strict +
  spctl --assess + stapler validate；Windows: Authenticode == Valid），
  任一失败即阻断发布（fail-closed，防假签名）。Linux 安装包当前没有独立
  软件仓库签名，统一由发布资产的 SHA-256 sidecar 与完整清单门禁保护。

## 5. 发布后抽查

1. 打开已发布的 release `vX.Y.Z`：核对 16 个公开安装包
   （双架构双语 NSIS EXE、双架构英文/简体中文 WiX MSI、双架构 DMG、双架构
   AppImage/DEB/RPM/Flatpak）、各自 `.sha256`、2 个 Windows NSIS updater
   signature（x64 与 ARM64，各一）与体积量级；确认没有 `.msix`。MSI 文件最后的 `_en-US` / `_zh-CN`
   只表示安装向导语言，不是控制器语言限制；NSIS 是一个内含中英文资源的安装器。
   macOS 两个 job 会在本地强制检查各自 `.app.tar.gz.sig` 已生成，但在 updater
   尚未启用期间不上传这些同名、无对应公开更新包的 build-only tripwire。
2. 抽查 checksum：`shasum -a 256 <下载文件>` 对照 `.sha256` 内容。
3. 手动下载安装验证（未签名构建：确认 SmartScreen/Gatekeeper 放行路径可走通）。
4. 点 **Publish release**。
5. 记录下载基线：`node scripts/release-stats.ts`（公开下载计数是本项目
   唯一的“遥测”，维护者本地 gh 凭据即可）。
6. 更新 README「当前状态」行。

## 6. 回滚/重发

- 草稿/清单阶段发现问题：草稿不会自动公开；修复后重新运行失败 job。若需更换
  提交，则删除 draft 和 tag 后重新打 tag（tag 重新指向新提交）。
- 已发布：永不覆盖资产；bump patch 版本重新发布，旧版标注。
- workflow_dispatch（不发布）可用于在**不打 tag** 的情况下全流程演练构建
  与验证；手动演练必须在受保护的 `main` 上启动，非 main workflow ref 不会进入
  签名/构建图。Release 的全部源码 checkout 都固定为 `main`，且不接受
  tag/branch 输入。`native_target`
  默认为 `all`；选 `macos-x64` 或 `macos-arm64` 时只跑对应的构建、
  Developer ID 签名、Submission-ID 公证与产物复验，并跳过 MSIX、
  Windows installer smoke 和 5 分钟 soak。此通道用于发布前低成本验证
  macOS 打包/公证修复，不会创建 Release。

### 6a. Apple 公证超时续跑

`notarize-macos` 若在 20 分钟内仍得到 `In Progress`，会以失败结束并在日志中
打印原 Submission ID；Apple 后台任务不会被取消。此时只能在该 workflow run
选择 **Re-run failed jobs**。成功的 `build macos-*` job 与其 handoff artifact
会被复用，等待脚本重新校验 DMG SHA-256 后继续查询原 ID。

不要选择 **Re-run all jobs**，也不要重新触发一条相同 ref 的 Release 演练；这两种
操作会重新执行提交 job，制造重复 submission。`Invalid` / `Rejected` 会生成
`dsh-macos-notarization-diagnostics-*` artifact，先查看 developer log 再修复签名
或 bundle，禁止把终态失败当作可重试网络故障。

## 7. 更新器（updater）运行语义

- 更新包真实性由内嵌 minisign 公钥校验（`tauri.conf.json` 的
  `plugins.updater.pubkey`），与代码签名无关；私钥存于
  `TAURI_SIGNING_PRIVATE_KEY` secret，**丢失即全部更新失效**——轮换需
  重新生成密钥对、更新 pubkey、发布一次强制全量安装的新版本。
- `latest.json` 由 publish job 的 `scripts/updater-manifest.ts` 在草稿 release
  创建后自动生成并上传（绝对资产 URL + .sig 内容），fail-closed：缺配对即失败；
  随后 workflow 才会把草稿公开。因此 `/releases/latest/download/latest.json`
  不会指向尚未通过资产校验的 release。
  Windows 条目固定为 `windows-x86_64-nsis` 和
  `windows-aarch64-nsis`，没有通用 `windows-*` fallback；这保证 MSI
  安装不会静默切换到 NSIS 更新器。应用内更新接管前会先停止 Harness、清理
  插件子进程树并写入生命周期收尾证据，因为 Tauri 的 Windows 更新安装器会
  直接退出进程而不经过普通 `RunEvent::Exit`。
- **macOS updater 未启用**。DMG 已可完成 Developer ID 签名与公证，但启用
  前还必须产出并验证“公证并 staple 后”的 `.app.tar.gz` updater archive、
  上传两种原生架构的签名资产、发布 `darwin-*-app` 精确 manifest 项，并在
  原生 x64/ARM64 机器上完成从已安装应用升级的 smoke；不得把 DMG 的公证
  成功误当作 updater 已可安全启用。
- **Linux updater 未启用**。AppImage、DEB、RPM 与 Flatpak 都由对应包格式的
  手工/包管理器升级路径处理；在有受签名的软件源或 Flathub 发布闭环前，不
  发布看似可用的通用应用内更新。
- 迁移说明：v0.2.13 的 NSIS 客户端会优先识别新的精确 NSIS key；该版本的
  MSI 客户端则会因不再提供通用 Windows key 而提示“无适用更新”。这是有意
  fail-closed，用户需手动安装一次同架构的下一版 MSI，之后控制器会明确说明
  MSI 的手动/Store 更新路径，而不会安装错误的 NSIS 包。
- 旧版本（无 pubkey 的 v0.2.x）没有自动更新迁移路径，用户需手动安装一次
  首个含 updater 的版本（v0.2.2+）。

## 8. 心跳 soak 门禁（重要版本发布前）

存活性心跳默认约 40 秒无响应即重启。发布流水线内置 5 分钟负载 soak
（`scripts/load-soak.ts`：真实 harness + CPU 燃烧进程 + 探针延迟记录，
生产默认旋钮）。重要版本（心跳变更 / harness 大版本升级）在 Publish 前
额外手动执行：

```bash
node scripts/load-soak.ts --duration-min 30 --cpu-burn 4
```

确认全程无误杀（`bad=0`）再 Publish。局限见脚本头注释：CPU 争抢覆盖探针
超时路径，事件循环阻塞由 verify-heartbeat 的 hang case 覆盖。
