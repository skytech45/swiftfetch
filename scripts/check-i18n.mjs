// Missing-key check for locales (M5). Fails when any non-English locale
// drifts from apps/desktop/src/locales/en.json.
import { readdirSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "apps", "desktop", "src", "locales");

function flatten(obj, prefix = "") {
  const out = new Map();
  for (const [k, v] of Object.entries(obj)) {
    const path = prefix ? `${prefix}.${k}` : k;
    if (v !== null && typeof v === "object") {
      for (const [sk, sv] of flatten(v, path)) out.set(sk, sv);
    } else {
      out.set(path, v);
    }
  }
  return out;
}

const en = JSON.parse(readFileSync(join(root, "en.json"), "utf8"));
const ref = flatten(en);
let failed = false;

for (const file of readdirSync(root)) {
  if (file === "en.json" || !file.endsWith(".json")) continue;
  const locale = JSON.parse(readFileSync(join(root, file), "utf8"));
  const keys = flatten(locale);
  for (const key of ref.keys()) {
    if (!keys.has(key)) {
      console.error(`${file}: missing key ${key}`);
      failed = true;
    } else {
      const v = keys.get(key);
      if (typeof v !== "string" || v.length === 0) {
        console.error(`${file}: empty value for ${key}`);
        failed = true;
      }
    }
  }
  for (const key of keys.keys()) {
    if (!ref.has(key)) {
      console.error(`${file}: extra key ${key} (not in en.json)`);
      failed = true;
    }
  }
}

if (failed) {
  console.error("i18n check FAILED");
  process.exit(1);
}
console.log(`i18n check OK (${ref.size} keys, locales in sync)`);
