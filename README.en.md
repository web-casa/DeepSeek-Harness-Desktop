# DeepSeek Harness Desktop

**A community desktop distribution layer**: packages the official DeepSeek
Harness as a native Windows / macOS app, with the Harness **plugin ecosystem**
ready to go. NOT a fork — the real Harness ships intact (Node.js +
node_modules + the Cordis plugin system); the desktop layer only handles
lifecycle and the security boundary: Tauri manages windows, a Rust sidecar
supervises the Harness process tree.

Repo: https://github.com/web-casa/DeepSeek-Harness-Desktop · Downloads: https://github.com/web-casa/DeepSeek-Harness-Desktop/releases
Docs: [SECURITY.md](SECURITY.md) · [FORKING.md](FORKING.md) · [RELEASING.md](RELEASING.md) · [AGENTS.md](AGENTS.md)
Website: https://dsharness.app · **Plugin marketplace: https://cordis.run**
中文文档见 [README.md](README.md)
(Current: v0.2.3 — Windows x64 NSIS / macOS arm64 DMG, unsigned preview builds)

```
┌────────────────────────────────────────────────┐
│ Tauri 2 (bootstrap window + Harness window)    │
│  bootstrap: local Svelte UI, limited desktop IPC│
│  harness:   http://127.0.0.1:<random port>      │
│             original Harness Web UI · zero IPC  │
└───────────────┬────────────────────────────────┘
                │ NDJSON (stdin/stdout)
┌───────────────▼────────────────────────────────┐
│ dsh-sidecar (Rust, no heavy dependencies)       │
│  start/stop/restart · readiness · heartbeat     │
│  Unix: process group    Windows: Job Object     │
└───────────────┬────────────────────────────────┘
                │
┌───────────────▼────────────────────────────────┐
│ Bundled Node 24 + @deepseek-ai/dsh (pinned)     │
│   node lib/bin.js web --host 127.0.0.1 --port 0 │
│   plugins land in DSH_HOME/profiles/web         │
└────────────────────────────────────────────────┘
```

## ✨ Plugin ecosystem

The Harness's native capabilities — skills, tools, model routes, MCP
integrations, presets — are all **Cordis plugins**. The desktop build ships
the whole ecosystem and adds safety rails around it.

### Discover plugins: cordis.run

