const ERROR_PREFIX = {
  timeout: "MARKET_TIMEOUT:",
  unavailable: "MARKET_UNAVAILABLE:",
  invalidResponse: "MARKET_INVALID_RESPONSE:",
  http: "MARKET_HTTP_ERROR:",
  api: "MARKET_API_ERROR:",
} as const;

/**
 * Convert the deliberately bounded Rust market-error protocol into a
 * user-facing Chinese message. Unknown values deliberately do not echo the
 * rejected value, which may contain an implementation-specific URL or socket
 * detail outside that protocol.
 */
export function marketFailureText(context: string, error: unknown): string {
  const raw = String(error);
  if (raw.startsWith(ERROR_PREFIX.timeout)) {
    return `${context}：请求超时，请检查网络后重试`;
  }
  if (raw.startsWith(ERROR_PREFIX.unavailable)) {
    return `${context}：市场服务暂时不可用，请稍后重试`;
  }
  if (raw.startsWith(ERROR_PREFIX.invalidResponse)) {
    return `${context}：市场服务返回了无法识别的数据`;
  }
  if (raw.startsWith(ERROR_PREFIX.http)) {
    return `${context}：市场服务返回 HTTP 错误`;
  }
  if (raw.startsWith(ERROR_PREFIX.api)) {
    const detail = raw.slice(ERROR_PREFIX.api.length).trim();
    return detail ? `${context}：${detail}` : `${context}：市场服务返回错误`;
  }
  return `${context}：市场请求失败，请稍后重试`;
}
