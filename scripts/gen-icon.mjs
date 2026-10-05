// Generates the placeholder SwiftFetch app icon (scripts/assets/icon-source.png):
// a rounded gradient tile with an original bolt mark. Run: node scripts/gen-icon.mjs
import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const OUT = resolve(ROOT, "scripts", "assets", "icon-source.png");
const SIZE = 1024;
const RADIUS = 200;

const bolt = [
  [560, 120],
  [300, 560],
  [480, 560],
  [420, 904],
  [740, 440],
  [540, 440],
  [660, 120],
];

function inBolt(x, y) {
  let inside = false;
  for (let i = 0, j = bolt.length - 1; i < bolt.length; j = i++) {
    const [xi, yi] = bolt[i];
    const [xj, yj] = bolt[j];
    if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) {
      inside = !inside;
    }
  }
  return inside;
}

const px = new Uint8Array(SIZE * SIZE * 4);
const from = [0x25, 0x63, 0xeb];
const to = [0x7c, 0x3a, 0xed];

for (let y = 0; y < SIZE; y++) {
  const t = y / (SIZE - 1);
  const r = Math.round(from[0] + (to[0] - from[0]) * t);
  const g = Math.round(from[1] + (to[1] - from[1]) * t);
  const b = Math.round(from[2] + (to[2] - from[2]) * t);
  for (let x = 0; x < SIZE; x++) {
    const dx = Math.max(RADIUS - x, x - (SIZE - 1 - RADIUS), 0);
    const dy = Math.max(RADIUS - y, y - (SIZE - 1 - RADIUS), 0);
    const inside = dx * dx + dy * dy <= RADIUS * RADIUS;
    const i = (y * SIZE + x) * 4;
    if (!inside) {
      continue; // transparent corners stay zeroed
    }
    if (inBolt(x, y)) {
      px[i] = 255;
      px[i + 1] = 255;
      px[i + 2] = 255;
    } else {
      px[i] = r;
      px[i + 1] = g;
      px[i + 2] = b;
    }
    px[i + 3] = 255;
  }
}

let crcTable;
function crc32(bytes) {
  if (!crcTable) {
    crcTable = new Int32Array(256);
    for (let n = 0; n < 256; n++) {
      let c = n;
      for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      crcTable[n] = c;
    }
  }
  let crc = -1;
  for (const byte of bytes) crc = (crc >>> 8) ^ crcTable[(crc ^ byte) & 0xff];
  return (crc ^ -1) >>> 0;
}

function chunk(type, data) {
  const out = Buffer.alloc(8 + data.length + 4);
  out.writeUInt32BE(data.length, 0);
  out.write(type, 4, "ascii");
  data.copy(out, 8);
  out.writeUInt32BE(crc32(out.subarray(4, 8 + data.length)), 8 + data.length);
  return out;
}

const stride = SIZE * 4;
const raw = Buffer.alloc((stride + 1) * SIZE);
for (let y = 0; y < SIZE; y++) {
  raw[y * (stride + 1)] = 0; // filter type: none
  raw.set(px.subarray(y * stride, (y + 1) * stride), y * (stride + 1) + 1);
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // color type: RGBA

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);

mkdirSync(dirname(OUT), { recursive: true });
writeFileSync(OUT, png);
console.log(`wrote ${OUT} (${png.length} bytes)`);
