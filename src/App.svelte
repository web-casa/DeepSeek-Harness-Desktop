<script lang="ts">
  import { onMount } from "svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import {
    getStatus,
    getLogs,
    getVersions,
    getDiagnostics,
    getDiagnosticMode,
    setDiagnosticMode,
    clearDiagnosticLogs,
    getPresentationLocale,
    restart,
    setPresentationLocale,
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
    previewProfilePatchCleanup,
    applyProfilePatchCleanup,
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
    type ProfileConsistencyReport,
    type ProfileCleanupPreview,
    type PluginInstallRequest,
    type PluginRecoveryCandidate,
    type PluginRecoveryOverview,
    type PresentationLocaleState,
    type DiagnosticModeState,
  } from "./lib/api";
  import {
    arbitratePluginRequest,
    arbitrateRemotePresetRequest,
    type InstallSurfaceSnapshot,
  } from "./lib/install-arbitration";
  import { trapDialog } from "./lib/dialog-trap";
  import { classifyMarketFailure } from "./lib/market-error";
  import { reconcilePluginCompletion } from "./lib/plugin-completion";
  import {
    browserLanguages,
    formatControllerDate,
    isLocalePreference,
    loadLocalePreference,
    nativePreferenceMigration,
    resolveControllerLocale,
    saveLocalePreference,
    translate,
    type LocalePreference,
    type ControllerLocale,
    type TranslationKey,
    type TranslationValues,
  } from "./lib/controller-i18n";

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
  let diagnosticMode = $state<DiagnosticModeState>({
    enabled: false,
    // The default-off policy is safe even before the native snapshot arrives.
    persisted: true,
    hasCapturedLogs: false,
  });
  let diagnosticModeBusy = $state(false);
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
  let profileConsistency = $state<ProfileConsistencyReport>({
    issues: [],
    cleanupEligibleCount: 0,
  });
  let profileCleanupPreview = $state<ProfileCleanupPreview | null>(null);
  let profileCleanupBusy = $state(false);
  let profileCleanupError = $state<string | null>(null);
  let pluginRefreshInFlight = false;
  let pluginRefreshQueued = false;
  let pluginDoneExpected = false;
  let pluginExpectedOp: "add" | "remove" | "market-install" | null = null;
  let pluginProfileTransitioning = $derived(
    status === "idle" || status === "starting" || status === "stopping",
  );
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

  let localePreference = $state<LocalePreference>(loadLocalePreference());
  let systemLanguages = $state<string[]>(browserLanguages());
  let nativeControllerLocale = $state<ControllerLocale | null>(null);
  let localeSaving = $state(false);
  let localeRequest = 0;
  let controllerLocale = $derived(
    nativeControllerLocale ?? resolveControllerLocale(localePreference, systemLanguages),
  );

  const STATUS_TEXT: Record<Status, TranslationKey> = {
    idle: "status.idle",
    starting: "status.starting",
    running: "status.running",
    stopping: "status.stopping",
    stopped: "status.stopped",
    crashed: "status.crashed",
  };

  const ISSUE_LABEL: Record<PresetIssueKind, TranslationKey> = {
    broken: "issue.broken",
    unsafe: "issue.unsafe",
    info: "issue.info",
  };

  const MARKET_FAILURE_KEYS = {
    timeout: "market.timeout",
    unavailable: "market.unavailable",
    invalidResponse: "market.invalidResponse",
    http: "market.http",
    api: "market.api",
    unknown: "market.requestFailed",
  } as const satisfies Record<ReturnType<typeof classifyMarketFailure>["kind"], TranslationKey>;

  function t(key: TranslationKey, values?: TranslationValues): string {
    return translate(controllerLocale, key, values);
  }

  function updateUnsupportedMessage(reason: UpdateInfo["unsupportedReason"]): string {
    switch (reason) {
      case "store":
        return t("update.unsupportedStore");
      case "msi":
        return t("update.unsupportedMsi");
      case "manual":
        return t("update.unsupportedManual");
      case "architecture":
        return t("update.unsupportedArchitecture");
      case "installer":
        return t("update.unsupportedInstaller");
      default:
        return t("toast.updatesUnsupported");
    }
  }

  function applyNativeLocale(snapshot: PresentationLocaleState, notifyPersistenceFailure = false) {
    localePreference = snapshot.preference;
    nativeControllerLocale = snapshot.locale;
    // Keep the v0.2.12 browser-only key in sync so a future browser-mode
    // session, or a downgrade, never silently loses the user's choice.
    saveLocalePreference(snapshot.preference);
    if (notifyPersistenceFailure && !snapshot.persisted) {
      showToast(t("locale.sessionOnly"));
    }
  }

  async function synchronizeNativeLocale() {
    const request = ++localeRequest;
    const legacyPreference = loadLocalePreference();
    try {
      let snapshot = await getPresentationLocale();
      if (request !== localeRequest) return;
      // Native state is authoritative once it exists. The only migration path
      // is a first-run native default plus a valid prior manual browser choice.
      const migration = nativePreferenceMigration(snapshot.persisted, legacyPreference);
      if (migration) {
        snapshot = await setPresentationLocale(migration, browserLanguages());
      }
      if (request !== localeRequest) return;
      applyNativeLocale(snapshot);
    } catch {
      // A partial/older desktop build must remain usable with the v0.2.12
      // localStorage fallback. Do not mark it as browser mode: core IPC works.
      if (request === localeRequest) nativeControllerLocale = null;
    }
  }

  async function setLocalePreference(event: Event) {
    const value = (event.currentTarget as HTMLSelectElement).value;
    const preference = isLocalePreference(value) ? value : "system";
    const languages = browserLanguages();
    systemLanguages = languages;
    if (!inTauri) {
      localePreference = preference;
      saveLocalePreference(preference);
      return;
    }

    const request = ++localeRequest;
    localeSaving = true;
    try {
      const snapshot = await setPresentationLocale(preference, languages);
      if (request === localeRequest) applyNativeLocale(snapshot, true);
    } catch {
      if (request === localeRequest) showToast(t("locale.updateFailed"));
    } finally {
      if (request === localeRequest) localeSaving = false;
    }
  }

  function marketFailureMessage(context: TranslationKey, error: unknown): string {
    const failure = classifyMarketFailure(error);
    const translatedContext = t(context);
    if (failure.kind === "api" && failure.detail) {
      return t("market.apiDetail", { context: translatedContext, detail: failure.detail });
    }
    return t("market.failure", {
      context: translatedContext,
      reason: t(MARKET_FAILURE_KEYS[failure.kind]),
    });
  }

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

  function installSurfaceSnapshot(): InstallSurfaceSnapshot {
    return {
      plugin: pluginInstallRequest,
      remotePresetRequestId: remotePresetRequest?.requestId ?? null,
      localPresetPreview: presetPreview !== null,
      marketConfirmation: marketConfirm !== null,
      marketPreparing,
    };
  }

  function presentPluginInstallRequest(request: PluginInstallRequest) {
    // The confirmation dialog is the security control: never let a second
    // deep link replace the package the user is currently reading. Keep this
    // decision in a pure, directly tested arbiter; this function only maps
    // the result to Rust-slot cleanup and UI state.
    const decision = arbitratePluginRequest(installSurfaceSnapshot(), request);
    if (decision.action === "keep-current") {
      if (decision.notify) showToast(t("toast.pluginRequestIgnored"));
      return;
    }
    if (decision.action === "dismiss-incoming") {
      showToast(
        decision.conflict === "market"
          ? t("toast.pluginRequestMarketConflict")
          : t("toast.pluginRequestPresetConflict"),
      );
      // A local file-preview does not own the Rust install arbiter. A plugin
      // deep link can therefore arrive while it is open; rejecting only in
      // the UI would leave the pending slot + arbiter held until a reload.
      void dismissPendingPluginInstall().catch(() => {});
      return;
    }
    pluginInstallRequest = request;
  }

  function presentRemotePresetRequest(request: RemotePresetRequest) {
    const decision = arbitrateRemotePresetRequest(
      installSurfaceSnapshot(),
      request.requestId,
    );
    if (decision.action === "dismiss-incoming") {
      showToast(
        decision.conflict === "preset"
          ? t("toast.presetRequestConflict")
          : t("toast.presetRequestPluginConflict"),
      );
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
    return item.source?.packageName?.trim() || t("market.noInstallSource");
  }

  function marketDescriptionText(desc: MarketDescription | null | undefined): string {
    if (!desc) return "";
    if (typeof desc === "string") return desc;
    return controllerLocale === "zh-CN" ? desc.zh ?? desc.en ?? "" : desc.en ?? desc.zh ?? "";
  }

  function pluginStateText(state: PluginEntry["state"]): string {
    if (state === "pending") return t("plugin.pending");
    if (state === "active") return t("plugin.active");
    return t("plugin.installed");
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
      marketError = marketFailureMessage("market.searchFailed", e);
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
      marketError = marketFailureMessage("market.detailFailed", e);
    }
    marketDetailBusy = false;
  }

  async function prepareMarketInstall(slug: string) {
    if (
      pluginBusy ||
      pluginProfileTransitioning ||
      marketPreparing ||
      marketOffline ||
      recoveryOverview?.transaction
    ) return;
    marketPreparing = true;
    marketError = null;
    try {
      // This refetches the detail with ETag revalidation. The modal then
      // presents its current entryRevision for an explicit user confirmation.
      marketConfirm = await marketPrepareInstall(slug);
    } catch (e) {
      marketError = marketFailureMessage("market.prepareFailed", e);
    }
    marketPreparing = false;
  }

  async function doMarketInstall(item: MarketPluginSummary) {
    if (marketOffline || item.installable !== true) return;
    await prepareMarketInstall(item.slug);
  }

  function confirmMarketInstall() {
    const preview = marketConfirm;
    if (!preview || pluginBusy || pluginProfileTransitioning) return;
    marketConfirm = null;
    pluginBusy = true;
    pluginDoneExpected = true;
    pluginExpectedOp = "market-install";
    pluginError = null;
    pluginLogs = [];
    pluginLogsOpen = true;
    void marketInstallPlugin(preview.slug, preview.entryRevision).catch((e) => {
      pluginBusy = false;
      pluginDoneExpected = false;
      pluginExpectedOp = null;
      pluginError = marketFailureMessage("market.installFailed", e);
      pluginLogsOpen = true;
      void refreshPlugins();
    });
    showToast(t("toast.marketInstalling", { name: preview.packageName }));
  }

  async function doActivateMarketPlugin(plugin: PluginEntry) {
    if (
      pluginBusy ||
      pluginProfileTransitioning ||
      recoveryOverview?.transaction ||
      plugin.state !== "pending" ||
      !plugin.slug ||
      !plugin.entryRevision
    ) {
      return;
    }
    pluginBusy = true;
    // Activation is a synchronous IPC mutation and deliberately emits no
    // plugin-done event. Keep it out of the asynchronous completion monitor.
    pluginDoneExpected = false;
    pluginExpectedOp = null;
    pluginError = null;
    try {
      await activateMarketPlugin(plugin.slug, plugin.entryRevision);
      pluginRestartNotice = t("plugin.activationRestart", { name: plugin.name });
      showToast(t("toast.pluginActivated", { name: plugin.name }));
    } catch (e) {
      pluginError = marketFailureMessage("market.activateFailed", e);
    } finally {
      pluginBusy = false;
      // Refresh after both success and partial-commit errors. The backend can
      // truthfully expose an active bundle even when final marker cleanup was
      // interrupted, and the UI must not keep rendering its stale pending row.
      await refreshPlugins();
      if (plugins.some((entry) => entry.name === plugin.name && entry.state === "active")) {
        pluginRestartNotice = t("plugin.activationRestart", { name: plugin.name });
        if (pluginError) {
          pluginError = t("plugin.activationPartial", { detail: pluginError });
        }
      }
    }
  }

  async function doPickSideload() {
    try {
      const path = await pickSideloadFile();
      if (path) sideloadPath = path;
    } catch (e) {
      showToast(t("toast.pickFileFailed", { detail: String(e) }));
    }
  }

  async function confirmSideloadInstall() {
    if (
      !sideloadPath ||
      pluginBusy ||
      pluginProfileTransitioning ||
      recoveryOverview?.transaction
    ) return;
    const path = sideloadPath;
    sideloadPath = null;
    pluginBusy = true;
    pluginDoneExpected = true;
    pluginExpectedOp = "add";
    pluginError = null;
    pluginLogs = [];
    pluginLogsOpen = true;
    showToast(t("toast.sideloadInstalling", { path }));
    try {
      await sideloadPlugin(path);
    } catch (e) {
      pluginBusy = false;
      pluginDoneExpected = false;
      pluginExpectedOp = null;
      pluginError = t("plugin.sideloadFailed", { detail: String(e) });
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
      showToast(t("toast.presetDownloadFailed", { detail: String(e) }));
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
      showToast(t("toast.presetImported", { id }));
      remotePresetRequest = null;
      remotePresetPreview = null;
      await refreshPresets();
    } catch (e) {
      showToast(t("toast.presetImportFailed", { detail: String(e) }));
    }
  }

  async function doRestart() {
    if (busy || pluginBusy || recoveryBusy) return;
    busy = true;
    restartInFlight = true;
    try {
      await restart();
      showToast(t("toast.restartRequested"));
    } catch (e) {
      restartInFlight = false;
      showToast(t("toast.actionFailed", { detail: String(e) }));
    }
    busy = false;
  }

  async function doShutdown() {
    busy = true;
    try {
      await shutdown();
      showToast(t("toast.harnessStopped"));
    } catch (e) {
      showToast(t("toast.actionFailed", { detail: String(e) }));
    }
    busy = false;
  }

  async function doOpen() {
    try {
      await openHarness();
    } catch (e) {
      showToast(t("toast.actionFailed", { detail: String(e) }));
    }
  }

  async function doCheckUpdate() {
    updateBusy = true;
    updateError = null;
    try {
      const info = await checkUpdate();
      updateInfo = info;
      if (info.unsupported) {
        showToast(updateUnsupportedMessage(info.unsupportedReason));
      } else if (!info.available) {
        showToast(t("toast.upToDate"));
      }
    } catch (e) {
      updateError = t("update.checkFailed", { detail: String(e) });
    }
    updateBusy = false;
  }

  async function doInstallUpdate() {
    updateBusy = true;
    updateError = null;
    updatePercent = null;
    try {
      showToast(t("toast.updateDownloading"));
      await installUpdateAndRestart();
    } catch (e) {
      updateError = t("update.installFailed", { detail: String(e) });
      updateBusy = false;
      updatePercent = null;
    }
  }

  async function doExportDiagnostics() {
    diagnosticsBusy = true;
    try {
      const saved = await exportDiagnostics();
      if (saved) showToast(t("toast.diagnosticsExported"));
    } catch (e) {
      if (String(e).includes("cancelled")) showToast(t("toast.diagnosticsCancelled"));
      else showToast(t("toast.exportFailed", { detail: String(e) }));
    } finally {
      diagnosticsBusy = false;
    }
  }

  async function doCancelDiagnosticsExport() {
    try {
      if (await cancelDiagnosticsExport()) showToast(t("toast.cancellingExport"));
    } catch (e) {
      showToast(t("toast.cancelFailed", { detail: String(e) }));
    }
  }

  async function setDetailedDiagnostics(enabled: boolean) {
    if (!inTauri || diagnosticModeBusy) return;
    diagnosticModeBusy = true;
    try {
      diagnosticMode = await setDiagnosticMode(enabled);
      showToast(t(enabled ? "toast.diagnosticModeEnabled" : "toast.diagnosticModeDisabled"));
      if (!diagnosticMode.persisted) {
        showToast(
          t(
            diagnosticMode.enabled
              ? "diagnostics.modeSessionOnly"
              : "diagnostics.modeDisabledSessionOnly",
          ),
        );
      }
    } catch {
      showToast(t("toast.diagnosticModeFailed"));
    } finally {
      diagnosticModeBusy = false;
    }
  }

  async function doClearDetailedDiagnostics() {
    if (!inTauri || diagnosticModeBusy) return;
    diagnosticModeBusy = true;
    try {
      await clearDiagnosticLogs();
      diagnosticMode = await getDiagnosticMode();
      showToast(t("toast.diagnosticLogsCleared"));
    } catch {
      showToast(t("toast.diagnosticModeFailed"));
    } finally {
      diagnosticModeBusy = false;
    }
  }

  async function refreshDetailedDiagnostics() {
    if (!inTauri) return;
    try {
      diagnosticMode = await getDiagnosticMode();
    } catch {
      // Diagnostic-mode state is optional for an interrupted downgrade or a
      // partially updated local shell.  Do not let a status refresh make the
      // controller unusable in that recovery scenario.
    }
  }

  async function doQuitApp() {
    try {
      await quitApp();
    } catch (e) {
      showToast(t("toast.quitFailed", { detail: String(e) }));
    }
  }

  async function openSite(url: string) {
    try {
      await openUrl(url);
    } catch (e) {
      showToast(t("toast.openFailed", { detail: String(e) }));
    }
  }

  async function reportContent() {
    try {
      const url = new URL("https://github.com/web-casa/DeepSeek-Harness-Desktop/issues/new");
      url.searchParams.set("title", t("report.contentTitle"));
      url.searchParams.set(
        "body",
        [
          t("report.contentPrompt"),
          "",
          t("report.contentPrivacy"),
        ].join("\n"),
      );
      await openUrl(url.toString());
    } catch (e) {
      showToast(t("toast.openFailed", { detail: String(e) }));
    }
  }

  async function reportIssue() {
    try {
      const d = await getDiagnostics();
      const platform = (d.platform as { os?: string; arch?: string }) ?? {};
      const body = [
        t("report.version", { desktop: versions.desktop, harness: versions.harness }),
        t("report.platform", { os: platform.os ?? "?", arch: platform.arch ?? "?" }),
        "",
        t("report.issuePrompt"),
        t("report.diagnosticsPrompt"),
      ].join("\n");
      const url = new URL("https://github.com/web-casa/DeepSeek-Harness-Desktop/issues/new");
      url.searchParams.set("template", "bug_report.md");
      url.searchParams.set("labels", "bug");
      url.searchParams.set("title", t("report.issueTitle"));
      url.searchParams.set("body", body);
      await openUrl(url.toString());
    } catch (e) {
      showToast(t("toast.openFailed", { detail: String(e) }));
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
      if (String(e) !== "cancelled") presetError = t("preset.readFailed", { detail: String(e) });
    }
    presetBusy = false;
  }

  async function doImportPreset() {
    presetBusy = true;
    presetError = null;
    try {
      const id = await importPreset();
      presetPreview = null;
      showToast(t("toast.presetImportedHarness", { id }));
      await refreshPresets();
    } catch (e) {
      presetError = t("toast.presetImportFailed", { detail: String(e) });
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
      showToast(t("toast.presetExported", { id }));
    } catch (e) {
      if (String(e) !== "cancelled") showToast(t("toast.presetExportFailed", { detail: String(e) }));
    }
    presetBusy = false;
  }

  async function doDeletePreset(id: string) {
    if (presetBusy) return;
    presetBusy = true;
    try {
      await deletePreset(id);
      confirmDelete = null;
      showToast(t("toast.presetDeleted", { id }));
      await refreshPresets();
    } catch (e) {
      showToast(t("toast.presetDeleteFailed", { detail: String(e) }));
    }
    presetBusy = false;
  }

  async function copyDiagnostics() {
    try {
      const payload = await getDiagnostics();
      await navigator.clipboard.writeText(JSON.stringify(payload, null, 2));
      showToast(t("toast.diagnosticsCopied"));
    } catch (e) {
      showToast(t("toast.copyFailed", { detail: String(e) }));
    }
  }

  async function refreshPlugins() {
    if (pluginRefreshInFlight) {
      pluginRefreshQueued = true;
      return;
    }
    pluginRefreshInFlight = true;
    const wasBusy = pluginBusy;
    const expectedDone = pluginDoneExpected;
    const expectedOp = pluginExpectedOp;
    try {
      const res = await listPlugins();
      if (
        pluginBusy !== wasBusy ||
        pluginDoneExpected !== expectedDone ||
        pluginExpectedOp !== expectedOp
      ) {
        // A user action or live completion event changed local state while
        // this request was in flight. Its busy snapshot may predate that
        // transition, so never let it resurrect or erase the newer state or
        // plugin list.
        pluginRefreshQueued = true;
        return;
      }
      plugins = res.plugins;
      // A controller webview can survive an interrupted binary downgrade.
      // Treat an older backend's missing read-only report as "no report",
      // rather than letting this purely advisory feature break the plugin
      // section before the user can repair or update Desktop.
      profileConsistency = res.consistency ?? {
        issues: [],
        cleanupEligibleCount: 0,
      };
      // Backend busy is the truth across webview reloads: an op may still be
      // running after the UI restarted, and the single-flight flag is
      // app-wide. The helper deliberately does not guess whether an unknown
      // operation emits plugin-done (activation/recovery do not).
      const completion = reconcilePluginCompletion({
        frontendBusy: wasBusy,
        backendBusy: res.busy,
        doneExpected: expectedDone,
      });
      pluginBusy = res.busy;
      pluginDoneExpected = completion.doneExpected;
      pluginExpectedOp = res.busy ? expectedOp : null;
      if (completion.missedDone) {
        // Tauri events are live notifications, not a durable queue. A very
        // fast setup failure or a webview reload can miss `plugin-done`;
        // polling backend truth prevents the UI from remaining busy forever.
        pluginError ??=
          expectedOp === "market-install"
            ? t("plugin.missedMarketCompletion")
            : t("plugin.missedCompletion");
        if (expectedOp === "remove") {
          pluginRestartNotice = t("plugin.removeFinishedRestart");
        } else if (expectedOp === "add") {
          pluginRestartNotice = t("plugin.installFinishedRestart");
        }
        pluginLogsOpen = true;
      }
    } catch {
      /* non-fatal */
    } finally {
      pluginRefreshInFlight = false;
      if (pluginRefreshQueued) {
        pluginRefreshQueued = false;
        void refreshPlugins();
      }
    }
  }

  async function refreshRecovery() {
    try {
      recoveryOverview = await getPluginRecovery();
      recoveryError = null;
    } catch (e) {
      recoveryError = t("recovery.unavailable", { detail: String(e) });
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
        showToast(t("toast.recoveryDisabled", { name: confirmation.candidate.packageName }));
      } else if (confirmation.action === "rollback" && transaction) {
        await rollbackPluginRecovery(transaction.transactionId);
        showToast(t("toast.recoveryRolledBack", { name: transaction.packageName }));
      } else if (confirmation.action === "finalize" && transaction) {
        await finalizePluginRecovery(transaction.transactionId);
        showToast(t("toast.recoveryFinalized", { name: transaction.packageName }));
      }
      await refreshRecovery();
      await refreshPlugins();
    } catch (e) {
      recoveryError = t("recovery.failed", { detail: String(e) });
      await refreshRecovery();
    }
    recoveryBusy = false;
  }

  function startPluginOp(name: string, op: "install" | "uninstall") {
    if (
      pluginBusy ||
      pluginProfileTransitioning ||
      recoveryOverview?.transaction ||
      !name.trim()
    ) return;
    pluginBusy = true;
    pluginDoneExpected = true;
    pluginExpectedOp = op === "install" ? "add" : "remove";
    pluginError = null;
    pluginLogs = [];
    pluginLogsOpen = true;
    const label =
      op === "install"
        ? t("toast.pluginInstalling", { name: name.trim() })
        : t("toast.pluginUninstalling", { name: name.trim() });
    const call = op === "install" ? installPlugin(name.trim()) : uninstallPlugin(name.trim());
    void call.catch((e) => {
      pluginBusy = false;
      pluginDoneExpected = false;
      pluginExpectedOp = null;
      if (op === "uninstall") {
        // Synchronous setup can fail after exact pre-disable but before the
        // worker exists, so no plugin-done event will arrive to set the usual
        // restart notice. Keep this wording truthful for failures that
        // happened before mutation as well.
        pluginRestartNotice = t("plugin.uninstallRestart");
      }
      pluginError = t("plugin.operationFailed", {
        operation: t(op === "install" ? "action.install" : "action.uninstall"),
        detail: String(e),
      });
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
      showToast(t("toast.cancelRequested"));
    } catch (e) {
      showToast(t("toast.cancelFailed", { detail: String(e) }));
    }
  }

  async function doPreviewProfileCleanup() {
    if (
      pluginBusy ||
      pluginProfileTransitioning ||
      recoveryOverview?.transaction ||
      profileCleanupBusy ||
      marketPreparing ||
      marketConfirm ||
      sideloadPath ||
      pluginInstallRequest ||
      remotePresetRequest ||
      recoveryConfirm
    ) {
      return;
    }
    profileCleanupBusy = true;
    profileCleanupError = null;
    try {
      profileCleanupPreview = await previewProfilePatchCleanup();
    } catch (e) {
      profileCleanupError = t("profileConsistency.previewFailed", { detail: String(e) });
    } finally {
      profileCleanupBusy = false;
    }
  }

  async function doApplyProfileCleanup() {
    const preview = profileCleanupPreview;
    if (!preview || profileCleanupBusy || pluginBusy || pluginProfileTransitioning) return;
    profileCleanupBusy = true;
    profileCleanupError = null;
    try {
      const applied = await applyProfilePatchCleanup(preview.transactionId);
      profileCleanupPreview = null;
      showToast(
        t("toast.profileConsistencyCleaned", {
          count: applied.removalCount,
        }),
      );
      await refreshPlugins();
    } catch (e) {
      profileCleanupPreview = null;
      profileCleanupError = t("profileConsistency.applyFailed", { detail: String(e) });
      await refreshPlugins();
    } finally {
      profileCleanupBusy = false;
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

  // Browsers notify language-preference changes, but do not expose a portable
  // native store. In Tauri, refresh the native state too so tray/menu/window
  // labels remain synchronized; browser mode stays local-only.
  onMount(() => {
    const syncSystemLanguages = () => {
      const languages = browserLanguages();
      systemLanguages = languages;
      if (!inTauri || localeSaving || localePreference !== "system") return;
      const request = ++localeRequest;
      void setPresentationLocale("system", languages)
        .then((snapshot) => {
          if (request === localeRequest) applyNativeLocale(snapshot);
        })
        .catch(() => {
          // Preserve the last synchronized native locale rather than making a
          // failed IPC call change only one of the native and Svelte surfaces.
        });
    };
    window.addEventListener("languagechange", syncSystemLanguages);
    return () => window.removeEventListener("languagechange", syncSystemLanguages);
  });

  $effect(() => {
    if (typeof document === "undefined") return;
    document.documentElement.lang = controllerLocale;
    // Native windows are titled by Rust. Updating document.title there could
    // race the fixed window-chrome title; browser mode still gets a localized
    // tab title.
    if (!inTauri) document.title = t("window.controllerTitle");
  });

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
    if (inTauri) {
      await synchronizeNativeLocale();
      await refreshDetailedDiagnostics();
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
      // `hasCapturedLogs` is durable state rather than an event per stderr
      // line. The writer intentionally runs behind a bounded background
      // queue, so do one short follow-up probe after a terminal crash: the
      // first snapshot can legitimately precede the final stderr flush. This
      // keeps the Clear button accurate without adding IPC to the high-volume
      // log stream.
      if (p.status === "crashed") {
        void refreshDetailedDiagnostics();
        setTimeout(() => {
          if (status === "crashed") void refreshDetailedDiagnostics();
        }, 250);
      }
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
      if (pluginDoneExpected && pluginExpectedOp && p.op !== pluginExpectedOp) {
        // Completion is queued before the backend releases its operation
        // transition gate. Keep this defensive check so an unexpected stale
        // event can never clear a different operation's busy state.
        void refreshPlugins();
        return;
      }
      pluginBusy = false;
      pluginDoneExpected = false;
      pluginExpectedOp = null;
      const tailLines = p.tail
        .split(/\r?\n/)
        .map((line) => line.trim())
        .filter(Boolean);
      // Early setup/spawn errors have no streamed plugin-log event. Preserve
      // plugin-done.tail so a failure can never render as "暂无日志".
      if (pluginLogs.length === 0 && tailLines.length > 0) {
        pluginLogs = tailLines.slice(-300);
      }
      if (p.op === "remove" && p.exit !== 0) {
        // Every spawned remove has already completed exact pre-disable. Even
        // when pnpm fails or cancellation wins, the currently running Harness
        // may still hold the old plugin and must be restarted to unload it.
        pluginRestartNotice = t("plugin.removeIncompleteRestart");
      }
      if (p.exit === 0) {
        // A delayed live event can arrive just after polling synthesized a
        // missed-completion warning. The durable success supersedes it.
        pluginError = null;
        pluginName = "";
        if (p.op === "market-install") {
          showToast(t("toast.marketPluginVerified"));
        } else if (p.op === "remove") {
          pluginRestartNotice = t("plugin.uninstalledRestart");
          showToast(t("toast.pluginUninstalled"));
        } else if (p.op === "add") {
          pluginRestartNotice = t("plugin.installedRestart");
          showToast(t("toast.pluginInstalled"));
        } else {
          pluginError = t("plugin.unknownOperation");
          pluginLogsOpen = true;
        }
      } else {
        const detail = tailLines.at(-1);
        pluginError = t("plugin.operationExit", {
          exit: p.exit ?? t("plugin.exitTerminated"),
          detail: detail ? t("plugin.exitDetail", { detail }) : t("plugin.exitLogDetail"),
        });
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

  // Event delivery can race an immediate backend failure or a webview
  // reload. While an operation is active, cheaply poll the local IPC state so
  // a missed `plugin-done` cannot leave every plugin control disabled.
  $effect(() => {
    if (!inTauri || !pluginBusy) return;
    const timer = setInterval(() => void refreshPlugins(), 1000);
    return () => clearInterval(timer);
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
      <span class="subtitle">{t("header.subtitle")}</span>
    </div>
    <div class="spacer"></div>
    <label class="locale-control">
      <span>{t("locale.label")}</span>
      <select
        value={localePreference}
        onchange={setLocalePreference}
        aria-label={t("locale.label")}
        disabled={localeSaving}
      >
        <option value="system">{t("locale.system")}</option>
        <option value="zh-CN">{t("locale.zhCN")}</option>
        <option value="en">{t("locale.en")}</option>
      </select>
    </label>
    <span class="badge">v{versions.desktop}</span>
  </header>

  {#if !inTauri}
    <div class="warn-banner">
      {t("browser.warning")}
      <b>pnpm tauri dev</b> {t("browser.warningSuffix")}
    </div>
  {/if}

  <div class="card">
    <div class="status-row">
      {#if booting}
        <div class="spinner"></div>
      {:else}
        <div class="dot {status}"></div>
      {/if}
      <span class="status-text">{t(STATUS_TEXT[status])}</span>
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
          {t("step.runtime", { node: versions.node, harness: versions.harness })}
        </div>
        <div class={stepClass("start")}>
          <span class="ico">{stepClass("start") === "fail" ? "✗" : stepClass("start") === "done" ? "✓" : "●"}</span>
          {t("step.start")}
        </div>
        <div class={stepClass("ready")}>
          <span class="ico">{stepClass("ready") === "fail" ? "✗" : stepClass("ready") === "done" ? "✓" : "●"}</span>
          {t("step.ready")}
        </div>
      </div>
    {/if}

    <div class="actions">
      {#if status === "running"}
        <button class="primary" onclick={doOpen}>{t("action.openHarness")}</button>
        <button class="ghost" onclick={doRestart} disabled={busy || pluginBusy || recoveryBusy}>{t("action.restart")}</button>
        <button class="danger-ghost" onclick={doShutdown} disabled={busy}>{t("action.stop")}</button>
      {:else if status === "crashed" || status === "stopped"}
        <button class="primary" onclick={doRestart} disabled={busy || pluginBusy || recoveryBusy}>
          {status === "stopped" ? t("action.restart") : t("action.restartHarness")}
        </button>
        {#if status === "crashed"}
          <button class="ghost" onclick={copyDiagnostics}>{t("action.copyDiagnostics")}</button>
          <button class="ghost" onclick={doExportDiagnostics} disabled={diagnosticsBusy}>
            {diagnosticsBusy ? t("action.exporting") : t("action.exportDiagnostics")}
          </button>
          {#if diagnosticsBusy}
            <button class="ghost" onclick={doCancelDiagnosticsExport}>{t("action.cancelExport")}</button>
          {/if}
          <button class="danger-ghost" onclick={doQuitApp}>{t("action.quit")}</button>
        {/if}
      {:else}
        <button class="ghost" disabled>{t("action.starting")}</button>
      {/if}
    </div>
    {#if status === "crashed"}
      <p class="privacy-note">
        {t("diagnostics.privacy")}
      </p>
    {/if}
  </div>

  {#if recoveryError || recoveryOverview?.transaction || (recoveryOverview?.candidates.length ?? 0) > 0}
    <div class="card recovery-card">
      <div class="update-row">
        <span class="update-title">{t("recovery.title")}</span>
        {#if recoveryBusy}<span class="plugin-busy"><span class="spinner"></span> {t("recovery.working")}</span>{/if}
      </div>
      {#if recoveryError}
        <div class="notice-box">{recoveryError}</div>
      {/if}
      {#if recoveryOverview?.transaction}
        {@const transaction = recoveryOverview.transaction}
        <div class="preset-row">
          <span>
            <span class="preset-name">{transaction.packageName}</span>
            <span class="badge">{transaction.marketManaged ? t("recovery.marketSource") : t("recovery.userSource")}</span>
            <span class="badge">
              {transaction.phase === "isolated" ? t("recovery.isolated") : t("recovery.disabled")}
            </span>
          </span>
          <button
            class="ghost"
            onclick={() => (recoveryConfirm = { action: "rollback" })}
            disabled={recoveryBusy}
          >{t("action.rollbackReenable")}</button>
          <button
            class="primary"
            onclick={() => (recoveryConfirm = { action: "finalize" })}
            disabled={recoveryBusy || transaction.phase !== "isolated"}
          >{t("action.keepIsolated")}</button>
        </div>
        <div class="trust-note">
          {t("recovery.rollbackNote")}
        </div>
      {:else if recoveryOverview?.terminalStartupFailure}
        <div class="trust-note">
          {t("recovery.candidatesNote")}
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
            >{t("action.reviewIsolate")}</button>
          </div>
        {/each}
      {/if}
    </div>
  {/if}

  <button class="logs-toggle" onclick={() => (logsOpen = !logsOpen)}>
    <span>{logsOpen ? "▾" : "▸"}</span>
    {t("logs.runtime")} {logsOpen ? "" : `(${logs.length})`}
  </button>

  {#if logsOpen}
    <div class="console">
      {#if logs.length === 0}
        <span class="l-empty">{t("logs.empty")}</span>
      {:else}
        {#each logs as [stream, line], i (i)}
          <div class="l-{stream}">{line}</div>
        {/each}
      {/if}
    </div>
  {/if}

  <div class="card diagnostics-card">
    <div class="update-row">
      <span class="update-title">{t("diagnostics.modeTitle")}</span>
      <span class="update-info">
        {diagnosticMode.enabled ? t("diagnostics.modeEnabled") : t("diagnostics.modeDisabled")}
      </span>
      <button
        class={diagnosticMode.enabled ? "danger-ghost" : "ghost"}
        onclick={() => void setDetailedDiagnostics(!diagnosticMode.enabled)}
        disabled={!inTauri || diagnosticModeBusy}
      >
        {diagnosticModeBusy
          ? t("diagnostics.switching")
          : diagnosticMode.enabled
            ? t("action.disableDetailedDiagnostics")
            : t("action.enableDetailedDiagnostics")}
      </button>
      {#if diagnosticMode.hasCapturedLogs}
        <button
          class="ghost"
          onclick={() => void doClearDetailedDiagnostics()}
          disabled={!inTauri || diagnosticModeBusy}
        >{t("action.clearDetailedDiagnostics")}</button>
      {/if}
    </div>
    <div class="trust-note">
      {t("diagnostics.modeDescription")}
    </div>
    {#if diagnosticMode.enabled}
      <div class="notice-box">{t("diagnostics.modeWarning")}</div>
    {:else if diagnosticMode.hasCapturedLogs}
      <div class="notice-box">{t("diagnostics.modeRetained")}</div>
    {/if}
    {#if !diagnosticMode.persisted}
      <div class="notice-box">
        {t(
          diagnosticMode.enabled
            ? "diagnostics.modeSessionOnly"
            : "diagnostics.modeDisabledSessionOnly",
        )}
      </div>
    {/if}
  </div>

  <div class="card update-card">
    <div class="update-row">
      <span class="update-title">{t("update.title")}</span>
      {#if storeBuild}
        <span class="update-info">{t("update.storeManaged")}</span>
      {:else if updateInfo?.available}
        <span class="update-info">{t("update.available", { version: updateInfo.version ?? t("value.unknown") })}</span>
        <button class="primary" onclick={doInstallUpdate} disabled={updateBusy}>
          {updateBusy
            ? updatePercent !== null
              ? t("update.progress", { percent: updatePercent })
              : t("update.inProgress")
            : t("action.installUpdateRestart")}
        </button>
      {:else if updateInfo?.unsupported}
        <span class="update-info">{updateUnsupportedMessage(updateInfo.unsupportedReason)}</span>
      {:else}
        <button class="ghost" onclick={doCheckUpdate} disabled={updateBusy}>
          {updateBusy ? t("action.checking") : t("action.checkUpdates")}
        </button>
      {/if}
    </div>
    <div class="update-row">
      <span class="update-title">{t("resources.title")}</span>
      <button class="ghost" onclick={() => openSite("https://dsharness.app")}>{t("action.website")}</button>
      <button class="ghost" onclick={() => openSite("https://cordis.run")}>{t("action.pluginMarket")}</button>
      <button class="ghost" onclick={reportContent}>{t("action.reportContent")}</button>
      <button class="ghost" onclick={reportIssue}>{t("action.reportIssue")}</button>
    </div>
    {#if updateError}
      <div class="notice-box">{updateError}</div>
    {/if}
  </div>

  <div class="card preset-card">
    <div class="update-row">
      <span class="update-title">{t("presets.title")}</span>
      <button class="ghost" onclick={doPreviewPreset} disabled={presetBusy}>{t("action.importPreset")}</button>
    </div>
    {#if presetPreview}
      <div class="notice-box">
        <b>{t("preset.files", { id: presetPreview.id, count: presetPreview.files.length })}</b>
        {#if presetPreview.warnings.includes("possible-secrets")}
          · <span class="warn">⚠ {t("preset.possibleSecrets")}</span>
        {/if}
        {#if presetPreview.warnings.includes("absolute-paths")}
          · <span class="warn">⚠ {t("preset.absolutePaths")}</span>
        {/if}
        <div>{t("preset.trust")}</div>
        <button class="primary" onclick={doImportPreset} disabled={presetBusy}>{t("action.confirmImport")}</button>
        <button class="ghost" onclick={doCancelPresetPreview}>{t("action.cancel")}</button>
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
              <span class="preset-badge {issue.kind}">{t(ISSUE_LABEL[issue.kind])}</span>
            {/each}
          </span>
          <button class="ghost" onclick={() => doExportPreset(row.id)} disabled={presetBusy}>{t("action.export")}</button>
          {#if confirmDelete === row.id}
            <button class="danger-ghost" onclick={() => doDeletePreset(row.id)} disabled={presetBusy}>{t("action.confirmDelete")}</button>
            <button class="ghost" onclick={() => (confirmDelete = null)}>{t("action.cancel")}</button>
          {:else}
            <button class="ghost" onclick={() => (confirmDelete = row.id)} disabled={presetBusy}>{t("action.delete")}</button>
          {/if}
        </div>
        {#if confirmDelete === row.id}
          <div class="preset-issues">
            · {t("preset.deleteNote")}
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
      <div class="preset-row"><span class="l-empty">{t("preset.empty")}</span></div>
    {/if}
  </div>

  <div class="card plugin-card">
    <div class="update-row">
      <span class="update-title">{t("market.title")}</span>
      {#if !storeBuild}
        <button class="ghost" onclick={doPickSideload} disabled={pluginProfileTransitioning || recoveryOverview?.transaction != null}>{t("action.sideload")}</button>
      {/if}
    </div>
    <div class="plugin-row">
      <input
        class="plugin-input"
        type="text"
        placeholder={t("market.searchPlaceholder")}
        aria-label={t("market.searchPlaceholder")}
        bind:value={marketQuery}
        disabled={marketBusy}
        spellcheck="false"
        onkeydown={(e) => {
          if (e.key === "Enter") doMarketSearch();
        }}
      />
      <select class="plugin-input" bind:value={marketCategory} disabled={marketBusy} aria-label={t("market.categoryLabel")}>
        <option value="">{t("market.allCategories")}</option>
        <option value="agent">{t("market.categoryAgent")}</option>
        <option value="market">{t("market.categoryMarket")}</option>
      </select>
      <button class="primary" onclick={() => doMarketSearch(true)} disabled={marketBusy}>
        {marketBusy ? t("action.searching") : t("action.search")}
      </button>
    </div>
    {#if marketError}
      <div class="notice-box">{marketError}</div>
    {/if}
    {#if marketOffline}
      <div class="notice-box">
        {t("market.offline", {
          cachedAt: marketOfflineFetchedAt
            ? t("market.cachedAt", { time: formatControllerDate(controllerLocale, marketOfflineFetchedAt) })
            : "",
        })}
      </div>
    {/if}
    {#if marketItems.length > 0}
      {#if marketCount !== null}
        <div class="preset-issues">{t("market.count", { count: marketCount })}</div>
      {/if}
      {#each marketItems as item (item.slug)}
        <div class="preset-row">
          <span class="preset-id">
            <span class="preset-name">{item.name}</span>
            {#if marketVersion(item)}<span class="badge">v{marketVersion(item)}</span>{/if}
            {#if item.stars != null}<span class="badge">★ {item.stars}</span>{/if}
            {#if item.category}<span class="badge">{item.category}</span>{/if}
            <span class="badge">{item.platforms.includes("desktop") ? t("market.desktop") : t("market.webOnly")}</span>
          </span>
          <button class="ghost" onclick={() => doMarketDetail(item.slug)} disabled={marketDetailBusy || marketOffline}>{t("action.details")}</button>
          {#if item.installable === true}
            <button
              class="primary"
              onclick={() => doMarketInstall(item)}
              disabled={pluginBusy || pluginProfileTransitioning || marketPreparing || marketOffline || recoveryOverview?.transaction != null}
            >{marketPreparing ? t("action.verifying") : t("action.install")}</button>
          {:else}
            <button class="ghost" disabled title={item.installReason ?? t("market.notInstallable")}>
              {item.blocked ? t("market.blocked") : item.deprecated ? t("market.deprecated") : t("market.notInstallable")}
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
            {marketBusy ? t("action.loading") : t("action.loadMore")}
          </button>
        </div>
      {/if}
    {:else}
      <div class="preset-row"><span class="l-empty">{t("market.empty")}</span></div>
    {/if}
  </div>

  <div class="card plugin-card">
    <div class="update-row">
      <span class="update-title">{t("plugins.title")}</span>
    </div>
    {#if storeBuild}
      <div class="plugin-row">
        <span class="update-info">{t("store.auditedOnly")}</span>
        {#if pluginBusy}
          <span class="plugin-busy"><span class="spinner"></span> {t("plugin.working")}</span>
          <button class="danger-ghost" onclick={doCancelPluginOp}>{t("action.cancel")}</button>
        {:else}
          <button class="ghost" onclick={() => openSite("https://cordis.run")}>{t("action.openPluginMarket")}</button>
        {/if}
      </div>
      <div class="trust-note">
        {t("store.trust")}
      </div>
    {:else}
      <div class="plugin-row">
        <input
          class="plugin-input"
          type="text"
          placeholder={t("plugins.placeholder")}
          aria-label={t("plugins.placeholder")}
          bind:value={pluginName}
          disabled={pluginBusy || pluginProfileTransitioning || recoveryOverview?.transaction != null}
          spellcheck="false"
          onkeydown={(e) => {
            if (e.key === "Enter") startPluginOp(pluginName, "install");
          }}
        />
        {#if pluginBusy}
          <span class="plugin-busy"><span class="spinner"></span> {t("plugin.working")}</span>
          <button class="danger-ghost" onclick={doCancelPluginOp}>{t("action.cancel")}</button>
        {:else}
          <button class="primary" onclick={() => startPluginOp(pluginName, "install")} disabled={pluginProfileTransitioning || !pluginName.trim() || recoveryOverview?.transaction != null}>
            {t("action.install")}
          </button>
        {/if}
      </div>
    <div class="trust-note">
      {t("plugins.trustBeforeLink")}
      <button class="inline-link" onclick={() => openSite("https://cordis.run")}>cordis.run {t("action.pluginMarket")}</button>
      {t("plugins.trustAfterLink")}
    </div>
  {/if}
    {#if profileConsistency.issues.length > 0 || profileCleanupError}
      <div class="notice-box">
        <b>{t("profileConsistency.title")}</b>
        {#each profileConsistency.issues as issue (issue.packageName)}
          <div>
            · {t(
              issue.active
                ? "profileConsistency.missingActiveDependency"
                : "profileConsistency.missingDependency",
              { name: issue.packageName },
            )}
          </div>
        {/each}
        {#if profileConsistency.cleanupEligibleCount > 0}
          <div class="trust-note">{t("profileConsistency.exactOnly")}</div>
          <button
            class="ghost"
            onclick={doPreviewProfileCleanup}
            disabled={
              profileCleanupBusy ||
              pluginBusy ||
              pluginProfileTransitioning ||
              recoveryOverview?.transaction != null
            }
          >{profileCleanupBusy ? t("action.reviewing") : t("action.reviewCleanup")}</button>
        {:else if profileConsistency.issues.length > 0}
          <div class="trust-note">{t("profileConsistency.manualReview")}</div>
        {/if}
        {#if profileCleanupError}
          <div>{profileCleanupError}</div>
        {/if}
      </div>
    {/if}
    {#if pluginError}
      <div class="notice-box">{pluginError}</div>
    {/if}
    {#if pluginRestartNotice}
      <div class="notice-box">
        {pluginRestartNotice}
        <button class="primary" onclick={doRestart} disabled={busy || pluginBusy || recoveryBusy || status !== "running"}>
          {t("action.restartNow")}
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
            <button class="primary" onclick={() => doActivateMarketPlugin(p)} disabled={pluginBusy || pluginProfileTransitioning || recoveryOverview?.transaction != null}>
              {t("action.activate")}
            </button>
          {/if}
          <button class="ghost" onclick={() => startPluginOp(p.name, "uninstall")} disabled={pluginBusy || pluginProfileTransitioning || recoveryOverview?.transaction != null}>{t("action.uninstall")}</button>
        </div>
      {/each}
    {:else}
      <div class="preset-row"><span class="l-empty">{t("plugins.empty")}</span></div>
    {/if}
    <button class="logs-toggle" onclick={() => (pluginLogsOpen = !pluginLogsOpen)}>
      <span>{pluginLogsOpen ? "▾" : "▸"}</span>
      {t("logs.install")} {pluginLogsOpen ? "" : `(${pluginLogs.length})`}
    </button>
    {#if pluginLogsOpen}
      <div class="console">
        {#if pluginLogs.length === 0}
          <span class="l-empty">{t("logs.empty")}</span>
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
      {t("footer.note")}
    </div>
  </footer>
</div>

{#if recoveryConfirm}
  <div class="modal-backdrop">
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      aria-label={t("dialog.recoveryLabel")}
      tabindex="-1"
      use:trapDialog={{
        onEscape: () => (recoveryConfirm = null),
        escapeDisabled: recoveryBusy,
      }}
    >
      <div class="modal-title">
        {recoveryConfirm.action === "disable"
          ? t("dialog.confirmIsolate")
          : recoveryConfirm.action === "rollback"
            ? t("dialog.confirmRollback")
            : t("dialog.confirmKeep")}
      </div>
      <div class="modal-name">
        {recoveryConfirm.candidate?.packageName ?? recoveryOverview?.transaction?.packageName ?? t("recovery.unknownPlugin")}
      </div>
      <div class="modal-warn">
        {#if recoveryConfirm.action === "disable"}
          {t("recovery.isolateWarning")}
        {:else if recoveryConfirm.action === "rollback"}
          {t("recovery.rollbackWarning")}
        {:else}
          {t("recovery.keepWarning")}
        {/if}
      </div>
      <div class="modal-actions">
        <button
          class={recoveryConfirm.action === "finalize" ? "primary" : "danger-ghost"}
          onclick={confirmRecoveryAction}
          disabled={recoveryBusy}
        >{t("action.confirm")}</button>
        <button class="ghost" onclick={() => (recoveryConfirm = null)} disabled={recoveryBusy}>{t("action.cancel")}</button>
      </div>
    </div>
  </div>
{/if}

{#if profileCleanupPreview}
  <div class="modal-backdrop">
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      aria-label={t("dialog.profileCleanupLabel")}
      tabindex="-1"
      use:trapDialog={{
        onEscape: () => (profileCleanupPreview = null),
        escapeDisabled: profileCleanupBusy,
      }}
    >
      <div class="modal-title">{t("dialog.reviewProfileCleanup")}</div>
      <div class="modal-meta">
        {t("dialog.profileCleanupCount", { count: profileCleanupPreview.removalCount })}
      </div>
      <div class="modal-name">
        {#each profileCleanupPreview.packages as packageName (packageName)}
          <div>{packageName}</div>
        {/each}
      </div>
      <div class="modal-warn">{t("dialog.profileCleanupWarning")}</div>
      <div class="modal-actions">
        <button class="danger-ghost" onclick={doApplyProfileCleanup} disabled={profileCleanupBusy}>
          {profileCleanupBusy ? t("action.cleaning") : t("action.confirmCleanup")}
        </button>
        <button
          class="ghost"
          onclick={() => (profileCleanupPreview = null)}
          disabled={profileCleanupBusy}
        >{t("action.cancel")}</button>
      </div>
    </div>
  </div>
{/if}

{#if remotePresetRequest}
  <div class="modal-backdrop">
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      aria-label={t("dialog.remotePresetLabel")}
      tabindex="-1"
      use:trapDialog={{
        onEscape: () => void doRemotePresetDismiss(),
        escapeDisabled:
          remotePresetDownloading || remotePresetRequest.stage === "installing",
      }}
    >
      <div class="modal-title">{t("dialog.installPreset")}</div>
      <div class="modal-meta">
        {t("dialog.claimedPagePreset")} <button
          class="inline-link"
          title={remotePresetRequest!.source}
          onclick={() => openSite(remotePresetRequest!.source)}
        >
          {remotePresetRequest.source}
        </button>
      </div>
      {#if remotePresetRequest.stage === "installing"}
        <div class="modal-warn">{t("dialog.installingPreset")}</div>
      {:else if remotePresetPreview && remotePresetPreview.requestId === remotePresetRequest.requestId}
        <div class="modal-name">{remotePresetPreview.id}</div>
        <div class="modal-meta">
          {t("dialog.presetFiles", { count: remotePresetPreview.files.length })}
          {#if remotePresetPreview.warnings.includes("possible-secrets")}
            · <span class="warn">⚠ {t("preset.possibleSecrets")}</span>
          {/if}
          {#if remotePresetPreview.warnings.includes("absolute-paths")}
            · <span class="warn">⚠ {t("preset.absolutePaths")}</span>
          {/if}
        </div>
        <div class="modal-warn">
          {t("dialog.presetTrust")}
        </div>
        <div class="modal-actions">
          <button class="primary" onclick={doRemotePresetImport}>{t("action.confirm")}</button>
          <button class="ghost" onclick={doRemotePresetDismiss}>{t("action.cancel")}</button>
        </div>
      {:else}
        <div class="modal-warn">
          {t("dialog.presetDownloadWarning")}
        </div>
        <div class="modal-actions">
          <button class="primary" onclick={doRemotePresetDownload} disabled={remotePresetDownloading}>
            {remotePresetDownloading ? t("action.downloading") : t("action.downloadAndCheck")}
          </button>
          <button class="ghost" onclick={doRemotePresetDismiss} disabled={remotePresetDownloading}>{t("action.cancel")}</button>
        </div>
      {/if}
    </div>
  </div>
{/if}

{#if marketDetail}
  <div class="modal-backdrop">
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      aria-label={t("dialog.marketDetailLabel")}
      tabindex="-1"
      use:trapDialog={{ onEscape: () => (marketDetail = null) }}
    >
      <div class="modal-title">{marketDetail.name}</div>
      <div class="modal-name">{marketPackageName(marketDetail)}</div>
      <div class="modal-meta">{marketDescriptionText(marketDetail.description)}</div>
      {#if marketImages.length > 0}
        <div class="market-shots">
          {#each marketImages as src, i (i)}
            <img
              src={src}
              alt={t("image.screenshotAlt", { name: marketDetail.name, index: i + 1 })}
              class="market-shot"
              referrerpolicy="no-referrer"
            />
          {/each}
        </div>
      {/if}
      <div class="modal-actions">
        <button class="ghost" onclick={() => (marketDetail = null)}>{t("action.close")}</button>
      </div>
    </div>
  </div>
{/if}

{#if marketConfirm}
  <div class="modal-backdrop">
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      aria-label={t("dialog.marketInstallLabel")}
      tabindex="-1"
      use:trapDialog={{
        onEscape: () => (marketConfirm = null),
        escapeDisabled: pluginBusy,
      }}
    >
      <div class="modal-title">{t("dialog.installPlugin")}</div>
      <div class="modal-name">{marketConfirm.packageName} v{marketConfirm.version}</div>
      <div class="modal-meta">
        {t("dialog.entryRevision", { revision: marketConfirm.entryRevision })}
      </div>
      <div class="modal-meta">
        {t("dialog.source")}<button class="inline-link" onclick={() => openSite("https://cordis.run/plugins/" + marketConfirm!.slug)}>
          https://cordis.run/plugins/{marketConfirm.slug}
        </button>
      </div>
      <div class="modal-warn">
        {t("dialog.marketInstallWarning")}
      </div>
      <div class="modal-actions">
        <button class="primary" onclick={confirmMarketInstall} disabled={pluginBusy || pluginProfileTransitioning}>{t("action.confirm")}</button>
        <button class="ghost" onclick={() => (marketConfirm = null)}>{t("action.cancel")}</button>
      </div>
    </div>
  </div>
{/if}

{#if sideloadPath && !storeBuild}
  <div class="modal-backdrop">
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      aria-label={t("dialog.sideloadLabel")}
      tabindex="-1"
      use:trapDialog={{
        onEscape: () => (sideloadPath = null),
        escapeDisabled: pluginBusy,
      }}
    >
      <div class="modal-title">{t("dialog.installOfflinePlugin")}</div>
      <div class="modal-name">{sideloadPath}</div>
      <div class="modal-warn">{t("dialog.sideloadWarning")}</div>
      <div class="modal-actions">
        <button class="primary" onclick={confirmSideloadInstall} disabled={pluginBusy || pluginProfileTransitioning}>{t("action.confirm")}</button>
        <button class="ghost" onclick={() => (sideloadPath = null)}>{t("action.cancel")}</button>
      </div>
    </div>
  </div>
{/if}

{#if pluginInstallRequest}
  <div class="modal-backdrop">
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      aria-label={t("dialog.deepLinkLabel")}
      tabindex="-1"
      use:trapDialog={{ onEscape: () => void dismissPluginInstallRequest() }}
    >
      <div class="modal-title">{t("dialog.installPlugin")}</div>
      <div class="modal-name">{t("dialog.claimedPackage", { name: pluginInstallRequest.name })}</div>
      <div class="modal-meta">
        {t("dialog.claimedPagePlugin")}<button
          class="inline-link"
          title={pluginInstallRequest!.source}
          onclick={() => openSite(pluginInstallRequest!.source)}
        >
          {pluginInstallRequest.source}
        </button>
      </div>
      <div class="modal-warn">
        {t("dialog.deepLinkWarning")}
      </div>
      {#if pluginBusy}
        <div class="notice-box">{t("dialog.pluginBusy")}</div>
      {/if}
      <div class="modal-actions">
        <button class="primary" onclick={confirmPluginInstallRequest} disabled={pluginBusy}>
          {pluginBusy ? t("action.busy") : t("action.continueVerify")}
        </button>
        <button class="ghost" onclick={dismissPluginInstallRequest}>{t("action.cancel")}</button>
      </div>
    </div>
  </div>
{/if}

{#if toast}
  <div class="toast">{toast}</div>
{/if}
