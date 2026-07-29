const BODY = "Review scope: uncomitted changes";

const PROMPTS = {
  codex: `$deslop $ponytail:ponytail-review\n${BODY}`,
  claude: `/deslop /ponytail:ponytail-review\n${BODY}`,
  pi: `/skill:deslop /skill:ponytail:ponytail-review\n${BODY}`,
};

export function reviewPrompt(agent) {
  const prompt = PROMPTS[agent];
  if (!prompt) throw new Error(`unsupported focused agent: ${agent || "none"}`);
  return prompt;
}
