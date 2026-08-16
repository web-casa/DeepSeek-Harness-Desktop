// Fake-harness simulators and the readiness-line contract for
// verify-heartbeat.ts. Separated so unit tests can import them without
// starting real sidecars.

/// A fake harness that prints the readiness line, listens, but never answers
/// HTTP — the "alive but event loop blocked" shape the heartbeat must catch
/// (the kernel completes the TCP handshake, no response bytes are written).
export const HANG_SCRIPT = `
const net = require("node:net");
const srv = net.createServer(() => {});
srv.listen(0, "127.0.0.1", () => {
  console.log("dsh web: http://127.0.0.1:" + srv.address().port);
});
setInterval(() => {}, 1000);
`;

/// A fake harness that answers every probe — the healthy negative case.
export const HEALTHY_SCRIPT = `
const http = require("node:http");
const srv = http.createServer((req, res) => {
  res.writeHead(200, { "Content-Type": "text/html" });
  res.end("<!doctype html><html><body>ok</body></html>");
});
srv.listen(0, "127.0.0.1", () => {
  console.log("dsh web: http://127.0.0.1:" + srv.address().port);
});
`;

/// The strictest printable form — what the simulators print. The sidecar's
/// extract_local_url additionally accepts prefixed lines and the
/// " (LAN: …)" suffix; the simulators stay conservative on purpose.
export function isParseableReadyLine(line: string): boolean {
  const m = /^dsh web: http:\/\/127\.0\.0\.1:(\d+)$/.exec(line);
  if (!m) return false;
  const port = Number(m[1]);
  return port >= 1 && port <= 65535;
}
