const ERROR_PREFIX = {
  timeout: "MARKET_TIMEOUT:",
  unavailable: "MARKET_UNAVAILABLE:",
  invalidResponse: "MARKET_INVALID_RESPONSE:",
  http: "MARKET_HTTP_ERROR:",
  api: "MARKET_API_ERROR:",
} as const;

export type MarketFailureKind = "timeout" | "unavailable" | "invalidResponse" | "http" | "api" | "unknown";

export interface MarketFailure {
  kind: MarketFailureKind;
  /**
   * The Rust protocol deliberately bounds this API error detail. Unknown
   * errors never receive a detail because they may contain an unsafe URL or
   * implementation-specific network information.
   */
  detail?: string;
}

export function classifyMarketFailure(error: unknown): MarketFailure {
  const raw = String(error);
  if (raw.startsWith(ERROR_PREFIX.timeout)) return { kind: "timeout" };
  if (raw.startsWith(ERROR_PREFIX.unavailable)) return { kind: "unavailable" };
  if (raw.startsWith(ERROR_PREFIX.invalidResponse)) return { kind: "invalidResponse" };
  if (raw.startsWith(ERROR_PREFIX.http)) return { kind: "http" };
  if (raw.startsWith(ERROR_PREFIX.api)) {
    const detail = raw.slice(ERROR_PREFIX.api.length).trim();
    return detail ? { kind: "api", detail } : { kind: "api" };
  }
  return { kind: "unknown" };
}

/**
 * Convert the deliberately bounded Rust market-error protocol into a
 * user-facing Chinese message. Unknown values deliberately do not echo the
 * rejected value, which may contain an implementation-specific URL or socket
 * detail outside that protocol.
 */
export function marketFailureText(context: string, error: unknown): string {
  const failure = classifyMarketFailure(error);
  if (failure.kind === "timeout") {
    return `${context}：请求超时，请检查网络后重试`;
  }
  if (failure.kind === "unavailable") {
    return `${context}：市场服务暂时不可用，请稍后重试`;
  }
  if (failure.kind === "invalidResponse") {
    return `${context}：市场服务返回了无法识别的数据`;
  }
  if (failure.kind === "http") {
    return `${context}：市场服务返回 HTTP 错误`;
  }
  if (failure.kind === "api") {
    return failure.detail ? `${context}：${failure.detail}` : `${context}：市场服务返回错误`;
  }
  return `${context}：市场请求失败，请稍后重试`;
}
