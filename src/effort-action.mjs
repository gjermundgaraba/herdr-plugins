import { changeEffort } from "./effort.mjs";

const direction = process.argv[2];
const context = JSON.parse(process.env.HERDR_PLUGIN_CONTEXT_JSON ?? "{}");

const result = await changeEffort({
  herdrBin: process.env.HERDR_BIN_PATH ?? "herdr",
  agent: context.focused_pane_agent,
  direction,
  paneId: context.focused_pane_id,
});

console.log(JSON.stringify(result));
