# Changelog

This file records user-facing DSH Desktop changes. For immutable release
assets, checksums, and the full technical record, use the linked GitHub
Release for each version.

Chinese edition: [CHANGELOG.zh-CN.md](CHANGELOG.zh-CN.md).

## [0.2.18] — 2026-08-27

### AppImage desktop runtime compatibility

- Stops bundling build-host Wayland, GLib, GIO, and nghttp2 ABI libraries in
  AppImages, allowing the application to use the compatible desktop libraries
  supplied by the target Linux system.
- Bundles the GStreamer plugins required by the embedded WebView.
- Preserves an explicitly selected GTK backend while retaining X11 as the
  default fallback for broader AppImage compatibility.
- Packages official, unmodified DeepSeek Harness `0.1.1-rc.2`, Node.js
  `24.19.0`, and `dsh-sidecar` `0.2.7`.

**Release:** [v0.2.18](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/tag/v0.2.18)

## [0.2.17] — 2026-08-24

### Strict Snap access restored

- Restores the desktop-portal access required by the embedded Harness inside a
  strictly confined Snap, so it can interact with the desktop session without
  weakening confinement.
- Fixes native select contrast in the controller, including language choices.
- Packages official, unmodified DeepSeek Harness `0.1.1-rc.2`, Node.js
  `24.19.0`, and `dsh-sidecar` `0.2.6`.

**Release:** [v0.2.17](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/tag/v0.2.17)

## [0.2.16] — 2026-08-24

### Strict Snap Store distribution

- Introduces the strict Snap package definition and its reviewed desktop
  interface policy.
- Adds package, runner, and artifact verification for the Snap release path.
- Adds Windows runtime prerequisite guidance and refreshed product screenshots.

**Release:** [v0.2.16](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/tag/v0.2.16)

## [0.2.15] — 2026-08-23

### Published checksum audit

- Makes release publication fail closed when a public GitHub asset name or its
  reported SHA-256 digest differs from the reviewed artifact.
- Prevents a checksum sidecar from being published under a filename that cannot
  verify the downloaded asset.

> v0.2.15 supersedes v0.2.14 for downloads. Use this version or a newer release
> when verifying checksums.

**Release:** [v0.2.15](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/tag/v0.2.15)

## [0.2.14] — 2026-08-23

### Native multi-architecture release line

- Adds native Windows x64/arm64, macOS x64/arm64, and Linux x64/arm64 release
  targets, with architecture checks for bundled Node.js and `node-pty`.
- Repairs Windows installer runtime and deep-link smoke coverage.
- Updates the bundled official Harness to `0.1.1-rc.2` and hardens install
  arbitration and CodeQL coverage.

> The package bytes and digests are valid, but GitHub-normalized filenames made
> the v0.2.14 checksum sidecars impractical to use. Download v0.2.15 or later
> instead.

**Release:** [v0.2.14](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/tag/v0.2.14)

## [0.2.13] — 2026-08-22

### Localized controller and safer tray shortcuts

- Adds Simplified Chinese and English dictionaries, system-language detection,
  manual selection, and a persisted language preference.
- Keeps the controller title, tray, and native macOS menu synchronized with the
  selected language.
- Adds state-gated tray shortcuts to open the controller or Harness, start or
  restart, stop, and quit.
- Updates audited `pnpm`, `getrandom`, `zip`, and `reqwest` dependencies while
  retaining the Rust `ring` TLS provider.

**Release:** [v0.2.13](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/tag/v0.2.13)

## [0.2.12] — 2026-08-22

### Safer plugin lifecycle recovery

- Fixes Windows PATH handling so every path segment is preserved when the
  bundled `pnpm` shim is placed first.
- Hardens plugin installation, activation, removal, process cleanup, and
  recovery behavior.
- Normalizes verbatim Node launch paths so the bundled runtime starts reliably
  from Windows installation paths.

**Release:** [v0.2.12](https://github.com/web-casa/DeepSeek-Harness-Desktop/releases/tag/v0.2.12)
