#!/usr/bin/env node

import { chmod, open, readFile, rename, stat } from "node:fs/promises";

const [storagePath, targetArg, mode = "--dry-run"] = process.argv.slice(2);
const targetIndex = Number(targetArg) - 1;

if (!storagePath || !Number.isInteger(targetIndex) || targetIndex < 1) {
  throw new Error("usage: node clone-oai-layout.mjs INPUT_STORAGE TARGET_LAYER [--apply]");
}

const raw = await readFile(storagePath, "utf8");
const root = JSON.parse(raw);
const matches = [];

for (const collection of root.collections ?? []) {
  for (const device of collection.data ?? []) {
    for (const profile of device.profiles ?? []) {
      const source = profile.layers?.[0];
      const keycodes = JSON.stringify(source?.layout ?? {});
      if (keycodes.includes("KV_OAI_AG00") && keycodes.includes("KV_OAI_AG05")) {
        matches.push({ profile, source });
      }
    }
  }
}

if (matches.length !== 1) throw new Error(`expected one OAI profile, found ${matches.length}`);

const { profile, source } = matches[0];
const target = profile.layers?.[targetIndex];
if (!target) throw new Error(`Layer ${targetIndex + 1} does not exist`);
if (JSON.stringify(target.layout).includes("KV_OAI_")) {
  throw new Error(`Layer ${targetIndex + 1} already contains OAI keycodes`);
}

target.layout = structuredClone(source.layout);

const oai = JSON.stringify(source.layout).match(/\bKV_OAI_[A-Z0-9_]+\b/g)?.length ?? 0;
console.log(`source=Layer 1 target=Layer ${targetIndex + 1} oai=${oai} lights=${Object.hasOwn(target, "lights")}`);
if (mode !== "--apply") process.exit(0);

const info = await stat(storagePath);
const temporaryPath = `${storagePath}.codex-oai-clone.tmp`;
const output = await open(temporaryPath, "wx", info.mode);

try {
  await output.writeFile(JSON.stringify(root));
  await output.sync();
} finally {
  await output.close();
}

await chmod(temporaryPath, info.mode);
await rename(temporaryPath, storagePath);
console.log("applied");
