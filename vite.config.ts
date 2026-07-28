import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import type { Plugin } from "vite";

const host = process.env["TAURI_DEV_HOST"];

/**
 * Radix Themes ships a responsive variant of every size, spacing, and layout
 * rule, one copy per breakpoint. Those copies are two thirds of its stylesheet
 * and they only ever apply when a component prop is given an object of
 * breakpoint keys — something Skill Manager never does. Stripping them takes a
 * 700 kB stylesheet down to well under 200 kB, which is the difference between
 * a visible pause and an instant first paint on a software-rendered Windows
 * session, where every byte of CSS is parsed before anything appears.
 *
 * The transform is deliberately narrow: a media block is removed only when its
 * condition is exactly one of Radix's breakpoints *and* every selector inside it
 * is a breakpoint-prefixed variant class. Anything else — `@media (hover: hover)`,
 * `prefers-reduced-motion`, this project's own stylesheet — is passed through
 * untouched.
 */
const RADIX_STYLESHEET_DIRECTORY = "@radix-ui/themes";
const BREAKPOINT_CONDITIONS = new Set([520, 768, 1024, 1280, 1640].map((width) => `(min-width: ${String(width)}px)`));
/** Radix prefixes a responsive variant class with its breakpoint and an escaped colon, as in `.xs\:rt-r-mb-3`. */
const RESPONSIVE_VARIANT_PREFIXES = ["xs", "sm", "md", "lg", "xl"].map((breakpoint) => `.${breakpoint}\\:`);
const NEGATED_VARIANT = /:not\([^)]*\.(?:xs|sm|md|lg|xl)\\:/;
const AT_MEDIA = "@media";
const COMMENT_START = "/*";
const COMMENT_END = "*/";

/** The index just past a comment or quoted string starting at `index`, or `index` itself when neither starts there. */
function skipNonCode(css: string, index: number): number {
  if (css.startsWith(COMMENT_START, index)) {
    const end = css.indexOf(COMMENT_END, index + COMMENT_START.length);
    return end === -1 ? css.length : end + COMMENT_END.length;
  }

  const quote = css[index];
  if (quote !== '"' && quote !== "'") {
    return index;
  }

  let cursor = index + 1;
  while (cursor < css.length) {
    if (css[cursor] === "\\") {
      cursor += 2;
      continue;
    }
    if (css[cursor] === quote) {
      return cursor + 1;
    }
    cursor += 1;
  }
  return css.length;
}

function findOpeningBrace(css: string, from: number): number {
  let index = from;
  while (index < css.length) {
    const skipped = skipNonCode(css, index);
    if (skipped !== index) {
      index = skipped;
      continue;
    }
    if (css[index] === "{") {
      return index;
    }
    index += 1;
  }
  return -1;
}

function findClosingBrace(css: string, openingBrace: number): number {
  let depth = 0;
  let index = openingBrace;
  while (index < css.length) {
    const skipped = skipNonCode(css, index);
    if (skipped !== index) {
      index = skipped;
      continue;
    }
    const character = css[index];
    if (character === "{") {
      depth += 1;
    } else if (character === "}") {
      depth -= 1;
      if (depth === 0) {
        return index;
      }
    }
    index += 1;
  }
  return -1;
}

/**
 * True when every selector in the list needs a breakpoint-prefixed variant class
 * to match, so dropping the rule can affect nothing else. Radix writes those
 * classes either on their own (`.xs\:rt-r-mb-3`) or inside a `:where()` beside a
 * component class (`.rt-Section:where(.xs\:rt-r-size-1)`); a negated one would
 * match *without* the variant, so a rule containing one is left in place.
 */
function targetsOnlyResponsiveVariants(selectors: string): boolean {
  const parts = selectors
    .split(",")
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
  return parts.length > 0 && parts.every((part) => RESPONSIVE_VARIANT_PREFIXES.some((prefix) => part.includes(prefix)) && !NEGATED_VARIANT.test(part));
}

/** True when the block holds at least one rule and every one of them targets a breakpoint-prefixed variant class. */
function holdsOnlyResponsiveVariants(body: string): boolean {
  let index = 0;
  let sawRule = false;

  while (index < body.length) {
    const openingBrace = findOpeningBrace(body, index);
    if (openingBrace === -1) {
      return sawRule;
    }
    if (!targetsOnlyResponsiveVariants(body.slice(index, openingBrace))) {
      return false;
    }
    const closingBrace = findClosingBrace(body, openingBrace);
    if (closingBrace === -1) {
      return false;
    }
    sawRule = true;
    index = closingBrace + 1;
  }

  return sawRule;
}

function withoutResponsiveVariants(css: string): string {
  let output = "";
  let index = 0;

  while (index < css.length) {
    const atMedia = css.indexOf(AT_MEDIA, index);
    if (atMedia === -1) {
      break;
    }
    const openingBrace = findOpeningBrace(css, atMedia + AT_MEDIA.length);
    const closingBrace = openingBrace === -1 ? -1 : findClosingBrace(css, openingBrace);
    if (closingBrace === -1) {
      break;
    }

    output += css.slice(index, atMedia);
    const condition = css.slice(atMedia + AT_MEDIA.length, openingBrace).trim();
    const body = css.slice(openingBrace + 1, closingBrace);
    if (!BREAKPOINT_CONDITIONS.has(condition) || !holdsOnlyResponsiveVariants(body)) {
      output += css.slice(atMedia, closingBrace + 1);
    }
    index = closingBrace + 1;
  }

  return output + css.slice(index);
}

function trimRadixResponsiveVariants(): Plugin {
  return {
    name: "trim-radix-responsive-variants",
    enforce: "pre",
    transform(code, id) {
      // Vite hands over ids with platform separators and sometimes a query suffix.
      const [path = ""] = id.replace(/\\/g, "/").split("?");
      if (!path.includes(RADIX_STYLESHEET_DIRECTORY) || !path.endsWith(".css")) {
        return null;
      }
      return { code: withoutResponsiveVariants(code), map: { mappings: "" } };
    }
  };
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), trimRadixResponsiveVariants()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host ?? false,
    ...(host === undefined ? {} : { hmr: { protocol: "ws", host, port: 1421 } }),
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"]
    }
  },
  build: {
    // The webview loads the bundle from the local filesystem, so there is no
    // request to save by splitting it — one script and one stylesheet start
    // parsing immediately instead of waiting on a discovery round trip.
    cssCodeSplit: false,
    // Every Tauri webview supports module preloading natively, so the polyfill
    // is dead weight in the critical path.
    modulePreload: { polyfill: false },
    sourcemap: false,
    // Gzipping the output to print a size nobody reads adds seconds to a build
    // whose artifacts are never served over HTTP.
    reportCompressedSize: false,
    rollupOptions: { output: { entryFileNames: "assets/[name]-[hash].js", assetFileNames: "assets/[name]-[hash][extname]" } }
  },
  esbuild: { legalComments: "none" }
});
