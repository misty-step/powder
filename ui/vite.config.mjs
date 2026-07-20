import { defineConfig } from "vite";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

// powder-ui-toolchain-foundation: this build has exactly one job -- turn
// ui/src into the four files crates/powder-server/src/main.rs embeds with
// include_str! (index.html, assets/aesthetic.css, assets/powder-board.css,
// assets/powder-board.js). Filenames MUST stay stable: the deploy-SHA ETag
// in main.rs::static_asset already busts caches on every deploy, so a
// content hash in the filename would be redundant *and* would break the
// hardcoded include_str! paths.
//
// Only powder-board.css runs through Vite's real bundler, because it's the
// only file that needs transformation (Tailwind's CSS-first pipeline via
// @tailwindcss/vite). index.html, aesthetic.css (a vendored kit file
// scripts/check-aesthetic-currency.sh expects byte-for-byte), and
// powder-board.js (plain script, zero imports, nothing to resolve) are
// copied verbatim by build.mjs instead of round-tripped through the
// bundler's module graph: bundling a single-module JS entry still forces
// Rolldown/Rollup's scope-hoisting code generator to rewrite top-level
// `const`/`let` into `var`, which silently breaks the literal
// `.contains("const RAW_STATUSES")`-style assertions in
// crates/powder-server/src/tests.rs. When a later card adds real
// TS/JSX/imports to the JS, move it into this same rollupOptions.input
// graph.
const root = fileURLToPath(new URL("./src", import.meta.url));
const outDir = fileURLToPath(
  new URL("../crates/powder-server/static", import.meta.url),
);

export default defineConfig({
  root,
  plugins: [tailwindcss()],
  build: {
    outDir,
    emptyOutDir: true,
    minify: false,
    cssMinify: false,
    sourcemap: false,
    rollupOptions: {
      input: {
        "powder-board": `${root}/powder-board.css`,
      },
      output: {
        assetFileNames: "assets/powder-board.css",
      },
    },
  },
});
