# Future Micro investigations

The current design keeps raw USB ownership: Herdr captures the Micro on Layer 2
so ChatGPT cannot receive the same reports, then restores native HID ownership
for ChatGPT on Layer 1. The items below are evidence-gated follow-ups, not
planned features.

## Physical results — 2026-08-04

- **Helper death:** with the user daemon frozen, `SIGKILL` left the Micro
  captured and absent from `hidutil` after five seconds. A fresh helper could
  recapture it, and a controlled close restored both native HID services in
  100 ms without unplugging. With helper build 2, the daemon completed that
  recovery cycle 1.09 s after a second forced helper death while ChatGPT was
  frontmost; status cleared the error, and both native HID services were
  verified. No unplug was required.
- **Focus switching:** 25 ChatGPT/Ghostty round trips reached the correct state
  in all 50 transitions. Median latency was 1.22 s, p95 was 2.24 s, and maximum
  was 2.97 s. Two transitions exceeded the two-second target. No physical key
  was pressed during the automated run, so duplicate-output coverage remains
  manual. After the close-race fix, another five round trips completed with no
  false device errors.

## Conditional alternatives

- **Focus handoff latency:** investigate the two >2 s transitions before adding
  a stability window. Any debounce must cover both ownership directions and
  revoke Herdr dispatch immediately while ownership is undecided.
- **IOHID seize experiment:** if USB re-enumeration proves unreliable, test
  exclusive IOHID access as the first alternative. It must demonstrate Layer 2
  isolation from ChatGPT, OAI lighting, prompt native Layer 1 restoration, and
  acceptable Input Monitoring permission behavior before replacing raw USB.

Do not return to shared IOHID or Bluetooth delivery: both allow ChatGPT to see
Layer 2 reports and reproduce the duplicate-input failure.
