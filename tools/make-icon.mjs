/**
 * Draws the application icon.
 *
 * Run with `node tools/make-icon.mjs <out.png>`, then feed the result to
 * `tauri icon` to produce the platform set.
 *
 * This exists as code rather than as a checked-in binary so the mark is
 * reproducible and reviewable — a PNG in the repository is a black box, and
 * the first thing anyone does with a placeholder is tweak it. Replace this
 * with the real artwork when there is any; the shape below is a placeholder
 * that reads correctly at 16 px, which is the only size that can go badly
 * wrong.
 */

import { deflateSync } from "node:zlib";
import { writeFileSync } from "node:fs";

const SIZE = 1024;
/** Samples per axis. The shapes are all curves, so edges need the help. */
const SAMPLES = 4;

// Matches --accent in ui/src/lib/tokens.css. Duplicated deliberately: the icon
// is generated ahead of any build that could import the token.
const PLATE = [0x2a, 0x6b, 0xf2];
const MARK = [0xff, 0xff, 0xff];

/** Signed distance to a rounded rectangle, negative inside. */
function roundedBox(px, py, halfW, halfH, radius) {
  const dx = Math.abs(px) - (halfW - radius);
  const dy = Math.abs(py) - (halfH - radius);
  const outside = Math.hypot(Math.max(dx, 0), Math.max(dy, 0));
  return outside + Math.min(Math.max(dx, dy), 0) - radius;
}

/** Signed distance to a vertical capsule — the shape each bar is drawn from. */
function capsule(px, py, cx, halfHeight, radius) {
  // Distance to the segment from (cx, -halfHeight) to (cx, +halfHeight), less
  // the radius. Not written in the rounded-box form used above, tempting as it
  // is: a capsule is a box of zero width, and dropping the width term leaves
  // |dx| + |dy|, which is a diamond. That mistake is invisible in the algebra
  // and obvious only on screen.
  const dx = px - cx;
  const dy = Math.max(Math.abs(py) - halfHeight, 0);
  return Math.hypot(dx, dy) - radius;
}

/**
 * Coverage of a shape at a point, from a signed distance.
 *
 * Distances are in normalised units, where 1 is the whole canvas. The ramp is
 * one sample wide either side of the edge; together with the supersampling
 * below, that is what keeps a curve from looking stepped at 16 px.
 */
const EDGE = 1 / (SIZE * SAMPLES);

function coverage(sd) {
  return Math.min(Math.max(0.5 - sd / EDGE, 0), 1);
}

// The mark: three bars of different heights, the middle one tallest. A sound
// level, drawn with the fewest strokes that still say "audio" at 16 px.
const BARS = [
  { cx: -0.19, half: 0.13 },
  { cx: 0.0, half: 0.235 },
  { cx: 0.19, half: 0.17 },
];
const BAR_RADIUS = 0.05;
const PLATE_HALF = 0.46;
const PLATE_RADIUS = 0.21;

/** `i` and `j` are pixel indices. Everything inside is in normalised units. */
function pixel(i, j) {
  const step = 1 / SIZE;
  let plate = 0;
  let mark = 0;

  for (let sy = 0; sy < SAMPLES; sy++) {
    for (let sx = 0; sx < SAMPLES; sx++) {
      const px = (i + (sx + 0.5) / SAMPLES) * step - 0.5;
      const py = (j + (sy + 0.5) / SAMPLES) * step - 0.5;

      plate += coverage(roundedBox(px, py, PLATE_HALF, PLATE_HALF, PLATE_RADIUS));

      let bar = 0;
      for (const { cx, half } of BARS) {
        bar = Math.max(bar, coverage(capsule(px, py, cx, half, BAR_RADIUS)));
      }
      mark += bar;
    }
  }

  const total = SAMPLES * SAMPLES;
  plate /= total;
  mark /= total;

  // White over blue, composited before the alpha channel is folded in.
  const colour = [0, 1, 2].map((i) => MARK[i] * mark + PLATE[i] * (1 - mark));
  return [colour[0], colour[1], colour[2], plate * 255];
}

// ---- PNG encoding ----------------------------------------------------------

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(bytes) {
  let c = 0xffffffff;
  for (const byte of bytes) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const out = Buffer.alloc(data.length + 12);
  out.writeUInt32BE(data.length, 0);
  out.write(type, 4, "ascii");
  Buffer.from(data).copy(out, 8);
  out.writeUInt32BE(crc32(out.subarray(4, 8 + data.length)), 8 + data.length);
  return out;
}

function encodePng(width, height, pixels) {
  // Each scanline is prefixed with its filter type; 0 means "none".
  const raw = Buffer.alloc(height * (1 + width * 4));
  for (let y = 0; y < height; y++) {
    const row = y * (1 + width * 4);
    raw[row] = 0;
    pixels.copy(raw, row + 1, y * width * 4, (y + 1) * width * 4);
  }

  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // colour type: RGBA
  // 10..12 stay zero: deflate, adaptive filtering, no interlace.

  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

// ---- main ------------------------------------------------------------------

const output = process.argv[2];
if (!output) {
  console.error("usage: node tools/make-icon.mjs <out.png>");
  process.exit(1);
}

const pixels = Buffer.alloc(SIZE * SIZE * 4);
for (let y = 0; y < SIZE; y++) {
  for (let x = 0; x < SIZE; x++) {
    const [r, g, b, a] = pixel(x, y);
    pixels.writeUInt8(r, (y * SIZE + x) * 4);
    pixels.writeUInt8(g, (y * SIZE + x) * 4 + 1);
    pixels.writeUInt8(b, (y * SIZE + x) * 4 + 2);
    pixels.writeUInt8(a, (y * SIZE + x) * 4 + 3);
  }
}

writeFileSync(output, encodePng(SIZE, SIZE, pixels));
console.log(`wrote ${output} (${SIZE}x${SIZE})`);
