const LEVELS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

export function nextThinkingLevel(current, direction) {
  const index = LEVELS.indexOf(current);
  if (index < 0) throw new Error(`unknown Pi thinking level: ${current}`);
  const delta = direction === "raise" ? 1 : direction === "lower" ? -1 : 0;
  if (delta === 0) throw new Error(`direction must be raise or lower`);
  return LEVELS[Math.max(0, Math.min(LEVELS.length - 1, index + delta))];
}

export default function herdrEffort(pi) {
  for (const [shortcut, direction] of [
    ["ctrl+shift+right", "raise"],
    ["ctrl+shift+left", "lower"],
  ]) {
    pi.registerShortcut(shortcut, {
      description: `${direction === "raise" ? "Raise" : "Lower"} thinking effort`,
      handler: async (ctx) => {
        try {
          const current = pi.getThinkingLevel();
          pi.setThinkingLevel(nextThinkingLevel(current, direction));
          ctx.ui.notify(
            `Thinking: ${current} → ${pi.getThinkingLevel()}`,
            "info",
          );
        } catch (error) {
          ctx.ui.notify(
            error instanceof Error ? error.message : String(error),
            "error",
          );
        }
      },
    });
  }
}
