import { execFile } from "node:child_process";
import { promisify } from "node:util";
import {
  controlConfigPath,
  ensureControlConfig,
} from "./control-config.mjs";

const file = ensureControlConfig(controlConfigPath());
await promisify(execFile)("/usr/bin/open", ["-t", file]);
console.log(file);
