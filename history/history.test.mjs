import assert from "node:assert/strict";
import { test } from "node:test";
import * as h from "./history.mjs";

const FP = "dev:ino:birth";

test("record pushes and dedupes consecutive focuses", () => {
  let state = visited("A", "B", "B", "C");
  assert.deepEqual(state.entries, ["A", "B", "C"]);
  assert.equal(state.cursor, 2);
});

test("new focus after back truncates the forward branch", () => {
  let state = visited("A", "B", "C");
  state = h.onFocusOk(state, 0);
  state = h.record(state, "D");
  assert.deepEqual(state.entries, ["A", "D"]);
  assert.equal(state.cursor, 1);
});

test("echo consumption skips recording and drops older coalesced echoes", () => {
  // two jumps queued: only the final pane's focus event arrives (tick coalescing)
  let state = visited("A", "B", "C");
  state = h.pushEcho(state, "B", 0);
  state = h.pushEcho(state, "A", 0);
  const before = state.entries;
  state = h.record(state, "A");
  assert.deepEqual(state.entries, before);
  assert.deepEqual(state.echoes, []); // the never-arriving echo for B is gone too
});

test("rapid double-back does not truncate the forward branch", () => {
  let state = visited("A", "B", "C");
  // back #1: C -> B
  state = h.pushEcho(state, "B", 0);
  state = h.onFocusOk(state, 1);
  // back #2: B -> A
  state = h.pushEcho(state, "A", 0);
  state = h.onFocusOk(state, 0);
  // echoes arrive late, in order
  state = h.record(state, "B");
  state = h.record(state, "A");
  assert.deepEqual(state.entries, ["A", "B", "C"]);
  assert.equal(state.cursor, 0);
});

test("dead-pane splice adjusts cursor going back", () => {
  let state = visited("A", "B", "C", "D");
  state = h.pushEcho(state, "C", 0);
  state = h.onFocusFailed(state, 2);
  assert.deepEqual(state.entries, ["A", "B", "D"]);
  assert.equal(state.cursor, 2);
  assert.deepEqual(state.echoes, []);
  assert.deepEqual(h.planJump(state, -1), { target: "B", index: 1 });
});

test("dead-pane splice adjusts cursor going forward", () => {
  let state = visited("A", "B", "C");
  state = h.onFocusOk(state, 0);
  state = h.onFocusFailed(state, 1);
  assert.deepEqual(state.entries, ["A", "C"]);
  assert.equal(state.cursor, 0);
  assert.deepEqual(h.planJump(state, 1), { target: "C", index: 1 });
});

test("jump planning stops at both ends and on empty history", () => {
  assert.equal(h.planJump(fresh(), -1), null);
  assert.equal(h.planJump(fresh(), 1), null);
  const state = visited("A", "B");
  assert.equal(h.planJump(state, 1), null);
  assert.equal(h.planJump(h.onFocusOk(state, 0), -1), null);
});

test("cap at MAX_ENTRIES keeps cursor valid", () => {
  let state = fresh();
  for (let i = 0; i < h.MAX_ENTRIES + 20; i++) state = h.record(state, `w1:p${i}`);
  assert.equal(state.entries.length, h.MAX_ENTRIES);
  assert.equal(state.cursor, h.MAX_ENTRIES - 1);
  assert.equal(state.entries[0], "w1:p20");
});

test("echoes expire", () => {
  let state = fresh();
  for (let i = 0; i < 3; i++) state = h.pushEcho(state, `p${i}`, 1000);
  assert.deepEqual(h.expireEchoes(state, 1000 + h.ECHO_TTL_MS).echoes, []);
  assert.equal(h.expireEchoes(state, 1000).echoes.length, 3);
});

test("loadState resets on fingerprint mismatch or corruption", () => {
  const valid = JSON.stringify(visited("A", "B"));
  assert.equal(h.loadState(valid, FP).cursor, 1);
  assert.deepEqual(h.loadState(valid, "other"), {
    fingerprint: "other",
    entries: [],
    cursor: -1,
    echoes: [],
  });
  assert.deepEqual(h.loadState("", FP), fresh());
  assert.deepEqual(h.loadState('{"entries":["A"],"cursor":5,"echoes":[],"fingerprint":"' + FP + '"}', FP), fresh());
});

function visited(...panes) {
  let state = fresh();
  for (const p of panes) state = h.record(state, p);
  return state;
}

function fresh() {
  return { fingerprint: FP, entries: [], cursor: -1, echoes: [] };
}
