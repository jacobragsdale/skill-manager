// Generates every icon asset from one geometry definition.
//
//   pnpm icons
//
// Writes assets/icon/*.svg (the readable sources), assets/icon/app-icon.png
// (the 1024px master that `tauri icon` expands into the platform set) and
// src-tauri/icons/tray.png (the macOS menu bar template image).

import { Resvg } from "@resvg/resvg-js";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const assets = join(root, "assets", "icon");
const trayOut = join(root, "src-tauri", "icons", "tray.png");

// The mark: three rounded squares (installed skills) and a spark (the live one).
//
// Laid out on a 24 unit grid chosen so the block lands on whole pixels when
// rendered at 16px for the menu bar: origin 1.5u -> 1px, cell 9u -> 6px,
// gap 3u -> 2px. Corner radius stays small so the squares still read as
// squares rather than dots once they are six pixels wide.
const ORIGIN = 1.5;
const CELL = 9;
const GAP = 3;
const RADIUS = 2;
const SPARK_RADIUS = 5;
// How far the bezier controls pull toward the centre. Lower is pointier;
// below about 0.25 the points disappear at 16px.
const SPARK_PINCH = 0.28;

function sparkPath(cx, cy, r, pinch) {
  const c = r * pinch;
  return [`M${cx} ${cy - r}`, `Q${cx + c} ${cy - c} ${cx + r} ${cy}`, `Q${cx + c} ${cy + c} ${cx} ${cy + r}`, `Q${cx - c} ${cy + c} ${cx - r} ${cy}`, `Q${cx - c} ${cy - c} ${cx} ${cy - r}`, "Z"].join(
    " "
  );
}

// The spark takes its own fill so the app icon can highlight it. The tray
// image passes a single colour, since template mode is a silhouette.
function mark(blockFill, sparkFill = blockFill) {
  const second = ORIGIN + CELL + GAP;
  const square = (x, y) => `<rect x="${x}" y="${y}" width="${CELL}" height="${CELL}" rx="${RADIUS}" fill="${blockFill}"/>`;
  return `${square(ORIGIN, ORIGIN)}
    ${square(ORIGIN, second)}
    ${square(second, second)}
    <path d="${sparkPath(second + CELL / 2, ORIGIN + CELL / 2, SPARK_RADIUS, SPARK_PINCH)}" fill="${sparkFill}"/>`;
}

// Full bleed rounded square on a graphite tile. The blocks sit back in muted
// slate so the spark reads as the highlight on both colour and brightness.
const APP_SIZE = 1024;
const APP_CORNER = 224;
const GLYPH_BOX = 560;
const glyphScale = GLYPH_BOX / 24;
const glyphOffset = (APP_SIZE - GLYPH_BOX) / 2;

const TILE_TOP = "#262e35";
const TILE_BOTTOM = "#0d1114";
const BLOCK_COLOR = "#8e9da8";
const SPARK_COLOR = "#3fbcf5";
// A dark icon reads as a hole punched in a dark dock. This rim is too faint to
// register as a border but gives the silhouette an edge.
const RIM_COLOR = "rgba(255,255,255,0.10)";
const RIM_WIDTH = 3;

const appIconSvg = `<svg xmlns="http://www.w3.org/2000/svg" width="${APP_SIZE}" height="${APP_SIZE}" viewBox="0 0 ${APP_SIZE} ${APP_SIZE}">
  <defs>
    <linearGradient id="tile" x1="0" y1="0" x2="0.45" y2="1">
      <stop offset="0" stop-color="${TILE_TOP}"/>
      <stop offset="1" stop-color="${TILE_BOTTOM}"/>
    </linearGradient>
  </defs>
  <rect width="${APP_SIZE}" height="${APP_SIZE}" rx="${APP_CORNER}" fill="url(#tile)"/>
  <rect x="${RIM_WIDTH / 2}" y="${RIM_WIDTH / 2}" width="${APP_SIZE - RIM_WIDTH}" height="${APP_SIZE - RIM_WIDTH}" rx="${APP_CORNER - RIM_WIDTH / 2}" fill="none" stroke="${RIM_COLOR}" stroke-width="${RIM_WIDTH}"/>
  <g transform="translate(${glyphOffset} ${glyphOffset}) scale(${glyphScale})">
    ${mark(BLOCK_COLOR, SPARK_COLOR)}
  </g>
</svg>`;

// The tray image is a silhouette: macOS template mode throws away colour and
// keeps only the alpha channel. The 28 unit box insets the mark so it does not
// crowd the menu bar.
const TRAY_BOX = 28;
const TRAY_PX = 44;
const trayInset = (TRAY_BOX - 24) / 2;

const traySvg = `<svg xmlns="http://www.w3.org/2000/svg" width="${TRAY_BOX}" height="${TRAY_BOX}" viewBox="0 0 ${TRAY_BOX} ${TRAY_BOX}">
  <g transform="translate(${trayInset} ${trayInset})">
    ${mark("#000000")}
  </g>
</svg>`;

function png(svg, width) {
  return new Resvg(svg, { fitTo: { mode: "width", value: width } }).render().asPng();
}

mkdirSync(assets, { recursive: true });
writeFileSync(join(assets, "app-icon.svg"), appIconSvg);
writeFileSync(join(assets, "tray.svg"), traySvg);
writeFileSync(join(assets, "app-icon.png"), png(appIconSvg, APP_SIZE));
writeFileSync(trayOut, png(traySvg, TRAY_PX));

console.log(`assets/icon/app-icon.png (${APP_SIZE}px)`);
console.log(`src-tauri/icons/tray.png (${TRAY_PX}px template)`);
console.log("Now run: pnpm tauri icon assets/icon/app-icon.png");
