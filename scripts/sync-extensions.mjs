// Copies the canonical extension sources (extensions/shared/) into the
// per-browser packages (extensions/chrome, extensions/firefox) so the
// shipped extensions always carry the tested single source of truth.
// Run: `node scripts/sync-extensions.mjs` (also enforced by the vitest
// suite — packaged copies must match shared/).
import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "extensions");
const SHARED = ["sniff.js", "popup.js", "background.js", "youtube.js", "popup.html", "popup-ui.js"];

for (const target of ["chrome", "firefox"]) {
  for (const file of SHARED) {
    const src = join(root, "shared", file);
    if (!existsSync(src)) {
      console.error(`missing canonical source: extensions/shared/${file}`);
      process.exit(1);
    }
    mkdirSync(join(root, target), { recursive: true });
    copyFileSync(src, join(root, target, file));
    console.log(`synced ${target}/${file}`);
  }
}
