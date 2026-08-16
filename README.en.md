# DeepSeek Harness Desktop

Packages the official DeepSeek Harness as a native Windows / macOS app, with
the Harness **plugin ecosystem** ready to go. Not a fork: the Harness ships
intact; the desktop layer only handles lifecycle and the security boundary.

| Repo | [web-casa/DeepSeek-Harness-Desktop](https://github.com/web-casa/DeepSeek-Harness-Desktop) |
| Downloads | [Releases](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases) |
| Website | [dsharness.app](https://dsharness.app) |
| Plugin marketplace | [cordis.run](https://cordis.run) |
| Docs | [SECURITY](SECURITY.md) · [FORKING](FORKING.md) · [RELEASING](RELEASING.md) · [AGENTS](AGENTS.md) |
| Version | v0.2.5 · Windows x64 NSIS / macOS arm64 DMG · unsigned preview · [中文](README.md) |

> ⚠️ **macOS users**: the Apple developer certificate is still being
> applied for, so the app is unsigned. If Gatekeeper blocks the first
> launch, run:
> ```bash
> xattr -dr com.apple.quarantine "/Applications/DeepSeek Harness Desktop.app"
> ```
> (prefix with `sudo` if it reports a permission error.) This step should
> disappear in the next release once signing + notarization land.

## Features

| | |
|---|---|
| 🔌 **Plugin ecosystem** | Cordis plugins ship with the bundle; in-app install/uninstall; safe preset import/export |
| 🔄 **Auto-updater** (Windows) | Update packages verified by an embedded minisign pubkey; macOS activates once signed + notarized |
| 💓 **Hung-process self-healing** | Heartbeat detects an unresponsive Harness and restarts it (backoff + cap) |
| 🛡️ **Security boundary** | Harness window has zero IPC; app commands granted to the local window only; env sanitization |
| 🔒 **Privacy defaults** | Session telemetry OFF; child env sanitized (NODE_OPTIONS, loader injection keys, …) |
| 🧰 **Diagnostics & feedback** | One-click diagnostics zip (best-effort redaction), copy diagnostics, prefilled issue reports |
| 🪟 **Desktop UX** | Single instance, window-state memory, crash auto-restart, macOS close-to-tray |

## ✨ Plugin ecosystem

The Harness's skills, tools, model routes, MCP integrations and presets are
**all Cordis plugins**.

**Discover**: the settings page links straight to the plugin marketplace,
[cordis.run](https://cordis.run).

**Install**: the settings page's "Plugins (user-installed)" section takes an
npm package name and installs/uninstalls it — the desktop layer drives the
official `dsh plugin --profile web add/remove`, with both the pinned CLI and
pnpm bundled (**no system pnpm needed**), live install logs, and cancel
(cleaning the whole node → dsh → pnpm process tree).

On a cordis.run plugin page, "Install in desktop app" opens
`dsharness://plugin/install?v=1&name=<package>&source=<plugin-page>`. The
desktop shell validates the URL strictly in Rust, asks for confirmation, and
only then starts the install. Older desktop builds and market pages without
the deep-link button can still use copy-paste package names. Command-line
equivalent (for power users):

```bash
# macOS
DSH_HOME="<dshHome from the diagnostics page>" node "/Applications/DeepSeek Harness Desktop.app/Contents/Resources/runtime/harness/node_modules/@deepseek-ai/dsh/lib/bin.js" plugin --profile web add <package-name>
# Windows PowerShell
$env:DSH_HOME="<dshHome from the diagnostics page>"; node "<install-dir>\runtime\harness\node_modules\@deepseek-ai\dsh\lib\bin.js" plugin --profile web add <package-name>
```

Plugins land in `<dshHome>/profiles/web/` (user data, never the install
directory); hit "Restart Harness" in the app to activate.

**Presets** (`.dshpreset`): a set of plugin rows packaged as a shareable agent
configuration. The settings page offers safe import/export/delete —
path/symlink/quota/secret validation → two-phase confirmation → atomic
install into `<dshHome>/.agent-presets/`, visible in the Harness settings
immediately. The settings page also re-validates preset-root health on every
look: broken (missing/unreadable/empty `agent.cordis.yml` — upstream refuses
to mount), unsafe (symlinks and other id-occupying entries upstream skips),
and missing metadata (`preset.yml`) rows are surfaced and deletable. Before
deleting: the desktop layer removes the directory directly and does not
touch the Harness's default-preset setting — if you delete the current
default, pick another default in the Harness settings or the next session
may fail to start.

> ⚠️ **Trust model**: plugins and presets run inside the Harness process
> with **the same privileges as the Agent**. The desktop validation blocks
> malformed archives, not hostile content in a trusted package — only
> install from trusted sources. See [SECURITY.md](SECURITY.md).

## Architecture

```
Tauri 2 shell
 ├─ bootstrap window: local Svelte UI · limited desktop IPC (22 commands, ACL-gated)
 └─ harness window: http://127.0.0.1:<random port> · original Harness Web UI · zero IPC
        │ NDJSON (stdin/stdout)
dsh-sidecar (Rust, no heavy dependencies)
 ├─ start/stop/restart · readiness · heartbeat · tree cleanup
 └─ Unix process group / Windows Job Object
        │
Bundled Node 24 + @deepseek-ai/dsh (pinned)
 └─ node lib/bin.js web --host 127.0.0.1 --port 0 · DSH_HOME isolated from the CLI
```

## Core principles

1. **Never fork**: pin `@deepseek-ai/dsh` only; an upstream release = a
   version bump + CI smoke.
2. **Never modify the Web UI**: the original UI loads as-is; `--port 0` +
   the readiness line is the whole handshake.
3. **Security boundary**: the harness window's capability set is empty;
   app commands are ACL-granted to the bootstrap window only.
4. **Single source of truth**: `runtime/runtime-manifest.json`.

## Development

```bash
pnpm install && pnpm check && pnpm check:scripts   # deps + type checks
pnpm runtime:all && pnpm runtime:verify           # stage runtime + e2e smoke
pnpm test:scripts                                 # node:test unit suite (zero deps)
pnpm tauri dev                                    # desktop dev mode
```

> Read-only `~/.cargo`? `export CARGO_HOME=<repo>/.tmp/cargo-home`.

### Quality gates

| Layer | Gates |
|---|---|
| Frontend | `tsc --noEmit` + `svelte-check`, 0 errors |
| Rust | `cargo nextest` (sidecar 35 + Tauri 35, also run on a Windows host) · llvm-cov ≥50%/25% · `clippy -D warnings` |
| Supply chain | `cargo deny` · `cargo vet --locked` (70 full + 2 delta + exemptions) · `npm audit`/`pnpm audit` block high |
| Security scan | CodeQL (rust/js-ts/actions) · Dependency Review |
| Bundles | `verify-bundle` + `verify-signing` (fail-closed) + SHA-256 |
| Release | 5-minute load soak · updater artifacts + `latest.json` |

### Layout

```
crates/dsh-sidecar/   Rust supervisor        runtime/    pin + lock + allowlist
scripts/              build/smoke/verify     src/        Svelte frontend
src-tauri/            Tauri shell (state machine / ACL / preset boundary)
deny.toml + supply-chain/   policy & audits   .github/workflows/   CI
```

## Release & versioning

- CI: pushes/PRs run quality gates + three-platform smoke; a `v*` tag runs
  the full pipeline (gates → soak → bundle → content/signing assertions →
  draft release + `latest.json`).
- Harness upgrades follow the startup-contract checklist in
  [AGENTS.md](AGENTS.md); the release flow lives in [RELEASING.md](RELEASING.md).
- Status: v0.2.5 in development (cordis.run deep-link install confirmation;
  v0.2.4 is the current release, and the v0.2.3 draft is obsolete and will
  not be published).
- Known limits: unsigned (manual SmartScreen/Gatekeeper approval); macOS
  updater pending signing; Linux is dev-only.
- License: MIT; the bundled Harness and every dependency license ship in
  `runtime/harness/licenses/`.
