<script lang="ts">
  import { onMount } from "svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import {
    getStatus,
    getLogs,
    getVersions,
    getDiagnostics,
    restart,
    shutdown,
    openHarness,
    checkUpdate,
    installUpdateAndRestart,
    exportDiagnostics,
    quitApp,
    listUserPresets,
    previewPreset,
    importPreset,
    exportPreset,
    onEvent,
    onUpdateProgress,
    type Status,
    type StatusPayload,
    type PresetPreview,
    type UpdateInfo,
    type Versions,
  } from "./lib/api";

  let status = $state<Status>("idle");
  let url = $state<string | null>(null);
  let pid = $state<number | null>(null);
  let lastError = $state<string | null>(null);
  let versions = $state<Versions>({
    desktop: "…",
    harness: "…",
    node: "…",
    sidecar: "…",
  });
  let logs = $state<[string, string][]>([]);
  let inTauri = $state(true);
  let busy = $state(false);
  let logsOpen = $state(false);
  let toast = $state<string | null>(null);
  // Suppresses the brief "已停止" flash while a user-initiated restart
  // passes through the sidecar's stopping → stopped → starting sequence.
  let restartInFlight = $state(false);
  let updateInfo = $state<UpdateInfo | null>(null);
  let updateBusy = $state(false);
  let updateError = $state<string | null>(null);
  let updatePercent = $state<number | null>(null);
  let userPresets = $state<string[]>([]);
  let presetPreview = $state<PresetPreview | null>(null);
  let presetError = $state<string | null>(null);
  let presetBusy = $state(false);

  const STATUS_TEXT: Record<Status, string> = {
    idle: "等待启动",
    starting: "正在启动 Harness…",
    running: "Harness 运行中",
    stopping: "正在停止 Harness…",
    stopped: "Harness 已停止",
    crashed: "Harness 进程异常退出",
  };

  function apply(p: StatusPayload) {
    if (restartInFlight && p.status === "stopped") {
      // Keep showing the starting view; the sidecar will emit `starting`
      // (or `ready`) right after and clear the flag there.
      lastError = p.lastError;
      return;
    }
    status = p.status;
    url = p.url;
    pid = p.pid;
    lastError = p.lastError;
    if (p.versions) versions = p.versions;
    if (p.status === "starting" || p.status === "running") {
      restartInFlight = false;
    }
  }

  function showToast(message: string) {
    toast = message;
    setTimeout(() => {
      if (toast === message) toast = null;
    }, 2600);
  }

  async function doRestart() {
    busy = true;
    restartInFlight = true;
    try {
      await restart();
      showToast("已请求重新启动");
    } catch (e) {
      restartInFlight = false;
      showToast(`操作失败：${e}`);
    }
    busy = false;
  }

  async function doShutdown() {
    busy = true;
    try {
      await shutdown();
      showToast("已停止 Harness");
    } catch (e) {
      showToast(`操作失败：${e}`);
    }
    busy = false;
  }

  async function doOpen() {
    try {
      await openHarness();
    } catch (e) {
      showToast(`操作失败：${e}`);
    }
  }

  async function doCheckUpdate() {
    updateBusy = true;
    updateError = null;
    try {
      const info = await checkUpdate();
      updateInfo = info;
      if (info.unsupported) {
        showToast("当前平台不支持自动更新");
      } else if (!info.available) {
        showToast("已是最新版本");
      }
    } catch (e) {
      updateError = `检查更新失败：${e}`;
    }
    updateBusy = false;
  }

  async function doInstallUpdate() {
    updateBusy = true;
    updateError = null;
    updatePercent = null;
    try {
      showToast("正在下载更新，完成后自动重启…");
      await installUpdateAndRestart();
    } catch (e) {
      updateError = `更新失败：${e}`;
      updateBusy = false;
      updatePercent = null;
    }
  }

  async function doExportDiagnostics() {
    try {
      await exportDiagnostics();
      showToast("诊断信息已导出");
    } catch (e) {
      showToast(`导出失败：${e}`);
    }
  }

  async function doQuitApp() {
    try {
      await quitApp();
    } catch (e) {
      showToast(`退出失败：${e}`);
    }
  }

  async function openSite(url: string) {
    try {
      await openUrl(url);
    } catch (e) {
      showToast(`打开失败：${e}`);
    }
  }

  async function reportIssue() {
    try {
      const d = await getDiagnostics();
      const platform = (d.platform as { os?: string; arch?: string }) ?? {};
      const body = [
        `版本：desktop ${versions.desktop} · harness ${versions.harness}`,
        `平台：${platform.os ?? "?"}/${platform.arch ?? "?"}`,
        "",
        "请在此描述问题与复现步骤；",
        "诊断信息请用「导出诊断」按钮生成 zip 后拖入此处（可能含敏感信息，请自行核对）。",
      ].join("\n");
      const url = new URL("https://github.com/web-casa/DeepSeek-Harness-Desktop/issues/new");
      url.searchParams.set("template", "bug_report.md");
      url.searchParams.set("labels", "bug");
      url.searchParams.set("title", "[Bug] 来自桌面的问题报告");
      url.searchParams.set("body", body);
      await openUrl(url.toString());
    } catch (e) {
      showToast(`打开失败：${e}`);
    }
  }

  async function refreshPresets() {
    try {
      userPresets = await listUserPresets();
    } catch {
      /* non-fatal */
    }
  }

  async function doPreviewPreset() {
    presetBusy = true;
    presetError = null;
    try {
      presetPreview = await previewPreset();
    } catch (e) {
      if (String(e) !== "cancelled") presetError = `读取失败：${e}`;
    }
    presetBusy = false;
  }

  async function doImportPreset() {
    presetBusy = true;
    presetError = null;
    try {
      const id = await importPreset();
      presetPreview = null;
      showToast(`预设 ${id} 已导入（在 Harness 设置页可见）`);
      await refreshPresets();
    } catch (e) {
      presetError = `导入失败：${e}`;
    }
    presetBusy = false;
  }

  async function doExportPreset(id: string) {
    if (presetBusy) return;
    presetBusy = true;
    try {
      await exportPreset(id);
      showToast(`预设 ${id} 已导出`);
    } catch (e) {
      if (String(e) !== "cancelled") showToast(`导出失败：${e}`);
    }
    presetBusy = false;
  }

  async function copyDiagnostics() {
    try {
      const payload = await getDiagnostics();
      await navigator.clipboard.writeText(JSON.stringify(payload, null, 2));
      showToast("诊断信息已复制到剪贴板");
    } catch (e) {
      showToast(`复制失败：${e}`);
    }
  }

  // Initial data load (async onMount is fine here — no cleanup needed).
  onMount(async () => {
    try {
      const [st, ver, lg] = await Promise.all([getStatus(), getVersions(), getLogs()]);
      apply(st);
      versions = ver;
      logs = lg;
    } catch {
      inTauri = false;
    }
    refreshPresets();
    // Silent boot-time update check: only inform, never prompt.
    try {
      const info = await checkUpdate();
      if (info.available) updateInfo = info;
    } catch {
      /* offline / draft release: stay silent */
    }
  });

  // Event subscription in a $effect: async onMount cannot return a cleanup
  // (Svelte ignores non-function returns), so the listener would never be
  // unbound. Effects handle async registration + cancellation properly.
  $effect(() => {
    if (!inTauri) return;
    let cancelled = false;
    let unlistenFn: (() => void) | null = null;
    onEvent((p) => apply(p)).then((fn) => {
      if (cancelled) fn();
      else unlistenFn = fn;
    });
    return () => {
      cancelled = true;
      unlistenFn?.();
    };
  });

  $effect(() => {
    if (!inTauri) return;
    let cancelled = false;
    let unlistenFn: (() => void) | null = null;
    onUpdateProgress((p) => {
      if (p.total && p.total > 0) {
        updatePercent = Math.min(100, Math.round((p.downloaded / p.total) * 100));
      }
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenFn = fn;
    });
    return () => {
      cancelled = true;
      unlistenFn?.();
    };
  });

  // Poll logs only while the console is open; clean up via effect return.
  $effect(() => {
    if (!inTauri || !logsOpen) return;
    const timer = setInterval(async () => {
      logs = await getLogs();
    }, 1000);
    return () => clearInterval(timer);
  });

  function stepClass(target: "check" | "start" | "ready"): string {
    if (status === "running") return "done";
    if (status === "crashed") return target === "check" ? "done" : "fail";
    if (status === "idle") return target === "check" ? "active" : "";
    // starting / stopping: runtime check done, dsh web step active, readiness pending
    if (target === "check") return "done";
    if (target === "start") return "active";
    return "";
  }

  let booting = $derived(status === "idle" || status === "starting" || status === "stopping");
