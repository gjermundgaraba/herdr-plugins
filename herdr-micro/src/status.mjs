import { spawnSync } from "node:child_process";

const result = spawnSync(process.env.HERDR_BIN_PATH ?? "herdr", ["status"], {
  stdio: "inherit",
});

process.exitCode = result.status ?? 1;
