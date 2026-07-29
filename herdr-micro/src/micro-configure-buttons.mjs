import { execFile } from "node:child_process";
import { promisify } from "node:util";
import {
  buttonConfigPath,
  ensureButtonConfig,
} from "./button-config.mjs";

const file = ensureButtonConfig(buttonConfigPath());
await promisify(execFile)("/usr/bin/open", ["-t", file]);
console.log(file);
