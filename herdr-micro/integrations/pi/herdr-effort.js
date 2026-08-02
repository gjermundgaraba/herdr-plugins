const LEVELS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

export function nextThinkingLevel(current, delta) {
  const index = LEVELS.indexOf(current);
  if (index < 0) throw new Error(`unknown Pi thinking level: ${current}`);
  return LEVELS[Math.max(0, Math.min(LEVELS.length - 1, index + delta))];
}

export default function herdrEffort(pi) {
  for (const [shortcut, delta, label] of [
    ["ctrl+shift+right", 1, "Raise"],
    ["ctrl+shift+left", -1, "Lower"],
  ]) {
    pi.registerShortcut(shortcut, {
      description: `${label} thinking effort`,
      handler: async (ctx) => {
        try {
          const current = pi.getThinkingLevel();
          pi.setThinkingLevel(nextThinkingLevel(current, delta));
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
