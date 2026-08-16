# DeepSeek Harness Desktop

**Community desktop distribution layer**: packages the official DeepSeek
Harness as a native Windows / macOS application. This is NOT a fork — the real
Harness ships intact (Node.js + node_modules + Cordis plugins); the desktop
layer only handles lifecycle: Tauri manages windows, a Rust sidecar supervises
the Harness process.

Repo: https://github.com/web-casa/DeepSeek-Harness-Desktop · Downloads: https://github.com/web-casa/DeepSeek-Harness-Desktop/releases
Docs: [SECURITY.md](SECURITY.md) · [FORKING.md](FORKING.md) · [RELEASING.md](RELEASING.md) · [AGENTS.md](AGENTS.md)
Website: https://dsharness.app · Plugin marketplace: https://cordis.run
(Current: v0.2.3 — Windows x64 NSIS / macOS arm64 DMG, unsigned preview builds)

> 中文文档见 [README.md](README.md)。

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
│  start/stop/restart · readiness · crash detect  │
│  Unix: process group    Windows: Job Object     │
└───────────────┬────────────────────────────────┘
                │
┌───────────────▼────────────────────────────────┐
│ Bundled Node 24 + @deepseek-ai/dsh (pinned)     │
│   node lib/bin.js web --host 127.0.0.1 --port 0 │
│   DSH_HOME isolated from the CLI's ~/.dsh       │
└────────────────────────────────────────────────┘
```

## Core principles

1. **Never fork the Harness**: `runtime/package.json` only pins
   `@deepseek-ai/dsh`; upgrading means bumping one version + CI smoke.
2. **Never modify the Harness Web UI**: the desktop loads the original UI
   (`--port 0` auto port + the official readiness line
   `dsh web: http://127.0.0.1:<port>` is the whole handshake).
3. **Security boundary**: the Harness window's capability set is EMPTY —
   remote content has no Tauri IPC surface; the app's own commands are
   granted to the bootstrap window only via the app ACL.
4. **Single source of truth for versions**: `runtime/runtime-manifest.json`
   pins desktop / harness / node / sidecar.

## Layout

```
Cargo.toml                  cargo workspace root (single Cargo.lock)
crates/dsh-sidecar/         Rust supervisor (standalone crate, builds on 3 OSes)
runtime/                    version pin + npm lock + install-script allowlist
scripts/                    download Node / prepare runtime / build sidecar / e2e smoke
src/                        bootstrap frontend (Svelte 5 + Vite)
src-tauri/                  Tauri shell: state machine, ACL commands, capabilities
deny.toml                   cargo-deny supply-chain policy
supply-chain/               cargo-vet audit records (audits/config/imports)
.github/workflows/          test / release / codeql / dependency-review
```

## Development

```bash
pnpm install                       # frontend + tauri CLI
pnpm check && pnpm check:scripts   # svelte-check + script type checks
pnpm runtime:all                   # download Node + prepare runtime + build sidecar
pnpm runtime:verify                # e2e smoke (sidecar→node→dsh web→HTTP 200→no orphans)
pnpm tauri dev                     # desktop dev mode
pnpm icons                         # regenerate platform icons
```

> If your `~/.cargo` is read-only, set `CARGO_HOME=<repo>/.tmp/cargo-home` first.

### Quality gates

| Layer | Tool | Bar |
|---|---|---|
| Frontend | `tsc --noEmit` + `svelte-check` | 0 errors (plus a dedicated `tsconfig.scripts.json` for the Node TS scripts) |
| Rust tests | `cargo nextest` | sidecar + Tauri state machine (proptests, platform integration tests, NDJSON golden contract) |
| Rust coverage | `cargo llvm-cov` | sidecar ≥ 50%, Tauri ≥ 25% (lifecycle/state-machine focused) |
| Rust lints | `cargo fmt --check` + `clippy -D warnings` | unwrap/expect/panic denied in production code |
| Supply chain | `cargo-deny` | advisories (yanked=deny) / licenses / bans / sources |
| Supply-chain audit | `cargo-vet --locked` | exemption baseline; new crates must be audited or exempted |
| Security scan | CodeQL | rust + javascript-typescript + actions |
| PR dependency gate | Dependency Review | fail-on-severity low + GPL/AGPL/LGPL denied |
| Bundles | `verify-bundle.ts` + `checksums.ts` | 7z/hdiutil content assertions + manifest match + SHA-256 |

### Scripts (Node ≥ 24 runs TS natively, zero dependencies)

