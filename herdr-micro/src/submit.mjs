export function promptArgs(action, agent) {
  if (!agent.pane_id) throw new Error("focused agent has no pane");
  return action.submit === false
    ? ["pane", "send-text", agent.pane_id, action.prompt]
    : ["agent", "prompt", agent.pane_id, action.prompt];
}

export function submitArgs(agent) {
  if (!agent.pane_id) throw new Error("focused agent has no pane");
  return ["agent", "send-keys", agent.pane_id, "enter"];
}
