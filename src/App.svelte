<script lang="ts">
  import { onMount } from "svelte";
  import {
    getStatus,
    getLogs,
    getVersions,
    restart,
    shutdown,
    openHarness,
    onEvent,
    type Status,
    type StatusPayload,
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

  let unlisten: (() => void) | null = null;
  let logsTimer: ReturnType<typeof setInterval> | null = null;

  const STATUS_TEXT: Record<Status, string> = {
    idle: "等待启动",
    starting: "正在启动 Harness…",
    running: "Harness 运行中",
    stopping: "正在停止 Harness…",
    stopped: "Harness 已停止",
    crashed: "Harness 启动失败",
  };

  function apply(p: StatusPayload) {
    status = p.status;
    url = p.url;
    pid = p.pid;
    lastError = p.lastError;
    if (p.versions) versions = p.versions;
  }

  function showToast(message: string) {
    toast = message;
    setTimeout(() => {
      if (toast === message) toast = null;
    }, 2600);
  }

  async function doRestart() {
    busy = true;
    try {
      await restart();
      showToast("已请求重新启动");
    } catch (e) {
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

  async function copyDiagnostics() {
    const payload = {
      status,
      url,
      pid,
      lastError,
      versions,
      logsTail: logs.slice(-60),
    };
    try {
      await navigator.clipboard.writeText(JSON.stringify(payload, null, 2));
      showToast("诊断信息已复制到剪贴板");
    } catch {
      showToast("复制失败（剪贴板不可用）");
    }
  }

  onMount(async () => {
    try {
      const [st, ver, lg] = await Promise.all([getStatus(), getVersions(), getLogs()]);
      apply(st);
      versions = ver;
      logs = lg;
    } catch {
      inTauri = false;
      return;
    }
    unlisten = await onEvent((p) => apply(p));
    logsTimer = setInterval(async () => {
      if (logsOpen) logs = await getLogs();
    }, 1000);
    return () => {
      unlisten?.();
      if (logsTimer) clearInterval(logsTimer);
    };
  });

  $effect(() => {
    logsOpen; // re-run timer decision when toggled
    if (!inTauri) return;
    if (logsOpen && logsTimer === null) {
      logsTimer = setInterval(async () => {
        logs = await getLogs();
      }, 1000);
    }
    if (!logsOpen && logsTimer !== null) {
      clearInterval(logsTimer);
      logsTimer = null;
    }
  });

  function stepClass(target: "check" | "start" | "ready"): string {
    if (status === "running") return "done";
    if (status === "crashed" && target !== "check") return "fail";
    if (target === "check") return status === "starting" || status === "stopping" || status === "running" ? "done" : "active";
    if (target === "start") return status === "starting" ? "active" : "done";
    return status === "starting" ? "active" : "done";
  }

  const booting = status === "idle" || status === "starting" || status === "stopping";
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
    <span class="badge">P0</span>
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
        {#each logs as [stream, line] (line)}
          <div class="l-{stream}">{line}</div>
        {/each}
      {/if}
    </div>
  {/if}

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