| Script | Purpose |
|---|---|
| `download-node.ts` | Download the official Node binary per the manifest (SHA-256 verified) |
| `prepare-harness.ts` | `npm ci` (flat layout) → materialize (zero symlinks) into bundle resources; cross-checks manifest vs installed version |
| `build-sidecar.ts` | `cargo build --release` for the host triple, staged to resources/runtime |
| `verify-runtime.ts` | Smoke: boot → readiness → HTTP 200 → status → restart → shutdown → orphan check; `--runtime-dir` for relocation |
| `check-runtime-links.ts` | Asserts the staged harness tree has zero symlinks |
| `relocate-runtime.ts` | Materialized copy for the relocation smoke |
| `verify-bundle.ts` | Bundle content assertions (7z/hdiutil); binary types, full runtime tree, node-pty prebuild, zero symlinks; `--self-test` runs anywhere |

## Presets & plugins

- **Agent Presets**: `.dshpreset` archives (`preset.yml` + `agent.cordis.yml`).
  The Harness Web UI manages presets natively; the desktop settings page adds
  a **safe import/export**: path/symlink/quota validation plus a secret scan,
  two-phase confirmation, and an atomic install into
  `<dshHome>/.agent-presets/` (matching upstream's discovery). **Presets run
  with the same privileges as the Agent — import trusted sources only.**
- **Plugins (the pnpm line)**: `dsh plugin --profile web add <pkg>` uses the
  system pnpm with a workspace under `<dshHome>/profiles/web/`. In the desktop
  build this is manual (see the trust model in SECURITY.md).

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

Timeouts via `DSH_READY_TIMEOUT_MS` (default 120s) / `DSH_SHUTDOWN_GRACE_MS` (default 10s).

### Process-tree guarantees

| Scenario | Cleanup path |
|---|---|
| Normal exit / app close | Tauri `RunEvent::Exit` → shutdown → graceful stop (unix SIGTERM to process group; win CTRL_C → node SIGINT) → force after timeout |
| App crash (sidecar survives) | sidecar stdin EOF detection → force-kill tree → exit 0 |
| Group signals (Ctrl+C / `timeout`) | sidecar SIGTERM/SIGINT/SIGHUP handlers → cleanup → exit 0 |
| Windows last resort | Job Object `KILL_ON_JOB_CLOSE`; `TerminateJobObject` after grace |
| Note | Windows graceful stop requires the hidden console allocated at sidecar startup (`platform.rs`); without it, the force path takes over |

### Licensing & third-party attribution

- This project: MIT (see `LICENSE`).
- The bundle ships the original DeepSeek Harness (MIT, its LICENSE/README
  included) plus the npm dependency tree; `prepare-harness` collects every
  package license into `runtime/harness/licenses/` inside the bundle.

## CI

- **test.yml**: sidecar tests on ubuntu + three-platform runtime smoke
  (boot → HTTP 200 → restart → shutdown → no orphans + relocation).
- **release.yml**: tag → gates → bundle (NSIS/DMG) → bundle content
  assertions → signing-state verification → artifacts → draft release.
  `workflow_dispatch` is a build+verify-only test channel (no publish).

## Version upgrade flow

```
Upstream @deepseek-ai/dsh rc.x → rc.y
  → bump runtime/package.json + runtime-manifest.json
  → cd runtime && npm install (refresh lock; new install scripts need allowlisting)
  → CI three-platform smoke green
  → tag v* to release (new Node/Harness ship with the Desktop)
```

## Current status & known limits

- ✅ CI green on all three platforms (quality gates + smoke incl. relocation)
- ✅ Single-instance lock, window-state memory, crash auto-restart,
  diagnostics, macOS close-to-tray
- ✅ Privacy defaults: session telemetry OFF (`DSH_TELEMETRY_DISABLED=1`, see
  SECURITY.md); child env sanitized (`NODE_OPTIONS`/`NODE_PATH`/`npm_config_*`
  never reach the Harness)
- ✅ Windows auto-updater (update authenticity enforced by an embedded
  minisign pubkey; see RELEASING.md)
- ⏳ Not yet: code signing / notarization (macOS updater stays off until
  then), bundled pnpm for plugins
- Unsigned builds: Windows SmartScreen and macOS Gatekeeper require manual
  user approval
- Linux is a development target only; node-pty has no prebuild there (the
  Web UI boots fine; shipped Windows/macOS bundles carry the prebuild)
