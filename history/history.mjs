// Pure state transitions for the focus-history ring. No fs/net here.
//
// State: { fingerprint, entries: ["w1:p2", ...], cursor, echoes: [{p, ts}] }
// - entries: pane ids in visit order; cursor indexes the current position (-1 when empty)
// - echoes: pane ids we are about to focus ourselves; the resulting pane.focused
//   event is consumed instead of recorded (Herdr focus events carry no source field)

export const MAX_ENTRIES = 100;
export const ECHO_TTL_MS = 1500;

export function loadState(raw, fingerprint) {
  try {
    const s = JSON.parse(raw);
    if (
      s.fingerprint === fingerprint &&
      Array.isArray(s.entries) &&
      Number.isInteger(s.cursor) &&
      s.cursor >= -1 &&
      s.cursor < s.entries.length &&
      Array.isArray(s.echoes)
    ) {
      return s;
    }
  } catch {
    // corrupt or missing: fall through to a fresh state
  }
  return { fingerprint, entries: [], cursor: -1, echoes: [] };
}

// Echo hooks can be dropped silently (32-in-flight cap), so expiry is load-bearing.
export function expireEchoes(state, now) {
  return { ...state, echoes: state.echoes.filter((e) => now - e.ts < ECHO_TTL_MS) };
}

export function record(state, paneId) {
  const k = state.echoes.findIndex((e) => e.p === paneId);
  if (k >= 0) {
    // Echo of our own jump. Older pending echoes were coalesced into this one
    // server-side (focus events are diffed once per tick) and will never arrive.
    return { ...state, echoes: state.echoes.slice(k + 1) };
  }
  if (state.entries[state.cursor] === paneId) return state;
  let entries = state.entries.slice(0, state.cursor + 1);
  entries.push(paneId);
  if (entries.length > MAX_ENTRIES) entries = entries.slice(entries.length - MAX_ENTRIES);
  return { ...state, entries, cursor: entries.length - 1 };
}

export function planJump(state, step) {
  const index = state.cursor + step;
  if (index < 0 || index >= state.entries.length) return null;
  return { target: state.entries[index], index };
}

// Written before the pane.focus call so a crash mid-jump cannot leave an
// unconsumable echo unaccounted for; dropped again in onFocusFailed.
export function pushEcho(state, paneId, now) {
  return { ...state, echoes: [...state.echoes, { p: paneId, ts: now }] };
}

export function onFocusOk(state, index) {
  return { ...state, cursor: index };
}

export function onFocusFailed(state, index) {
  const target = state.entries[index];
  const entries = state.entries.slice();
  entries.splice(index, 1);
  const cursor = index <= state.cursor ? state.cursor - 1 : state.cursor;
  const last = state.echoes.findLastIndex((e) => e.p === target);
  const echoes = last >= 0 ? state.echoes.filter((_, i) => i !== last) : state.echoes;
  return { ...state, entries, cursor, echoes };
}
