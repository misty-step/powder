#!/usr/bin/env node
// powder-ui-toolchain-foundation: orchestrates the full `npm run build`.
// Vite/Rollup only bundles powder-board.css (the one file that needs
// Tailwind's transform). index.html, aesthetic.css (a vendored kit file),
// and powder-board.js (a plain script with zero imports) need no
// transformation at all, so they're copied byte-for-byte -- see
// vite.config.mjs's comment for why running the JS through the bundler is
// actively harmful (it rewrites const/let to var during scope hoisting,
// breaking crates/powder-server/src/tests.rs's literal source assertions).
// Must be idempotent and deterministic: two runs back to back produce
// byte-identical output (the CI job diffs the committed dist against a
// fresh build).
import { build } from "vite";
import { fileURLToPath } from "node:url";
import { copyFileSync } from "node:fs";

const srcDir = fileURLToPath(new URL("./src", import.meta.url));
const outDir = fileURLToPath(
  new URL("../crates/powder-server/static", import.meta.url),
);

await build({ configFile: fileURLToPath(new URL("./vite.config.mjs", import.meta.url)) });

copyFileSync(`${srcDir}/index.html`, `${outDir}/index.html`);
copyFileSync(`${srcDir}/aesthetic.css`, `${outDir}/assets/aesthetic.css`);
copyFileSync(`${srcDir}/powder-board.js`, `${outDir}/assets/powder-board.js`);

console.log("ui build: wrote index.html, assets/aesthetic.css, assets/powder-board.css, assets/powder-board.js");