The app's settings page has a one-click link to the official **plugin
marketplace, [cordis.run](https://cordis.run)** — browse plugins and
presets, then bring the package name back to install.

### Install plugins (`dsh plugin`)

Plugins are npm packages; the official mechanism is
`dsh plugin --profile web add <pkg>`. In the desktop build, run it with the
**exact pinned CLI that ships in the bundle** (system pnpm required):

```bash
# macOS
DSH_HOME="<dshHome from the diagnostics page>" node "/Applications/DeepSeek Harness Desktop.app/Contents/Resources/runtime/harness/node_modules/@deepseek-ai/dsh/lib/bin.js" plugin --profile web add <package-name>

# Windows (PowerShell)
$env:DSH_HOME="<dshHome from the diagnostics page>"; node "<install-dir>\runtime\harness\node_modules\@deepseek-ai\dsh\lib\bin.js" plugin --profile web add <package-name>
```

- `dshHome` is shown on the app's diagnostics page — copy it; `--profile web`
  matches the profile the desktop runs;
- plugins land in `<dshHome>/profiles/web/` (user data, never the install
  directory);
- hit "Restart Harness" in the app to activate.

### Presets (`.dshpreset`): the configuration-pack sibling

A preset is a zip (`preset.yml` + `agent.cordis.yml`) packaging a set of
plugin rows into a shareable agent configuration. The desktop settings page
offers a **safe import/export**:

- pre-import validation: paths / symlinks / quotas (16/32/12 MiB, 512 files)
  / a secret scan;
- two-phase confirmation + atomic install into
  `<dshHome>/.agent-presets/` — the exact location upstream discovers, so
  the preset appears in the Harness settings page immediately;
- export runs the same checks and refuses symlink impersonation.

### Trust model (important)

**Plugins and presets run inside the Harness process with the same
privileges as the Agent** (upstream's words: "carries the same trust as
shell access"). The desktop's validation blocks malformed archives — it
cannot block a "trusted" package whose content is hostile — **only install
from trusted sources**. See [SECURITY.md](SECURITY.md).

### The rails the desktop layer adds

- plugins/presets write to user data only, never the install directory
  (zero-symlink invariant);
- preset directory permissions match upstream (0700/0600);
- the "upstream can discover it" claim is proven in CI: the e2e drives the
  real upstream `discoverPresets`;
- the updater keeps you on a Harness version compatible with new plugins.

## Core principles

1. **Never fork the Harness**: `runtime/package.json` only pins
   `@deepseek-ai/dsh`; upgrading means bumping one version + CI smoke.
2. **Never modify the Harness Web UI**: the desktop loads the original UI
   (`--port 0` auto port + the official readiness line is the handshake).
3. **Security boundary**: the Harness window's capability set is EMPTY —
   remote content has no Tauri IPC surface; the app's own commands are
   granted to the bootstrap window only via the app ACL.
4. **Single source of truth for versions**: `runtime/runtime-manifest.json`
   pins desktop / harness / node / sidecar.

## Desktop-layer features

- ✅ **Windows auto-updater**: update packages verified by an embedded
  minisign pubkey (independent of code signing); macOS updater activates
  once signed + notarized
- ✅ **Hung-process self-healing**: a heartbeat detects "alive but
  unresponsive" Harness and restarts it (backoff + attempt cap)
- ✅ **Preset import/export**: see the plugin ecosystem section
- ✅ Single-instance lock, window-state memory, crash auto-restart, macOS
  close-to-tray
- ✅ **Diagnostics & feedback**: one-click diagnostics zip export
  (best-effort redaction), copy diagnostics, prefilled issue reports
- ✅ Privacy defaults: session telemetry OFF (`DSH_TELEMETRY_DISABLED=1`);
  child env sanitized (`NODE_OPTIONS`/`NODE_PATH`/`npm_config_*`/dynamic
  linker injection keys never reach the Harness)
- ✅ Crash-view actions: retry / export diagnostics / quit

## Layout

```
Cargo.toml                  cargo workspace root (single Cargo.lock)
crates/dsh-sidecar/         Rust supervisor (standalone crate, builds on 3 OSes)
runtime/                    version pin + npm lock + install-script allowlist
scripts/                    download Node / prepare runtime / build sidecar / e2e smokes / preset & updater verification
src/                        bootstrap frontend (Svelte 5 + Vite)
src-tauri/                  Tauri shell: state machine, ACL commands, preset boundary, capabilities
deny.toml                   cargo-deny supply-chain policy
supply-chain/               cargo-vet audit records
.github/workflows/          test / release (with soak) / codeql / dependency-review
```

## Development

```bash
pnpm install                       # frontend + tauri CLI
pnpm check && pnpm check:scripts   # svelte-check + script type checks
pnpm runtime:all                   # download Node + prepare runtime + build sidecar
pnpm runtime:verify                # e2e smoke (sidecar→node→dsh web→HTTP 200→no orphans)
pnpm test:scripts                  # node:test unit suite (zero deps)
pnpm tauri dev                     # desktop dev mode
pnpm icons                         # regenerate platform icons
```

> If your `~/.cargo` is read-only, set `CARGO_HOME=<repo>/.tmp/cargo-home` first.

### Quality gates

| Layer | Tool | Bar |
|---|---|---|
| Frontend | `tsc --noEmit` + `svelte-check` | 0 errors (+ dedicated script tsconfig) |
| Rust tests | `cargo nextest` | sidecar 35 + Tauri 35 (proptests, process-level integration, NDJSON goldens, preset attack fixtures; also executed on a Windows host) |
| Rust coverage | `cargo llvm-cov` | sidecar ≥ 50%, Tauri ≥ 25% (lifecycle/state-machine/preset focused) |
| Rust lints | `cargo fmt --check` + `clippy -D warnings` | unwrap/expect/panic denied in production code |
| Supply chain | `cargo-deny` | advisories (yanked=deny) / licenses / bans / sources |
| Supply-chain audit | `cargo-vet --locked` | 70 full + 2 delta audits + exemption baseline |
| npm audits | `npm audit` (runtime) + `pnpm audit` (root) | blocking at `--audit-level=high` |
| Security scan | CodeQL | rust + javascript-typescript + actions |
| PR dependency gate | Dependency Review | fail-on-severity low + GPL/AGPL/LGPL denied |
| Bundles | `verify-bundle.ts` + `verify-signing.ts` + `checksums.ts` | content assertions + signing state (fail-closed) + SHA-256 |

### Core scripts (Node ≥ 24 runs TS natively, zero dependencies)

| Script | Purpose |
|---|---|
| `download-node.ts` | Download the official Node binary per the manifest (SHA-256) |
| `prepare-harness.ts` | `npm ci` (flat) → materialize (zero symlinks) → collect every dependency LICENSE |
| `build-sidecar.ts` | `cargo build --release`, staged to resources/runtime |
| `verify-runtime.ts` | Full-chain smoke: boot → readiness → HTTP 200 → restart → shutdown → no orphans |
| `verify-heartbeat.ts` | Hung-detection e2e (hang case + healthy negative case) |
| `verify-preset.ts` | Drives the REAL upstream `discoverPresets` to prove preset discoverability |
| `load-soak.ts` | Load soak (CPU burners + probe latency, production heartbeat knobs) |
| `updater-manifest.ts` | Post-publish `latest.json` (absolute asset URLs + minisign signature) |
| `verify-bundle.ts` / `verify-signing.ts` | Bundle content / signing-state assertions (each with `--self-test`) |

## dsh-sidecar protocol (NDJSON, stdin commands / stdout events)

```text
→ {"id":1,"command":"start","node":"…","script":"…/lib/bin.js",
   "args":["web","--host","127.0.0.1","--port","0"],
   "cwd":"…/harness","env":{"DSH_HOME":"…"}}
← {"type":"ack","id":1,"ok":true} → {"type":"starting"} → {"type":"ready","url":"…"}
→ {"command":"restart" | "shutdown" | "status"}
← {"type":"stopped","code":0} · {"type":"crashed","code":1} · {"type":"status",…}
[stdin EOF]  → graceful tree shutdown, sidecar exits 0
```

Timeouts via `DSH_READY_TIMEOUT_MS` (default 120s) / `DSH_SHUTDOWN_GRACE_MS`
(default 10s); heartbeat knobs `DSH_HEARTBEAT_INTERVAL_MS` (0 = disabled) /
`DSH_HEARTBEAT_FAIL_LIMIT` / `DSH_HEARTBEAT_READ_TIMEOUT_MS`.

### Process-tree guarantees

| Scenario | Cleanup path |
|---|---|
| Normal exit / app close | Tauri `RunEvent::Exit` → shutdown → graceful stop (unix SIGTERM to process group; win CTRL_C) → force after timeout |
| App crash (sidecar survives) | sidecar stdin EOF detection → force-kill tree → exit 0 |
| Group signals (Ctrl+C / `timeout`) | sidecar signal handlers → cleanup → exit 0 |
| Windows last resort | Job Object `KILL_ON_JOB_CLOSE`; `TerminateJobObject` after grace |

### Licensing & third-party attribution

- This project: MIT (see `LICENSE`).
- The bundle ships the original DeepSeek Harness (MIT, its LICENSE/README
  included) plus the npm dependency tree; `prepare-harness` collects every
  package license into `runtime/harness/licenses/`.

## CI

- **test.yml**: three-platform runtime smoke (ubuntu/windows/macos-14) +
  heartbeat + preset-discovery e2e; Linux quality gates (unit tests,
  coverage, clippy, deny, vet, npm audits); Windows host executes the
  workspace test suite.
- **release.yml**: tag → quality gates → **5-minute load soak** → bundle
  (NSIS/DMG) → content/signing-state assertions → draft release +
  `latest.json`. `workflow_dispatch` is a build+verify-only test channel.

## Version upgrade flow

```
Upstream @deepseek-ai/dsh rc.x → rc.y
  → follow the AGENTS.md startup-contract checklist
  → bump runtime/package.json + runtime-manifest.json
  → cd runtime && npm install (new install scripts need allowlisting)
  → CI three-platform smoke green
  → tag v* to release
```

## Current status & known limits (v0.2.3)

- ✅ CI green (quality gates + three-platform smoke + release pipeline with
  soak and updater artifacts)
- ✅ v0.2.3 draft ready (Windows updater + preset import/export + website /
  marketplace entry points)
- ✅ Windows auto-updater (minisign pubkey verification; macOS updater
  activates once signed + notarized)
- ⏳ Not yet: code signing / notarization (macOS updater depends on it), an
  in-app plugin installer UI (the CLI path is documented above)
- Unsigned builds: Windows SmartScreen and macOS Gatekeeper require manual
  user approval
- Linux is a development target only; node-pty has no prebuild there (the
  Web UI boots fine; shipped bundles carry the prebuild)
