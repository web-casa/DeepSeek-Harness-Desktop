// Generate the app icon source PNG (1024×1024) with zero dependencies:
// hand-rolled PNG encoder (zlib.deflateSync + CRC32).
//
// Design: dark rounded square, DeepSeek-blue ring + core disc, one satellite
// dot — a minimal "harness orbit" mark. Run `pnpm icons` afterwards to derive
// the full platform set (ico/icns/pngs) via `tauri icon`.

import { deflateSync } from "node:zlib";
import { writeFileSync, mkdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const SIZE = 1024;
const RADIUS = 220; // rounded corner radius

const BG: [number, number, number] = [0x0d, 0x11, 0x17];
const BLUE: [number, number, number] = [0x4d, 0x6b, 0xfe];
const WHITE: [number, number, number] = [0xe8, 0xeb, 0xf2];

// --- minimal PNG encoder -------------------------------------------------
const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(buf: Buffer): number {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type: string, data: Buffer): Buffer {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const typeBuf = Buffer.from(type, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])));
  return Buffer.concat([len, typeBuf, data, crc]);
}

function encodePng(rgba: Buffer, size: number): Buffer {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // color type RGBA
  const raw = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0; // filter: none
    rgba.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

// --- drawing --------------------------------------------------------------
function inRoundedRect(x: number, y: number): boolean {
  const cx = Math.min(Math.max(x, RADIUS), SIZE - 1 - RADIUS);
  const cy = Math.min(Math.max(y, RADIUS), SIZE - 1 - RADIUS);
  const dx = x - cx;
  const dy = y - cy;
  return dx * dx + dy * dy <= RADIUS * RADIUS;
}

const cx0 = SIZE / 2 - 0.5;
const cy0 = SIZE / 2 - 0.5;
const RING_OUT = 300;
const RING_IN = 236;
const CORE = 104;
const SAT_R = 36;
const SAT_A = Math.PI / 4; // 45°
const SAT_X = cx0 + 268 * Math.cos(SAT_A);
const SAT_Y = cy0 - 268 * Math.sin(SAT_A);

function colorAt(px: number, py: number): [number, number, number, number] {
  if (!inRoundedRect(px, py)) return [0, 0, 0, 0];
  const d = Math.hypot(px - cx0, py - cy0);
  if (Math.hypot(px - SAT_X, py - SAT_Y) <= SAT_R) return [...WHITE, 255];
  if (d <= CORE) return [...BLUE, 255];
  if (d >= RING_IN && d <= RING_OUT) return [...BLUE, 255];
  return [...BG, 255];
}

const rgba = Buffer.alloc(SIZE * SIZE * 4);
for (let y = 0; y < SIZE; y++) {
  for (let x = 0; x < SIZE; x++) {
    // 2×2 supersampling for smooth edges.
    let r = 0;
    let g = 0;
    let b = 0;
    let a = 0;
    for (const [ox, oy] of [
      [0.25, 0.25],
      [0.75, 0.25],
      [0.25, 0.75],
      [0.75, 0.75],
    ]) {
      const [cr, cg, cb, ca] = colorAt(x + ox, y + oy);
      r += cr;
      g += cg;
      b += cb;
      a += ca;
    }
    const i = (y * SIZE + x) * 4;
    rgba[i] = Math.round(r / 4);
    rgba[i + 1] = Math.round(g / 4);
    rgba[i + 2] = Math.round(b / 4);
    rgba[i + 3] = Math.round(a / 4);
  }
}

const outPath = join(dirname(fileURLToPath(import.meta.url)), "..", "src-tauri", "icons", "icon-source.png");
mkdirSync(dirname(outPath), { recursive: true });
writeFileSync(outPath, encodePng(rgba, SIZE));
console.log(`✓ icon source written → ${outPath}`);

// ---------------------------------------------------------------------------
// Tray template icon (32px, monochrome black ring, transparent background).
// macOS uses template images (auto light/dark); Windows/Linux tint per theme.
// ---------------------------------------------------------------------------
function genTrayTemplate(): void {
  const S = 32;
  const cx = S / 2 - 0.5;
  const cy = S / 2 - 0.5;
  const R_OUT = 13;
  const R_IN = 10;
  const CORE = 4.5;
  const rgba = Buffer.alloc(S * S * 4);
  for (let y = 0; y < S; y++) {
    for (let x = 0; x < S; x++) {
      const d = Math.hypot(x - cx, y - cy);
      const hit = d <= CORE || (d >= R_IN && d <= R_OUT);
      const i = (y * S + x) * 4;
      // 2×2 supersample for smooth edges.
      let a = 0;
      for (const [ox, oy] of [
        [0.25, 0.25],
        [0.75, 0.25],
        [0.25, 0.75],
        [0.75, 0.75],
      ]) {
        const dd = Math.hypot(x + ox - cx, y + oy - cy);
        if (dd <= CORE || (dd >= R_IN && dd <= R_OUT)) a += 255;
      }
      rgba[i] = 0;
      rgba[i + 1] = 0;
      rgba[i + 2] = 0;
      rgba[i + 3] = Math.round(a / 4);
    }
  }
  const outPath = join(dirname(fileURLToPath(import.meta.url)), "..", "src-tauri", "icons", "tray-template.png");
  writeFileSync(outPath, encodePng(rgba, S));
  console.log(`✓ tray template icon written → ${outPath}`);
}

genTrayTemplate();
