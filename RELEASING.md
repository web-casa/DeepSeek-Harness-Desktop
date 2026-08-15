# RELEASING.md — 发布手册

发布流程全自动：在 `main` 上打 `v*` tag 即触发完整流水线（质量门 → 双平台
构建 → 内容/签名验证 → draft release）。人工职责只有四步：版本对齐、打 tag、
审阅 draft、Publish。

## 1. 版本对齐（发布前本地完成）

版本单一事实源见 AGENTS.md「版本单一事实源」表。桌面与 sidecar 版本必须
同步 bump，harness pin 若同时升级必须走「Harness 升级启动契约」清单。
改动后本地跑：

```bash
pnpm release:preflight     # 版本对齐 + lock + checksum 表 + npm 边界 + tag 绑定演练
pnpm check && pnpm check:scripts
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace  # 或按 crate 分别跑
cargo vet --locked && cargo deny check
node scripts/verify-runtime.ts && node scripts/verify-heartbeat.ts
node scripts/verify-bundle.ts --self-test && node scripts/verify-signing.ts --self-test
```

## 2. 打 tag（只在 main 上）

```bash
git checkout main && git pull
git tag -a v0.2.1 -m "DeepSeek Harness Desktop 0.2.1"
git push origin v0.2.1
```

**语义**：tag 必须打在 `main` 的提交上（合入之后）。在 feature 分支上打 tag
再 squash-merge、或 main 被 force-push 重写，都会被流水线的 tag-ancestry
闸门拒绝（前置 `tag-gate` job，秒级失败，不浪费构建）。

## 3. 流水线做什么（无需人工）

1. `tag-gate`：tag 提交必须是 `origin/main` 祖先。
2. `quality`（reusable）：fmt / clippy（含 Windows 宿主）/ nextest /
   llvm-cov / deny / vet / 脚本类型与 self-test。
3. `build` ×2（windows-x64 NSIS、macos-arm64 DMG）：下载 Node（SHA-256）
   → prepare-harness（441+ 许可证 + 零链接断言）→ 构建 sidecar → 冒烟 →
   `tauri build` → **verify-bundle**（二进制类型、必需文件、scoped 许可证
   绊线、零符号链接、执行位、无 quarantine）→ **verify-signing**（见下）→
   SHA-256 checksum → 上传制品。
4. `release`：tag 绑定 preflight（`--expect-tag`）→ 下载制品 →
   **draft** GitHub Release（`files: artifacts/**/*`）。

## 4. 签名状态（verify-signing 语义）

- 未配置 `APPLE_CERTIFICATE` / `WINDOWS_CERTIFICATE` secrets → 构建为
  **有意未签名**，流水线断言「确实未签名/未公证」（warn 级，防状态漂移），
  照常发布；README/SECURITY 已如实披露放行方式。
- 配置了 secrets → **强制验签**（macOS: codesign --verify --deep --strict +
  spctl --assess + stapler validate；Windows: Authenticode == Valid），
  任一失败即阻断发布（fail-closed，防假签名）。

## 5. 审阅并 Publish

1. 打开 releases 页的 draft `vX.Y.Z`：核对 4 个资产
   （`*.exe`/`*.dmg` + 各自 `.sha256`）与体积量级。
2. 抽查 checksum：`shasum -a 256 <下载文件>` 对照 `.sha256` 内容。
3. 手动下载安装验证（未签名构建：确认 SmartScreen/Gatekeeper 放行路径可走通）。
4. 点 **Publish release**。完成后更新 README「当前状态」行。

## 6. 回滚/重发

- draft 阶段发现问题：删除 draft + tag 后重新打 tag（tag 重新指向新提交）。
- 已发布：永不覆盖资产；bump patch 版本重新发布，旧版标注。
- workflow_dispatch（不发布）可用于在**不打 tag** 的情况下全流程演练构建
  与验证（`tag` 输入为空时构建默认分支）。

## 7. 更新器（updater）运行语义

- 更新包真实性由内嵌 minisign 公钥校验（`tauri.conf.json` 的
  `plugins.updater.pubkey`），与代码签名无关；私钥存于
  `TAURI_SIGNING_PRIVATE_KEY` secret，**丢失即全部更新失效**——轮换需
  重新生成密钥对、更新 pubkey、发布一次强制全量安装的新版本。
- `latest.json` 由 publish job 的 `scripts/updater-manifest.ts` 在发布后
  自动生成并上传（绝对资产 URL + .sig 内容），fail-closed：缺配对即失败。
- **macOS updater 未启用**（未签名时 Gatekeeper 拒绝未公证更新，见
  C2 评审结论）；签名+公证落地后在 manifest 参数中追加
  `darwin-aarch64` 并启用对应 sig 上传。
- 旧版本（无 pubkey 的 v0.2.x）没有自动更新迁移路径，用户需手动安装一次
  首个含 updater 的版本（v0.2.2+）。

## 8. 心跳 soak 门禁（重要版本发布前）

存活性心跳默认约 40 秒无响应即重启。发布包含心跳变更、或 harness 大版本
升级时，手动执行一次长跑 soak（真实 agent 任务 ≥ 30 分钟），确认无误杀，
再 Publish。
