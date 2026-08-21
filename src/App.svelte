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
    cancelDiagnosticsExport,
    quitApp,
    listUserPresets,
    previewPreset,
    importPreset,
    cancelPresetPreview,
    exportPreset,
    deletePreset,
    listPlugins,
    installPlugin,
    uninstallPlugin,
    cancelPluginOp,
    getPluginRecovery,
    beginPluginRecovery,
    rollbackPluginRecovery,
    finalizePluginRecovery,
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
    marketSearch,
    marketPlugin,
    marketPrepareInstall,
    marketInstallPlugin,
    activateMarketPlugin,
    marketImage,
    sideloadPlugin,
    pickSideloadFile,
    type MarketPluginSummary,
    type MarketPluginDetail,
    type MarketDescription,
    type MarketInstallPreview,
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
    type PluginRecoveryCandidate,
    type PluginRecoveryOverview,
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
    distribution: "web",
  });
  let storeBuild = $state(false);
  let logs = $state<[string, string][]>([]);
  let inTauri = $state(true);
  let busy = $state(false);
  let diagnosticsBusy = $state(false);
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
  let pluginRestartNotice = $state<string | null>(null);
  let pluginInstallRequest = $state<PluginInstallRequest | null>(null);
  let remotePresetRequest = $state<RemotePresetRequest | null>(null);
  let remotePresetPreview = $state<RemotePresetPreview | null>(null);
  let remotePresetDownloading = $state(false);
  let marketQuery = $state("");
  let marketCategory = $state("");
  let marketItems = $state<MarketPluginSummary[]>([]);
  let marketCount = $state<number | null>(null);
  let marketNextCursor = $state<string | null>(null);
  let marketHasMore = $state(false);
  let marketOffline = $state(false);
  let marketOfflineFetchedAt = $state<number | null>(null);
  let marketBusy = $state(false);
  let marketError = $state<string | null>(null);
  let marketPreparing = $state(false);
  let marketConfirm = $state<MarketInstallPreview | null>(null);
  let marketDetail = $state<MarketPluginDetail | null>(null);
  let marketDetailBusy = $state(false);
  let marketImages = $state<string[]>([]);
  let sideloadPath = $state<string | null>(null);
  let recoveryOverview = $state<PluginRecoveryOverview | null>(null);
  let recoveryBusy = $state(false);
  let recoveryError = $state<string | null>(null);
  let recoveryConfirm = $state<{
    action: "disable" | "rollback" | "finalize";
    candidate?: PluginRecoveryCandidate;
  } | null>(null);

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
    if (p.status === "starting" && pluginRestartNotice) {
      pluginRestartNotice = null;
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
        pluginInstallRequest.source !== request.source ||
        pluginInstallRequest.slug !== request.slug
      ) {
        showToast("已有待确认的安装请求，新请求已忽略");
      }
      return;
    }
    if (marketConfirm || marketPreparing) {
      showToast("已有市场安装确认正在进行，插件安装请求已忽略");
      void dismissPendingPluginInstall().catch(() => {});
      return;
    }
    if (remotePresetRequest || presetPreview) {
      showToast("已有待处理的预设请求，插件安装请求已忽略");
      // A local file-preview does not own the Rust install arbiter. A plugin
      // deep link can therefore arrive while it is open; rejecting only in
      // the UI would leave the pending slot + arbiter held until a reload.
      void dismissPendingPluginInstall().catch(() => {});
      return;
    }
    pluginInstallRequest = request;
  }

  function presentRemotePresetRequest(request: RemotePresetRequest) {
    if (pluginInstallRequest || presetPreview || marketConfirm || marketPreparing) {
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
    remotePresetDownloading = request.stage === "downloading";
    if (
      request.stage === "awaiting-install" &&
      request.id &&
      request.files &&
      request.warnings
    ) {
      remotePresetPreview = {
        requestId: request.requestId,
        id: request.id,
        files: request.files,
        warnings: request.warnings,
      };
    }
  }

  function marketVersion(item: MarketPluginSummary | MarketPluginDetail): string | null {
    return item.source?.version ?? null;
  }

  function marketPackageName(item: MarketPluginSummary | MarketPluginDetail): string {
    return item.source?.packageName?.trim() || "（未提供可安装 npm 来源）";
  }

  function marketDescriptionText(desc: MarketDescription | null | undefined): string {
    if (!desc) return "";
    if (typeof desc === "string") return desc;
    return desc.zh ?? desc.en ?? "";
  }

  function pluginStateText(state: PluginEntry["state"]): string {
    if (state === "pending") return "待激活";
    if (state === "active") return "已激活";
    return "已安装";
  }

  async function doMarketSearch(reset = true) {
    if (marketBusy) return;
    if (!reset && (!marketHasMore || marketNextCursor === null)) return;
    marketBusy = true;
    marketError = null;
    try {
      const res = await marketSearch(
        marketQuery.trim(),
        marketCategory || undefined,
        30,
        reset ? undefined : marketNextCursor ?? undefined,
        "desktop",
      );
      const received = res.items ?? [];
      marketOffline = res.cache?.status === "offline";
      marketOfflineFetchedAt = marketOffline ? res.cache?.fetchedAtMs ?? null : null;
      if (reset) {
        marketItems = received;
      } else {
        const known = new Set(marketItems.map((item) => item.slug));
        marketItems = [...marketItems, ...received.filter((item) => !known.has(item.slug))];
      }
      // count and hasMore are catalog facts. Cursor values are opaque and are
      // stored exactly as returned rather than interpreted as a page number.
      marketCount = res.count;
      marketNextCursor = res.page?.cursor ?? null;
      marketHasMore = res.page?.hasMore === true;
    } catch (e) {
      marketError = "市场搜索失败：" + e;
    }
    marketBusy = false;
  }

  async function doMarketDetail(slug: string) {
    if (marketDetailBusy || marketOffline) return;
    marketDetailBusy = true;
    marketImages = [];
    try {
      const detail = await marketPlugin(slug);
      marketDetail = detail;
      const shots = (detail.screenshots ?? []).slice(0, 4);
      const loaded = await Promise.all(
        shots.map(async (url) => {
          try {
            return (await marketImage(url)).dataUrl;
          } catch {
            return null;
          }
        }),
      );
      marketImages = loaded.filter((src): src is string => src !== null);
    } catch (e) {
      marketError = `插件详情加载失败：${e}`;
    }
    marketDetailBusy = false;
  }

  async function prepareMarketInstall(slug: string) {
    if (pluginBusy || marketPreparing || marketOffline || recoveryOverview?.transaction) return;
    marketPreparing = true;
    marketError = null;
    try {
      // This refetches the detail with ETag revalidation. The modal then
      // presents its current entryRevision for an explicit user confirmation.
      marketConfirm = await marketPrepareInstall(slug);
    } catch (e) {
      marketError = "无法准备安装：" + e;
    }
    marketPreparing = false;
  }

  async function doMarketInstall(item: MarketPluginSummary) {
    if (marketOffline || item.installable !== true) return;
    await prepareMarketInstall(item.slug);
  }

  function confirmMarketInstall() {
    const preview = marketConfirm;
    if (!preview || pluginBusy) return;
    marketConfirm = null;
    pluginBusy = true;
    pluginError = null;
    pluginLogs = [];
    pluginLogsOpen = true;
    void marketInstallPlugin(preview.slug, preview.entryRevision).catch((e) => {
      pluginBusy = false;
      pluginError = "市场安装失败：" + e;
      pluginLogsOpen = true;
      void refreshPlugins();
    });
    showToast("正在校验并安装 " + preview.packageName + "；完成后仍需显式激活");
  }

  async function doActivateMarketPlugin(plugin: PluginEntry) {
    if (
      pluginBusy ||
      recoveryOverview?.transaction ||
      plugin.state !== "pending" ||
      !plugin.slug ||
      !plugin.entryRevision
    ) {
      return;
    }
    pluginBusy = true;
    pluginError = null;
    try {
      await activateMarketPlugin(plugin.slug, plugin.entryRevision);
      pluginRestartNotice = `插件 ${plugin.name} 已激活，需要重启 Harness 后才能生效。`;
      showToast("已激活 " + plugin.name + "；请重启 Harness 后加载");
      await refreshPlugins();
    } catch (e) {
      pluginError = "激活失败：" + e;
    }
    pluginBusy = false;
  }

  async function doPickSideload() {
    try {
      const path = await pickSideloadFile();
      if (path) sideloadPath = path;
    } catch (e) {
      showToast(`选择文件失败：${e}`);
    }
  }

  async function confirmSideloadInstall() {
    if (!sideloadPath || pluginBusy || recoveryOverview?.transaction) return;
    const path = sideloadPath;
    sideloadPath = null;
    pluginBusy = true;
    pluginError = null;
    pluginLogs = [];
    pluginLogsOpen = true;
    showToast(`正在离线安装 ${path}…`);
    try {
      await sideloadPlugin(path);
    } catch (e) {
      pluginBusy = false;
      pluginError = `离线安装失败：${e}`;
      pluginLogsOpen = true;
      void refreshPlugins();
    }
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
    diagnosticsBusy = true;
    try {
      const saved = await exportDiagnostics();
      if (saved) showToast("诊断信息已导出");
    } catch (e) {
      if (String(e).includes("cancelled")) showToast("已取消诊断导出");
      else showToast(`导出失败：${e}`);
    } finally {
      diagnosticsBusy = false;
    }
  }

  async function doCancelDiagnosticsExport() {
    try {
      if (await cancelDiagnosticsExport()) showToast("正在取消诊断导出…");
    } catch (e) {
      showToast(`取消失败：${e}`);
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

  async function reportContent() {
    try {
      const url = new URL("https://github.com/web-casa/DeepSeek-Harness-Desktop/issues/new");
      url.searchParams.set("title", "[Content] 报告 AI 生成内容");
      url.searchParams.set(
        "body",
        [
          "请说明需要报告的 AI 生成内容、使用的模型服务以及期望的处理方式。",
          "",
          "请勿粘贴 API key、个人数据、私密文件内容或其他敏感信息。",
        ].join("\n"),
      );
      await openUrl(url.toString());
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

  async function doCancelPresetPreview() {
    presetPreview = null;
    try {
      await cancelPresetPreview();
    } catch {
      /* best effort */
    }
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

  async function refreshRecovery() {
    try {
      recoveryOverview = await getPluginRecovery();
      recoveryError = null;
    } catch (e) {
      recoveryError = `插件恢复状态不可用：${e}`;
    }
  }

  async function confirmRecoveryAction() {
    const confirmation = recoveryConfirm;
    if (!confirmation || recoveryBusy) return;
    const transaction = recoveryOverview?.transaction;
    recoveryConfirm = null;
    recoveryBusy = true;
    recoveryError = null;
    try {
      if (confirmation.action === "disable" && confirmation.candidate) {
        await beginPluginRecovery(confirmation.candidate.packageName);
        showToast(`已精确禁用 ${confirmation.candidate.packageName}，正在验证重启`);
      } else if (confirmation.action === "rollback" && transaction) {
        await rollbackPluginRecovery(transaction.transactionId);
        showToast(`已回滚 ${transaction.packageName} 的隔离，正在重新启动`);
      } else if (confirmation.action === "finalize" && transaction) {
        await finalizePluginRecovery(transaction.transactionId);
        showToast(`已保留 ${transaction.packageName} 的隔离状态`);
      }
      await refreshRecovery();
      await refreshPlugins();
    } catch (e) {
      recoveryError = `插件恢复操作失败：${e}`;
      await refreshRecovery();
    }
    recoveryBusy = false;
  }

  function startPluginOp(name: string, op: "install" | "uninstall") {
    if (pluginBusy || recoveryOverview?.transaction || !name.trim()) return;
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

  async function confirmPluginInstallRequest() {
    const request = pluginInstallRequest;
    if (!request || pluginBusy || marketPreparing) return;
    pluginInstallRequest = null;
    try {
      await dismissPendingPluginInstall();
    } catch {
      // The request was already removed from the UI. The market command still
      // validates the Rust-derived slug and freshly revalidates the entry
      // before any profile mutation.
    }
    await prepareMarketInstall(request.slug);
  }

  // Initial data load (async onMount is fine here — no cleanup needed).
  onMount(async () => {
    try {
      const [st, ver, lg] = await Promise.all([getStatus(), getVersions(), getLogs()]);
      apply(st);
      versions = ver;
      storeBuild = ver.distribution === "store";
      logs = lg;
    } catch {
      inTauri = false;
    }
    refreshPresets();
    refreshPlugins();
    refreshRecovery();
    if (inTauri) void doMarketSearch(true);
    // Silent boot-time update check: only inform, never prompt.
    if (!storeBuild) {
      try {
        const info = await checkUpdate();
        if (info.available) updateInfo = info;
      } catch {
        /* offline / draft release: stay silent */
      }
    }
  });

  // Event subscription in a $effect: async onMount cannot return a cleanup
  // (Svelte ignores non-function returns), so the listener would never be
  // unbound. Effects handle async registration + cancellation properly.
  $effect(() => {
    if (!inTauri) return;
    let cancelled = false;
    let unlistenFn: (() => void) | null = null;
    void onEvent((p) => {
      apply(p);
      if (p.status === "crashed" || p.status === "running") void refreshRecovery();
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenFn = fn;
      })
      .catch(() => {
        /* listener registration can race webview startup/teardown */
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
    void onUpdateProgress((p) => {
      if (p.total && p.total > 0) {
        updatePercent = Math.min(100, Math.round((p.downloaded / p.total) * 100));
      }
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenFn = fn;
      })
      .catch(() => {
        /* listener registration can race webview teardown */
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
    void onPluginLog((lines) => {
      for (const l of lines) {
        pluginLogs.push(`[${l.stream}] ${l.line}`);
      }
      if (pluginLogs.length > 300) {
        pluginLogs.splice(0, pluginLogs.length - 300);
      }
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenFn = fn;
      })
      .catch(() => {
        /* listener registration can race webview teardown */
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
    void onPluginDone((p) => {
      pluginBusy = false;
      const tailLines = p.tail
        .split(/\r?\n/)
        .map((line) => line.trim())
        .filter(Boolean);
      // Early setup/spawn errors have no streamed plugin-log event. Preserve
      // plugin-done.tail so a failure can never render as "暂无日志".
      if (pluginLogs.length === 0 && tailLines.length > 0) {
        pluginLogs = tailLines.slice(-300);
      }
      if (p.exit === 0) {
        pluginName = "";
        if (p.op === "market-install") {
          showToast("插件已校验完成，正在等待你的显式激活");
        } else if (p.op === "remove") {
          pluginRestartNotice = "插件已卸载，需要重启 Harness 才能从当前进程中完全移除。";
          showToast("插件已卸载；请重启 Harness 完成移除");
        } else if (p.op === "add") {
          pluginRestartNotice = "插件已安装，需要重启 Harness 后才能生效。";
          showToast("插件已安装；请重启 Harness 后使用");
        } else {
          pluginError = "插件操作已完成，但返回了无法识别的操作类型；请刷新状态后重试。";
          pluginLogsOpen = true;
        }
      } else {
        const detail = tailLines.at(-1);
        pluginError = `插件操作失败（exit ${p.exit ?? "被终止"}）${detail ? `：${detail}` : "，详情见安装日志"}`;
        pluginLogsOpen = true;
      }
      void refreshPlugins();
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenFn = fn;
      })
      .catch(() => {
        /* listener registration can race webview teardown */
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
      try {
        const fn = await onPluginInstallRequest(presentPluginInstallRequest);
        if (cancelled) {
          fn();
          return;
        }
        unlistenFn = fn;
      } catch {
        /* still drain the durable Rust slot below */
      }
      // Drain any cold-start request after the live listener is armed when
      // possible. If listener registration failed, draining still prevents a
      // durable request + arbiter from becoming invisible until reload.
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
      try {
        const fn = await onPresetInstallRequest(presentRemotePresetRequest);
        if (cancelled) {
          fn();
          return;
        }
        unlistenFn = fn;
      } catch {
        /* still drain the durable Rust slot below */
      }
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
    let cancelled = false;
    let fetching = false;
    const refresh = async () => {
      if (fetching) return;
      fetching = true;
      try {
        const next = await getLogs();
        if (!cancelled) logs = next;
      } catch {
        /* a transient IPC failure must not produce an unhandled rejection */
      } finally {
        fetching = false;
      }
    };
    void refresh();
    const timer = setInterval(() => void refresh(), 1000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
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
      <h1>DSH Desktop</h1>
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
          <button class="ghost" onclick={doExportDiagnostics} disabled={diagnosticsBusy}>
            {diagnosticsBusy ? "正在导出…" : "导出诊断"}
          </button>
          {#if diagnosticsBusy}
            <button class="ghost" onclick={doCancelDiagnosticsExport}>取消导出</button>
          {/if}
          <button class="danger-ghost" onclick={doQuitApp}>退出应用</button>
        {/if}
      {:else}
        <button class="ghost" disabled>正在启动…</button>
      {/if}
    </div>
    {#if status === "crashed"}
      <p class="privacy-note">
        诊断包仅收集受限的 Desktop 生命周期证据和脱敏日志尾部，不包含会话或工作区文件；脱敏并非绝对，请在分享前检查内容。
      </p>
    {/if}
  </div>

  {#if recoveryError || recoveryOverview?.transaction || (recoveryOverview?.candidates.length ?? 0) > 0}
    <div class="card recovery-card">
      <div class="update-row">
        <span class="update-title">安全插件恢复</span>
        {#if recoveryBusy}<span class="plugin-busy"><span class="spinner"></span> 处理中…</span>{/if}
      </div>
      {#if recoveryError}
        <div class="notice-box">{recoveryError}</div>
      {/if}
      {#if recoveryOverview?.transaction}
        {@const transaction = recoveryOverview.transaction}
        <div class="preset-row">
          <span>
            <span class="preset-name">{transaction.packageName}</span>
            <span class="badge">{transaction.marketManaged ? "市场来源" : "用户来源"}</span>
            <span class="badge">
              {transaction.phase === "isolated" ? "已验证隔离" : "已禁用，等待健康启动"}
            </span>
          </span>
          <button
            class="ghost"
            onclick={() => (recoveryConfirm = { action: "rollback" })}
            disabled={recoveryBusy}
          >回滚并重新启用</button>
          <button
            class="primary"
            onclick={() => (recoveryConfirm = { action: "finalize" })}
            disabled={recoveryBusy || transaction.phase !== "isolated"}
          >保留隔离</button>
        </div>
        <div class="trust-note">
          回滚仅在 profile 仍与备份事务精确一致时执行；市场插件还必须通过当前线上 blocked、deprecated、兼容性和来源校验。
        </div>
      {:else if recoveryOverview?.terminalStartupFailure}
        <div class="trust-note">
          Desktop 从启动错误中识别到以下候选；日志只提供线索，Rust 已再次确认包同时存在于 dependencies 与激活 bundles。不会自动修改。
        </div>
        {#each recoveryOverview.candidates as candidate (candidate.packageName)}
          <div class="preset-row">
            <span>
              <span class="preset-name">{candidate.packageName}</span>
              <span class="badge">{candidate.versionSpec}</span>
              <span class="badge">{candidate.signals.join(" · ")}</span>
            </span>
            <button
              class="danger-ghost"
              onclick={() => (recoveryConfirm = { action: "disable", candidate })}
              disabled={recoveryBusy || pluginBusy}
            >审阅并隔离</button>
          </div>
        {/each}
      {/if}
    </div>
  {/if}

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
      {#if storeBuild}
        <span class="update-info">Microsoft Store 管理更新</span>
      {:else if updateInfo?.available}
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
      <button class="ghost" onclick={reportContent}>报告 AI 内容</button>
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
        <button class="ghost" onclick={doCancelPresetPreview}>取消</button>
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
      <span class="update-title">插件市场</span>
      {#if !storeBuild}
        <button class="ghost" onclick={doPickSideload} disabled={recoveryOverview?.transaction != null}>离线安装 .tgz</button>
      {/if}
    </div>
    <div class="plugin-row">
      <input
        class="plugin-input"
        type="text"
        placeholder="搜索 cordis.run 插件…"
        bind:value={marketQuery}
        disabled={marketBusy}
        spellcheck="false"
        onkeydown={(e) => {
          if (e.key === "Enter") doMarketSearch();
        }}
      />
      <select class="plugin-input" bind:value={marketCategory} disabled={marketBusy} aria-label="插件类别">
        <option value="">全部类别</option>
        <option value="agent">Agent</option>
        <option value="market">市场</option>
      </select>
      <button class="primary" onclick={() => doMarketSearch(true)} disabled={marketBusy}>
        {marketBusy ? "搜索中…" : "搜索"}
      </button>
    </div>
    {#if marketError}
      <div class="notice-box">{marketError}</div>
    {/if}
    {#if marketOffline}
      <div class="notice-box">
        当前显示离线缓存{marketOfflineFetchedAt ? `（缓存时间 ${new Date(marketOfflineFetchedAt).toLocaleString()}）` : ""}。离线条目仅供浏览；详情、翻页和安装均已禁用，请联网后重新搜索。
      </div>
    {/if}
    {#if marketItems.length > 0}
      {#if marketCount !== null}
        <div class="preset-issues">当前筛选共 {marketCount} 个条目；按 cursor 顺序加载。</div>
      {/if}
      {#each marketItems as item (item.slug)}
        <div class="preset-row">
          <span class="preset-id">
            <span class="preset-name">{item.name}</span>
            {#if marketVersion(item)}<span class="badge">v{marketVersion(item)}</span>{/if}
            {#if item.stars != null}<span class="badge">★ {item.stars}</span>{/if}
            {#if item.category}<span class="badge">{item.category}</span>{/if}
            <span class="badge">{item.platforms.includes("desktop") ? "desktop" : "web-only"}</span>
          </span>
          <button class="ghost" onclick={() => doMarketDetail(item.slug)} disabled={marketDetailBusy || marketOffline}>详情</button>
          {#if item.installable === true}
            <button
              class="primary"
              onclick={() => doMarketInstall(item)}
              disabled={pluginBusy || marketPreparing || marketOffline || recoveryOverview?.transaction != null}
            >{marketPreparing ? "校验中…" : "安装"}</button>
          {:else}
            <button class="ghost" disabled title={item.installReason ?? "该条目不可安装"}>
              {item.blocked ? "已封禁" : item.deprecated ? "已弃用" : "不可安装"}
            </button>
          {/if}
        </div>
        {#if marketDescriptionText(item.description)}
          <div class="preset-issues">{marketDescriptionText(item.description)}</div>
        {/if}
      {/each}
      {#if marketHasMore}
        <div class="plugin-row">
          <button
            class="ghost"
            onclick={() => doMarketSearch(false)}
            disabled={marketBusy || marketNextCursor === null}
          >
            {marketBusy ? "加载中…" : "加载更多"}
          </button>
        </div>
      {/if}
    {:else}
      <div class="preset-row"><span class="l-empty">（输入关键词搜索插件市场）</span></div>
    {/if}
  </div>

  <div class="card plugin-card">
    <div class="update-row">
      <span class="update-title">插件（用户安装）</span>
    </div>
    {#if storeBuild}
      <div class="plugin-row">
        <span class="update-info">仅允许安装 cordis.run 已审核插件</span>
        <button class="ghost" onclick={() => openSite("https://cordis.run")}>打开插件市场</button>
      </div>
      <div class="trust-note">
        插件与 Agent 同权限运行工具和命令；Store 版仅安装已审核插件。
      </div>
    {:else}
      <div class="plugin-row">
        <input
          class="plugin-input"
          type="text"
          placeholder="npm 包名，如 @cordisjs/plugin-example"
          bind:value={pluginName}
          disabled={pluginBusy || recoveryOverview?.transaction != null}
          spellcheck="false"
          onkeydown={(e) => {
            if (e.key === "Enter") startPluginOp(pluginName, "install");
          }}
        />
        {#if pluginBusy}
          <span class="plugin-busy"><span class="spinner"></span> 操作中…</span>
          <button class="danger-ghost" onclick={doCancelPluginOp}>取消</button>
        {:else}
          <button class="primary" onclick={() => startPluginOp(pluginName, "install")} disabled={!pluginName.trim() || recoveryOverview?.transaction != null}>
            安装
          </button>
        {/if}
      </div>
      <div class="trust-note">
        插件与 Agent 同权限运行工具和命令——仅安装可信来源；可在
        <button class="inline-link" onclick={() => openSite("https://cordis.run")}>cordis.run 插件市场</button>
        查找包名。
      </div>
    {/if}
    {#if pluginError}
      <div class="notice-box">{pluginError}</div>
    {/if}
    {#if pluginRestartNotice}
      <div class="notice-box">
        {pluginRestartNotice}
        <button class="primary" onclick={doRestart} disabled={busy || status !== "running"}>
          立即重启 Harness
        </button>
      </div>
    {/if}
    {#if plugins.length > 0}
      {#each plugins as p (p.name)}
        <div class="preset-row">
          <span>
            {p.name} <span class="badge">v{p.version}</span>
            <span class="badge">{pluginStateText(p.state)}</span>
          </span>
          {#if p.state === "pending" && p.slug && p.entryRevision}
            <button class="primary" onclick={() => doActivateMarketPlugin(p)} disabled={pluginBusy || recoveryOverview?.transaction != null}>
              Activate
            </button>
          {/if}
          <button class="ghost" onclick={() => startPluginOp(p.name, "uninstall")} disabled={pluginBusy || recoveryOverview?.transaction != null}>卸载</button>
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

{#if recoveryConfirm}
  <div class="modal-backdrop">
    <div class="modal" role="dialog" aria-modal="true" aria-label="安全插件恢复确认">
      <div class="modal-title">
        {recoveryConfirm.action === "disable"
          ? "确认隔离插件？"
          : recoveryConfirm.action === "rollback"
            ? "确认回滚隔离？"
            : "确认保留隔离？"}
      </div>
      <div class="modal-name">
        {recoveryConfirm.candidate?.packageName ?? recoveryOverview?.transaction?.packageName ?? "未知插件"}
      </div>
      <div class="modal-warn">
        {#if recoveryConfirm.action === "disable"}
          将先私有备份 package.json，再仅从 dsh.profile.bundles 精确移除此包并重启验证；不会卸载依赖或删除插件文件。失败时保留可审计事务与回滚入口。
        {:else if recoveryConfirm.action === "rollback"}
          将仅在当前 profile 与隔离结果逐字节一致时恢复备份并重启。市场插件必须在线重新通过安装门禁；回滚可能重新引入启动故障。
        {:else}
          当前插件将保持禁用，Desktop 会删除本次恢复备份。依赖和插件文件仍保留，可由你后续手动管理。
        {/if}
      </div>
      <div class="modal-actions">
        <button
          class={recoveryConfirm.action === "finalize" ? "primary" : "danger-ghost"}
          onclick={confirmRecoveryAction}
          disabled={recoveryBusy}
        >确认执行</button>
        <button class="ghost" onclick={() => (recoveryConfirm = null)} disabled={recoveryBusy}>取消</button>
      </div>
    </div>
  </div>
{/if}

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
      {#if remotePresetRequest.stage === "installing"}
        <div class="modal-warn">正在安装预设…</div>
      {:else if remotePresetPreview && remotePresetPreview.requestId === remotePresetRequest.requestId}
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

{#if marketDetail}
  <div class="modal-backdrop">
    <div class="modal" role="dialog" aria-modal="true" aria-label="插件详情">
      <div class="modal-title">{marketDetail.name}</div>
      <div class="modal-name">{marketPackageName(marketDetail)}</div>
      <div class="modal-meta">{marketDescriptionText(marketDetail.description)}</div>
      {#if marketImages.length > 0}
        <div class="market-shots">
          {#each marketImages as src, i (i)}
            <img
              src={src}
              alt="{marketDetail.name} screenshot {i + 1}"
              class="market-shot"
              referrerpolicy="no-referrer"
            />
          {/each}
        </div>
      {/if}
      <div class="modal-actions">
        <button class="ghost" onclick={() => (marketDetail = null)}>关闭</button>
      </div>
    </div>
  </div>
{/if}

{#if marketConfirm}
  <div class="modal-backdrop">
    <div class="modal" role="dialog" aria-modal="true" aria-label="安装 Cordis 插件确认">
      <div class="modal-title">安装 Cordis 插件？</div>
      <div class="modal-name">{marketConfirm.packageName} v{marketConfirm.version}</div>
      <div class="modal-meta">
        当前条目修订：{marketConfirm.entryRevision}
      </div>
      <div class="modal-meta">
        来源：<button class="inline-link" onclick={() => openSite("https://cordis.run/plugins/" + marketConfirm!.slug)}>
          https://cordis.run/plugins/{marketConfirm.slug}
        </button>
      </div>
      <div class="modal-warn">
        将校验此修订的 tarball 与 lockfile integrity，禁用构建脚本，并保持为待激活状态。安装完成后不会自动启用，需你再次点击 Activate。
      </div>
      <div class="modal-actions">
        <button class="primary" onclick={confirmMarketInstall} disabled={pluginBusy}>确认安装</button>
        <button class="ghost" onclick={() => (marketConfirm = null)}>取消</button>
      </div>
    </div>
  </div>
{/if}

{#if sideloadPath && !storeBuild}
  <div class="modal-backdrop">
    <div class="modal" role="dialog" aria-modal="true" aria-label="离线插件安装确认">
      <div class="modal-title">离线安装插件？</div>
      <div class="modal-name">{sideloadPath}</div>
      <div class="modal-warn">插件与 Agent 同权限运行工具和命令，仅安装可信插件。确认后将通过 `dsh plugin add` 安装该 .tgz。</div>
      <div class="modal-actions">
        <button class="primary" onclick={confirmSideloadInstall} disabled={pluginBusy}>确认安装</button>
        <button class="ghost" onclick={() => (sideloadPath = null)}>取消</button>
      </div>
    </div>
  </div>
{/if}

{#if pluginInstallRequest}
  <div class="modal-backdrop">
    <div class="modal" role="dialog" aria-modal="true" aria-label="安装 Cordis 插件确认">
      <div class="modal-title">安装 Cordis 插件？</div>
      <div class="modal-name">链接声明的包：{pluginInstallRequest.name}</div>
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
        该链接可由任意网页或程序构造。下一步只会使用 Rust 已校验的 Cordis slug 重新拉取市场详情，并显示当前 entryRevision 供你再次确认；不会按链接中的包名直接安装。插件与 Agent 同权限运行工具和命令，仅安装可信插件。
      </div>
      {#if pluginBusy}
        <div class="notice-box">当前已有插件操作正在进行，请等待完成后再确认安装。</div>
      {/if}
      <div class="modal-actions">
        <button class="primary" onclick={confirmPluginInstallRequest} disabled={pluginBusy}>
          {pluginBusy ? "已有操作进行中…" : "继续核验"}
        </button>
        <button class="ghost" onclick={dismissPluginInstallRequest}>取消</button>
      </div>
    </div>
  </div>
{/if}

{#if toast}
  <div class="toast">{toast}</div>
{/if}