</script>

<div class="app">
  <header>
    <div class="logo">
      <svg width="26" height="26" viewBox="0 0 64 64" fill="none">
        <circle cx="32" cy="32" r="24" stroke="#4d6bfe" stroke-width="7" />
        <circle cx="32" cy="32" r="8" fill="#4d6bfe" />
        <circle cx="49" cy="15" r="4" fill="#e8ebf2" />
      </svg>
    </div>
    <div class="title-wrap">
      <h1>DeepSeek Harness</h1>
      <span class="subtitle">桌面发行层 · 原版 Harness Web UI · 内置 Node Runtime</span>
    </div>
    <div class="spacer"></div>
    <span class="badge">v{versions.desktop}</span>
  </header>

  {#if !inTauri}
    <div class="warn-banner">
      当前以浏览器模式运行（未嵌入 Tauri，IPC 不可用）。请使用
      <b>pnpm tauri dev</b> 获得完整桌面体验。
    </div>
  {/if}

  <div class="card">
    <div class="status-row">
      {#if booting}
        <div class="spinner"></div>
      {:else}
        <div class="dot {status}"></div>
      {/if}
      <span class="status-text">{STATUS_TEXT[status]}</span>
      {#if pid !== null && (status === "running" || status === "starting")}
        <span class="badge">pid {pid}</span>
      {/if}
    </div>

    {#if status === "crashed" && lastError}
      <div class="error-box">{lastError}</div>
    {/if}

    {#if status === "starting" && lastError}
      <div class="notice-box">{lastError}</div>
    {/if}

    <!-- Stopped + lastError: e.g. "shutdown" requested while the sidecar was
         already down — the real status stays Stopped, only the notice shows. -->
    {#if status === "stopped" && lastError}
      <div class="notice-box">{lastError}</div>
    {/if}

    {#if status === "running" && url}
      <div class="url-box">{url}</div>
    {/if}

    {#if booting}
      <div class="steps">
        <div class={stepClass("check")}>
          <span class="ico">{stepClass("check") === "fail" ? "✗" : stepClass("check") === "done" ? "✓" : "●"}</span>
          Runtime 检查 — Node {versions.node} · Harness {versions.harness}
        </div>
        <div class={stepClass("start")}>
          <span class="ico">{stepClass("start") === "fail" ? "✗" : stepClass("start") === "done" ? "✓" : "●"}</span>
          启动 dsh web（127.0.0.1 · 自动端口）
        </div>
        <div class={stepClass("ready")}>
          <span class="ico">{stepClass("ready") === "fail" ? "✗" : stepClass("ready") === "done" ? "✓" : "●"}</span>
          等待 readiness 信号
        </div>
      </div>
    {/if}

    <div class="actions">
      {#if status === "running"}
        <button class="primary" onclick={doOpen}>打开 Harness 窗口</button>
        <button class="ghost" onclick={doRestart} disabled={busy}>重新启动</button>
        <button class="danger-ghost" onclick={doShutdown} disabled={busy}>停止</button>
      {:else if status === "crashed" || status === "stopped"}
        <button class="primary" onclick={doRestart} disabled={busy}>
          {status === "stopped" ? "重新启动" : "重新启动 Harness"}
        </button>
        {#if status === "crashed"}
          <button class="ghost" onclick={copyDiagnostics}>复制诊断信息</button>
          <button class="ghost" onclick={doExportDiagnostics}>导出诊断</button>
          <button class="danger-ghost" onclick={doQuitApp}>退出应用</button>
        {/if}
      {:else}
        <button class="ghost" disabled>正在启动…</button>
      {/if}
    </div>
  </div>

  <button class="logs-toggle" onclick={() => (logsOpen = !logsOpen)}>
    <span>{logsOpen ? "▾" : "▸"}</span>
    运行日志 {logsOpen ? "" : `(${logs.length})`}
  </button>

  {#if logsOpen}
    <div class="console">
      {#if logs.length === 0}
        <span class="l-empty">（暂无日志）</span>
      {:else}
        {#each logs as [stream, line], i (i)}
          <div class="l-{stream}">{line}</div>
        {/each}
      {/if}
    </div>
  {/if}

  <div class="card update-card">
    <div class="update-row">
      <span class="update-title">软件更新</span>
      {#if updateInfo?.available}
        <span class="update-info">发现新版本 v{updateInfo.version}</span>
        <button class="primary" onclick={doInstallUpdate} disabled={updateBusy}>
          {updateBusy
            ? updatePercent !== null
              ? `更新中 ${updatePercent}%…`
              : "更新中…"
            : "安装更新并重启"}
        </button>
      {:else}
        <button class="ghost" onclick={doCheckUpdate} disabled={updateBusy}>
          {updateBusy ? "检查中…" : "检查更新"}
        </button>
      {/if}
    </div>
    <div class="update-row">
      <span class="update-title">资源</span>
      <button class="ghost" onclick={() => openSite("https://dsharness.app")}>官网</button>
      <button class="ghost" onclick={() => openSite("https://cordis.run")}>插件市场</button>
      <button class="ghost" onclick={reportIssue}>报告问题</button>
    </div>
    {#if updateError}
      <div class="notice-box">{updateError}</div>
    {/if}
  </div>

  <div class="card preset-card">
    <div class="update-row">
      <span class="update-title">预设（Agent Presets）</span>
      <button class="ghost" onclick={doPreviewPreset} disabled={presetBusy}>导入 .dshpreset…</button>
    </div>
    {#if presetPreview}
      <div class="notice-box">
        <b>预设 {presetPreview.id}</b> · {presetPreview.files.length} 个文件
        {#if presetPreview.warnings.includes("possible-secrets")}
          · <span class="warn">⚠ 检测到疑似密钥</span>
        {/if}
        {#if presetPreview.warnings.includes("absolute-paths")}
          · <span class="warn">⚠ 含绝对路径</span>
        {/if}
        <div>预设与 Agent 同权限运行工具和命令——仅导入可信来源。</div>
        <button class="primary" onclick={doImportPreset} disabled={presetBusy}>确认导入</button>
        <button class="ghost" onclick={() => (presetPreview = null)}>取消</button>
      </div>
    {/if}
    {#if presetError}
      <div class="notice-box">{presetError}</div>
    {/if}
    {#if userPresets.length > 0}
      {#each userPresets as id}
        <div class="preset-row">
          <span>{id}</span>
          <button class="ghost" onclick={() => doExportPreset(id)} disabled={presetBusy}>导出</button>
        </div>
      {/each}
    {:else}
      <div class="preset-row"><span class="l-empty">（暂无用户预设）</span></div>
    {/if}
  </div>

  <footer>
    <div class="versions">
      <span class="version-pill">desktop {versions.desktop}</span>
      <span class="version-pill">harness {versions.harness}</span>
      <span class="version-pill">node {versions.node}</span>
      <span class="version-pill">sidecar {versions.sidecar}</span>
    </div>
    <div class="note">
      所有 Harness 功能（模型 / 工具 / 技能 / MCP / 沙箱）均由原版 Harness UI 提供；桌面层仅负责生命周期。
    </div>
  </footer>
</div>

{#if toast}
  <div class="toast">{toast}</div>
{/if}
