# Future work

These are evidence-gated follow-ups, not planned features. Start one only when
its trigger is satisfied.

## External capabilities

- **Atomic Herdr focus guard.** Herdr is the only component that can atomically
  verify that its foreground full-app client still has outer-terminal focus,
  that an expected pane still exists and is focused, and then perform a
  mutation. A failed guard must have no effect and return a distinct error.
  This premise was verified against Herdr commit
  `df2cb2c3a585bdb22bb670f6da8147c7c1ffe982`: `ClientConnection` stores
  `outer_terminal_focus`, `update_outer_focus_from_events` updates it, and
  `foreground_client_outer_focus` plus `sync_foreground_client_state` expose it
  internally. Trigger: Herdr releases this as a socket-API and Rust-client
  contract. Then send the expected pane with every mutation and, after physical
  focus-switch testing, remove the per-action Ghostty focus query. Keep local
  route generations for stale queued work.
- **Stable terminal identity or global session discovery.** Trigger: Ghostty or
  Herdr releases a supported cross-process mapping between a Ghostty terminal
  UUID and a Herdr session, or Herdr exposes global session discovery. Replace
  the title probe or the remaining `herdr session list` call respectively.
- **Stable pane scrolling.** Trigger: Herdr exposes stable host-cell metrics or
  a pane-scroll method. Replace the experimental `pane.graphics.info`
  dependency.

## Measure before changing

- **Ghostty focus polling.** The current 50 ms native ScriptingBridge query is
  the largest known idle cost. Change it only if profiling shows material CPU,
  energy, or Apple Event overhead, or Ghostty exposes a reliable cross-process
  focus event carrying terminal identity. Any replacement must preserve prompt
  layer switching and fail-closed dispatch.
- **Herdr event batching or persistent request connections.** Add either only
  if traces show snapshot amplification during event bursts or connection setup
  materially contributes to action latency.
- **Helper I/O timing.** Measure command enqueue-to-owner delay, USB output
  duration, and input callback-to-daemon delivery while rotating the dial and
  forcing lighting changes. Wake the owner run loop only if queue delay is
  material; use asynchronous output only if input gaps correlate with USB
  writes. Preserve command ordering, disconnect handling, and bounded teardown.
- **Input edge resynchronization.** Add it only if a raw USB trace after framing
  recovery shows missing press or release events. The current dispatcher clears
  held state when its input context changes or the device disconnects; do not
  add a second state-reconciliation path without a physical reproducer.
- **Synchronous mapping probes.** Move them off the status loop only if a trace
  shows visible status stalls during real session topology changes.

## Conditional hardware alternatives

- **Focus handoff latency.** Investigate if transitions above two seconds recur.
  Any debounce must revoke Herdr dispatch immediately while ownership is
  undecided and cover both ownership directions.
- **IOHID seize.** Test it only if USB re-enumeration proves unreliable. It must
  demonstrate Layer 2 isolation from ChatGPT, OAI lighting, prompt native Layer
  1 restoration, and acceptable Input Monitoring permission behavior.

Do not return to shared IOHID or Bluetooth delivery: both allow ChatGPT to see
Layer 2 reports and reproduce the duplicate-input failure.
