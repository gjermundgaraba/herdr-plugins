import fs from "node:fs/promises";
import { constants } from "node:fs";
import path from "node:path";

export async function installPiEffort(source, target, timestamp = Date.now()) {
  const bundled = await fs.readFile(source);
  const current = await fs.readFile(target).catch((error) => {
    if (error.code === "ENOENT") return null;
    throw error;
  });
  if (current?.equals(bundled)) return { target, unchanged: true };

  await fs.mkdir(path.dirname(target), { recursive: true });
  const backup = current ? `${target}.bak-${timestamp}` : null;
  if (backup) await fs.copyFile(target, backup, constants.COPYFILE_EXCL);
  await fs.writeFile(target, bundled, { mode: 0o644 });
  return { target, backup };
}
