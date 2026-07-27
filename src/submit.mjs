export function submitArgs(agent) {
  if (!agent.pane_id) throw new Error("focused agent has no pane");
  return ["agent", "send-keys", agent.pane_id, "enter"];
}
