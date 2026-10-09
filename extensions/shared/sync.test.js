// Guards the single-source-of-truth rule: packaged copies in chrome/ and
// firefox/ must match extensions/shared/ (run scripts/sync-extensions.mjs
// after editing shared sources).
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const SHARED = ["sniff.js", "popup.js", "background.js", "youtube.js", "popup.html", "popup-ui.js"];

describe("packaged copies match shared sources", () => {
  for (const target of ["chrome", "firefox"]) {
    for (const file of SHARED) {
      it(`${target}/${file} is in sync`, () => {
        const a = readFileSync(join(root, "shared", file), "utf8");
        const b = readFileSync(join(root, target, file), "utf8");
        expect(b).toBe(a);
      });
    }
  }
});
