// Stages browser integration assets for the installer:
//   apps/desktop/src-tauri/browser-assets/
//     swiftfetch-native-host(.exe)  — built by cargo (dev profile ok for
//                                     local installers; release for ships)
//     extensions/*.zip              — store/sideload packages
// Run: `node scripts/package-browser-assets.mjs [--release]`
// (tauri.conf.json bundles browser-assets/ as resources.)
import { execFileSync } from "node:child_process";
import {
  copyFileSync,
  createWriteStream,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const release = process.argv.includes("--release");
const profile = release ? "release" : "debug";
const exeName = process.platform === "win32" ? "swiftfetch-native-host.exe" : "swiftfetch-native-host";

const outDir = join(root, "apps", "desktop", "src-tauri", "browser-assets");
const extDir = join(outDir, "extensions");
mkdirSync(extDir, { recursive: true });

// 1. Native-host binary.
console.log(`building native host (${profile})…`);
execFileSync("cargo", ["build", ...(release ? ["--release"] : []), "-p", "swiftfetch-native-host"], {
  cwd: root,
  stdio: "inherit",
});
const built = join(root, "target", profile, exeName);
if (!existsSync(built)) {
  console.error(`missing native-host binary: ${built}`);
  process.exit(1);
}
copyFileSync(built, join(outDir, exeName));
console.log(`staged ${exeName}`);

// 2. Extension zips (store submission + sideload payloads).
const version = JSON.parse(readFileSync(join(root, "apps", "desktop", "package.json"), "utf8")).version;
for (const browser of ["chrome", "firefox"]) {
  const zip = join(extDir, `swiftfetch-${browser}-${version}.zip`);
  if (existsSync(zip)) rmSync(zip);
  zipDir(join(root, "extensions", browser), zip);
  console.log(`staged ${zip}`);
}

// 3. Manifest for the app (what got staged).
writeFileSync(
  join(outDir, "manifest.json"),
  JSON.stringify({ version, profile, stagedAt: new Date().toISOString() }, null, 2),
);
console.log("browser assets ready:", outDir);
void createWriteStream;

/** Minimal stored-zip writer (no dependency — extension files are small). */
function zipDir(srcDir, zipPath) {
  const entries = [];
  const walk = (dir, base) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.name.startsWith(".")) continue;
      if (entry.name === ".gitkeep") continue;
      const full = join(dir, entry.name);
      const rel = base ? `${base}/${entry.name}` : entry.name;
      if (entry.isDirectory()) walk(full, rel);
      else entries.push({ rel, data: readFileSync(full) });
    }
  };
  walk(srcDir, "");

  const chunks = [];
  const central = [];
  let offset = 0;
  const crcTable = makeCrcTable();
  for (const { rel, data } of entries) {
    const name = Buffer.from(rel);
    const crc = crc32(data, crcTable);
    const header = Buffer.alloc(30);
    header.writeUInt32LE(0x04034b50, 0);
    header.writeUInt16LE(20, 4);
    header.writeUInt16LE(0, 6);
    header.writeUInt16LE(0, 8); // stored
    header.writeUInt16LE(0, 10);
    header.writeUInt16LE(0, 12);
    header.writeUInt32LE(crc >>> 0, 14);
    header.writeUInt32LE(data.length, 18);
    header.writeUInt32LE(data.length, 22);
    header.writeUInt16LE(name.length, 26);
    header.writeUInt16LE(0, 28);
    chunks.push(header, name, data);
    central.push({ name, crc, size: data.length, offset });
    offset += 30 + name.length + data.length;
  }
  const centralStart = offset;
  let centralSize = 0;
  for (const { name, crc, size, offset: off } of central) {
    const header = Buffer.alloc(46);
    header.writeUInt32LE(0x02014b50, 0);
    header.writeUInt16LE(20, 4);
    header.writeUInt16LE(20, 6);
    header.writeUInt16LE(0, 8);
    header.writeUInt16LE(0, 10);
    header.writeUInt16LE(0, 12);
    header.writeUInt16LE(0, 14);
    header.writeUInt32LE(crc >>> 0, 16);
    header.writeUInt32LE(size, 20);
    header.writeUInt32LE(size, 24);
    header.writeUInt16LE(name.length, 28);
    header.writeUInt16LE(0, 30);
    header.writeUInt16LE(0, 32);
    header.writeUInt16LE(0, 34);
    header.writeUInt16LE(0, 36);
    header.writeUInt32LE(0, 38);
    header.writeUInt32LE(off, 42);
    chunks.push(header, name);
    centralSize += 46 + name.length;
  }
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(0, 4);
  end.writeUInt16LE(0, 6);
  end.writeUInt16LE(central.length, 8);
  end.writeUInt16LE(central.length, 10);
  end.writeUInt32LE(centralSize, 12);
  end.writeUInt32LE(centralStart, 16);
  end.writeUInt16LE(0, 20);
  chunks.push(end);
  writeFileSync(zipPath, Buffer.concat(chunks));
}

function makeCrcTable() {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c;
  }
  return table;
}

function crc32(buf, table) {
  let crc = 0xffffffff;
  for (const b of buf) crc = table[(crc ^ b) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}
