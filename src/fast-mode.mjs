export function fastModePlan(agent) {
  if (!agent.pane_id) throw new Error("focused agent has no pane");
  if (!["codex", "pi"].includes(agent.agent)) {
    throw new Error(`unsupported focused agent: ${agent.agent || "none"}`);
  }
  if (agent.agent === "codex") {
    return [["agent", "prompt", agent.pane_id, "/fast"]];
  }
  return [
    ["pane", "send-text", agent.pane_id, "/fast"],
    ["agent", "send-keys", agent.pane_id, "enter"],
  ];
}
