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
    deletePreset,
    listPlugins,
    installPlugin,
    uninstallPlugin,
    cancelPluginOp,
    getPendingPluginInstall,
    dismissPendingPluginInstall,
    onEvent,
    onUpdateProgress,
    onPluginLog,
    onPluginDone,
    onPluginInstallRequest,
    getPendingRemotePreset,
    dismissRemotePreset,
    confirmRemotePresetDownload,
    importRemotePreset,
    onPresetInstallRequest,
    type RemotePresetRequest,
    type RemotePresetPreview,
    type Status,
    type StatusPayload,
    type PresetPreview,
    type PresetIssueKind,
    type PresetRow,
    type UpdateInfo,
    type Versions,
    type PluginEntry,
    type PluginInstallRequest,
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
  let userPresets = $state<PresetRow[]>([]);
  let presetPreview = $state<PresetPreview | null>(null);
  let presetError = $state<string | null>(null);
  let presetBusy = $state(false);
  let confirmDelete = $state<string | null>(null);
  let plugins = $state<PluginEntry[]>([]);
  let pluginName = $state("");
  let pluginBusy = $state(false);
  let pluginLogs = $state<string[]>([]);
  let pluginLogsOpen = $state(false);
  let pluginError = $state<string | null>(null);
  let pluginInstallRequest = $state<PluginInstallRequest | null>(null);
  let remotePresetRequest = $state<RemotePresetRequest | null>(null);
  let remotePresetPreview = $state<RemotePresetPreview | null>(null);
  let remotePresetDownloading = $state(false);

  const STATUS_TEXT: Record<Status, string> = {
    idle: "等待启动",
    starting: "正在启动 Harness…",
    running: "Harness 运行中",
    stopping: "正在停止 Harness…",
    stopped: "Harness 已停止",
    crashed: "Harness 进程异常退出",
  };

  const ISSUE_LABEL: Record<PresetIssueKind, string> = {
    broken: "已损坏",
    unsafe: "不安全",
    info: "缺元数据",
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

  function presentPluginInstallRequest(request: PluginInstallRequest) {
    // The confirmation dialog is the security control: never let a second
    // deep link replace the package the user is currently reading. Same
    // request is idempotent; a different one is ignored with a visible note.
    if (pluginInstallRequest) {
      if (
        pluginInstallRequest.name !== request.name ||
        pluginInstallRequest.source !== request.source
      ) {
        showToast("已有待确认的安装请求，新请求已忽略");
      }
      return;
    }
    if (remotePresetRequest || presetPreview) {
      showToast("已有待处理的预设请求，插件安装请求已忽略");
      return;
    }
    pluginInstallRequest = request;
  }

  function presentRemotePresetRequest(request: RemotePresetRequest) {
    if (pluginInstallRequest || presetPreview) {
      showToast("已有待处理的安装请求，预设请求已忽略");
      void dismissRemotePreset(request.requestId).catch(() => {});
      return;
    }
    if (
      remotePresetRequest &&
      remotePresetRequest.requestId !== request.requestId
    ) {
      showToast("已有待处理的预设请求，新请求已忽略");
      void dismissRemotePreset(request.requestId).catch(() => {});
      return;
    }
    remotePresetRequest = request;
    remotePresetPreview = null;
  }

  async function doRemotePresetDownload() {
    const request = remotePresetRequest;
    if (!request || remotePresetPreview || remotePresetDownloading) return;
    remotePresetDownloading = true;
    try {
      remotePresetPreview = await confirmRemotePresetDownload(request.requestId);
    } catch (e) {
      showToast(`预设下载失败：${e}`);
      try {
        const pending = await getPendingRemotePreset();
        remotePresetRequest = pending;
      } catch {
        remotePresetRequest = null;
      }
    }
    remotePresetDownloading = false;
  }

  async function doRemotePresetDismiss() {
    const request = remotePresetRequest;
    if (!request || remotePresetDownloading) return;
    remotePresetRequest = null;
    remotePresetPreview = null;
    try {
      await dismissRemotePreset(request.requestId);
    } catch {
      /* best effort */
    }
  }

  async function doRemotePresetImport() {
    const request = remotePresetRequest;
    const preview = remotePresetPreview;
    if (!request || !preview || preview.requestId !== request.requestId) return;
    try {
      const id = await importRemotePreset(request.requestId);
      showToast(`预设 ${id} 已导入`);
      remotePresetRequest = null;
      remotePresetPreview = null;
      await refreshPresets();
    } catch (e) {
      showToast(`预设导入失败：${e}`);
    }
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

  async function doDeletePreset(id: string) {
    if (presetBusy) return;
    presetBusy = true;
    try {
      await deletePreset(id);
      confirmDelete = null;
      showToast(`预设 ${id} 已删除`);
      await refreshPresets();
    } catch (e) {
      showToast(`删除失败：${e}`);
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

  async function refreshPlugins() {
    try {
      const res = await listPlugins();
      plugins = res.plugins;
      // Backend busy is the truth across webview reloads: an op may still be
      // running after the UI restarted, and the single-flight flag is
      // app-wide — resync instead of showing a stale idle state.
      pluginBusy = res.busy;
    } catch {
      /* non-fatal */
    }
  }

  function startPluginOp(name: string, op: "install" | "uninstall") {
    if (pluginBusy || !name.trim()) return;
    pluginBusy = true;
    pluginError = null;
    pluginLogs = [];
    pluginLogsOpen = true;
    const label = op === "install" ? `正在安装 ${name.trim()}…` : `正在卸载 ${name.trim()}…`;
    const call = op === "install" ? installPlugin(name.trim()) : uninstallPlugin(name.trim());
    void call.catch((e) => {
      pluginBusy = false;
      pluginError = `${op === "install" ? "安装" : "卸载"}失败：${e}`;
      pluginLogsOpen = true;
      // Resync busy: "an operation is already running" means another
      // surface (or a pre-reload op) owns the backend flag.
      void refreshPlugins();
    });
    showToast(label);
  }

  async function doCancelPluginOp() {
    try {
      await cancelPluginOp();
      showToast("已请求取消");
    } catch (e) {
      showToast(`取消失败：${e}`);
    }
  }

  async function dismissPluginInstallRequest() {
    pluginInstallRequest = null;
    try {
      await dismissPendingPluginInstall();
    } catch {
      /* the slot is best-effort; the UI state is authoritative */
    }
  }

  function confirmPluginInstallRequest() {
    const request = pluginInstallRequest;
    if (!request || pluginBusy) return;
    pluginInstallRequest = null;
    void dismissPendingPluginInstall().catch(() => {});
    startPluginOp(request.name, "install");
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
    refreshPlugins();
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

  // Plugin op log stream: batch events arrive as arrays; cap the UI ring at
  // the same 300 lines the backend keeps in its tail.
  $effect(() => {
    if (!inTauri) return;
    let cancelled = false;
    let unlistenFn: (() => void) | null = null;
    onPluginLog((lines) => {
      for (const l of lines) {
        pluginLogs.push(`[${l.stream}] ${l.line}`);
      }
      if (pluginLogs.length > 300) {
        pluginLogs.splice(0, pluginLogs.length - 300);
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

  $effect(() => {
    if (!inTauri) return;
    let cancelled = false;
    let unlistenFn: (() => void) | null = null;
    onPluginDone((p) => {
      pluginBusy = false;
      if (p.exit === 0) {
        pluginName = "";
        showToast("插件操作完成");
      } else {
        pluginError = `插件操作失败（exit ${p.exit ?? "被终止"}），详情见安装日志`;
        pluginLogsOpen = true;
      }
      void refreshPlugins();
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenFn = fn;
    });
    return () => {
      cancelled = true;
      unlistenFn?.();
    };
  });

  // Deep-link requests: warm links arrive as events; the pending Rust slot
  // is drained only after the listener is armed so a URL delivered during
  // webview startup can never fall into the gap.
  $effect(() => {
    if (!inTauri) return;
    let cancelled = false;
    let unlistenFn: (() => void) | null = null;
    void (async () => {
      const fn = await onPluginInstallRequest(presentPluginInstallRequest);
      if (cancelled) {
        fn();
        return;
      }
      unlistenFn = fn;
      // Drain any cold-start request only AFTER the live listener is armed:
      // a URL delivered in the gap stays in the Rust slot and is picked up
      // here instead of being lost.
      try {
        const pending = await getPendingPluginInstall();
        if (!cancelled && pending) presentPluginInstallRequest(pending);
      } catch {
        /* non-fatal */
      }
    })();
    return () => {
      cancelled = true;
      unlistenFn?.();
    };
  });

  $effect(() => {
    if (!inTauri) return;
    let cancelled = false;
    let unlistenFn: (() => void) | null = null;
    void (async () => {
      const fn = await onPresetInstallRequest(presentRemotePresetRequest);
      if (cancelled) {
        fn();
        return;
      }
      unlistenFn = fn;
      try {
        const pending = await getPendingRemotePreset();
        if (!cancelled && pending) presentRemotePresetRequest(pending);
      } catch {
        /* non-fatal */
      }
    })();
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
      {#each userPresets as row (row.id)}
        <div class="preset-row">
          <span class="preset-id">
            <span class="preset-name">{row.id}</span>
            {#each row.issues as issue (issue.kind)}
              <span class="preset-badge {issue.kind}">{ISSUE_LABEL[issue.kind]}</span>
            {/each}
          </span>
          <button class="ghost" onclick={() => doExportPreset(row.id)} disabled={presetBusy}>导出</button>
          {#if confirmDelete === row.id}
            <button class="danger-ghost" onclick={() => doDeletePreset(row.id)} disabled={presetBusy}>确认删除？</button>
            <button class="ghost" onclick={() => (confirmDelete = null)}>取消</button>
          {:else}
            <button class="ghost" onclick={() => (confirmDelete = row.id)} disabled={presetBusy}>删除</button>
          {/if}
        </div>
        {#if confirmDelete === row.id}
          <div class="preset-issues">
            · 桌面层直接移除预设目录，不清理 Harness 的默认预设设置——若该预设是当前默认，
            请在 Harness 设置页改选默认，否则下次会话可能无法启动 Agent。
          </div>
        {/if}
        {#if row.issues.length > 0}
          <div class="preset-issues">
            {#each row.issues as issue (issue.kind)}
              <div>· {issue.detail}</div>
            {/each}
          </div>
        {/if}
      {/each}
    {:else}
      <div class="preset-row"><span class="l-empty">（暂无用户预设）</span></div>
    {/if}
  </div>

  <div class="card plugin-card">
    <div class="update-row">
      <span class="update-title">插件（用户安装）</span>
    </div>
    <div class="plugin-row">
      <input
        class="plugin-input"
        type="text"
        placeholder="npm 包名，如 @cordisjs/plugin-example"
        bind:value={pluginName}
        disabled={pluginBusy}
        spellcheck="false"
        onkeydown={(e) => {
          if (e.key === "Enter") startPluginOp(pluginName, "install");
        }}
      />
      {#if pluginBusy}
        <span class="plugin-busy"><span class="spinner"></span> 操作中…</span>
        <button class="danger-ghost" onclick={doCancelPluginOp}>取消</button>
      {:else}
        <button class="primary" onclick={() => startPluginOp(pluginName, "install")} disabled={!pluginName.trim()}>
          安装
        </button>
      {/if}
    </div>
    <div class="trust-note">
      插件与 Agent 同权限运行工具和命令——仅安装可信来源；可在
      <button class="inline-link" onclick={() => openSite("https://cordis.run")}>cordis.run 插件市场</button>
      查找包名。
    </div>
    {#if pluginError}
      <div class="notice-box">{pluginError}</div>
    {/if}
    {#if plugins.length > 0}
      {#each plugins as p (p.name)}
        <div class="preset-row">
          <span>{p.name} <span class="badge">v{p.version}</span></span>
          <button class="ghost" onclick={() => startPluginOp(p.name, "uninstall")} disabled={pluginBusy}>卸载</button>
        </div>
      {/each}
    {:else}
      <div class="preset-row"><span class="l-empty">（暂无用户安装的插件）</span></div>
    {/if}
    <button class="logs-toggle" onclick={() => (pluginLogsOpen = !pluginLogsOpen)}>
      <span>{pluginLogsOpen ? "▾" : "▸"}</span>
      安装日志 {pluginLogsOpen ? "" : `(${pluginLogs.length})`}
    </button>
    {#if pluginLogsOpen}
      <div class="console">
        {#if pluginLogs.length === 0}
          <span class="l-empty">（暂无日志）</span>
        {:else}
          {#each pluginLogs as line, i (i)}
            <div class="l-line">{line}</div>
          {/each}
        {/if}
      </div>
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

{#if remotePresetRequest}
  <div class="modal-backdrop">
    <div class="modal" role="dialog" aria-modal="true" aria-label="预设一键安装确认">
      <div class="modal-title">安装 Cordis 预设？</div>
      <div class="modal-meta">
        关联页面（未验证与预设内容的对应关系）：<button
          class="inline-link"
          title={remotePresetRequest!.source}
          onclick={() => openSite(remotePresetRequest!.source)}
        >
          {remotePresetRequest.source}
        </button>
      </div>
      {#if remotePresetPreview && remotePresetPreview.requestId === remotePresetRequest.requestId}
        <div class="modal-name">{remotePresetPreview.id}</div>
        <div class="modal-meta">
          {remotePresetPreview.files.length} 个文件
          {#if remotePresetPreview.warnings.includes("possible-secrets")}
            · <span class="warn">⚠ 检测到疑似密钥</span>
          {/if}
          {#if remotePresetPreview.warnings.includes("absolute-paths")}
            · <span class="warn">⚠ 含绝对路径</span>
          {/if}
        </div>
        <div class="modal-warn">
          预设与 Agent 同权限运行工具和命令——仅导入可信来源。确认后将安装到用户预设目录。
        </div>
        <div class="modal-actions">
          <button class="primary" onclick={doRemotePresetImport}>确认安装</button>
          <button class="ghost" onclick={doRemotePresetDismiss}>取消</button>
        </div>
      {:else}
        <div class="modal-warn">
          将先从 cordis.run 下载 .dshpreset 并做安全检查，确认内容后才会安装。
        </div>
        <div class="modal-actions">
          <button class="primary" onclick={doRemotePresetDownload} disabled={remotePresetDownloading}>
            {remotePresetDownloading ? "下载中…" : "下载并检查"}
          </button>
          <button class="ghost" onclick={doRemotePresetDismiss} disabled={remotePresetDownloading}>取消</button>
        </div>
      {/if}
    </div>
  </div>
{/if}

{#if pluginInstallRequest}
  <div class="modal-backdrop">
    <div class="modal" role="dialog" aria-modal="true" aria-label="安装 Cordis 插件确认">
      <div class="modal-title">安装 Cordis 插件？</div>
      <div class="modal-name">{pluginInstallRequest.name}</div>
      <div class="modal-meta">
        关联页面（未验证与插件包的对应关系）：<button
          class="inline-link"
          title={pluginInstallRequest!.source}
          onclick={() => openSite(pluginInstallRequest!.source)}
        >
          {pluginInstallRequest.source}
        </button>
      </div>
      <div class="modal-warn">
        该链接可由任意网页或程序构造，只能确认关联页面格式来自 cordis.run，无法证明插件包由该页面提供。插件与 Agent 同权限运行工具和命令，仅安装可信插件。确认后将立即开始安装，完成后需重新启动 Harness 生效。
      </div>
      {#if pluginBusy}
        <div class="notice-box">当前已有插件操作正在进行，请等待完成后再确认安装。</div>
      {/if}
      <div class="modal-actions">
        <button class="primary" onclick={confirmPluginInstallRequest} disabled={pluginBusy}>
          {pluginBusy ? "已有操作进行中…" : "确认安装"}
        </button>
        <button class="ghost" onclick={dismissPluginInstallRequest}>取消</button>
      </div>
    </div>
  </div>
{/if}

{#if toast}
  <div class="toast">{toast}</div>
{/if}
