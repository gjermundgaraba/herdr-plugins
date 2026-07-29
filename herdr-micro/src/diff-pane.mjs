const PLUGIN_ID = "gjermundgaraba.herdr-micro";

export function diffPaneArgs(agent) {
  if (!agent.cwd) {
    throw new Error("focused agent has no repository context");
  }
  return [
    "plugin",
    "pane",
    "open",
    "--plugin",
    PLUGIN_ID,
    "--entrypoint",
    "diff",
    "--cwd",
    agent.cwd,
    "--focus",
  ];
}
