const GESTURES = ["tap", "doubleTap", "hold", "release"];

export function isGestureBinding(binding) {
  return Boolean(
    binding &&
      !Array.isArray(binding) &&
      typeof binding === "object" &&
      GESTURES.some((gesture) => Object.hasOwn(binding, gesture)),
  );
}

export class GestureDispatcher {
  #states = new Map();

  constructor(fire, schedule = setTimeout, cancel = clearTimeout) {
    this.fire = fire;
    this.schedule = schedule;
    this.cancel = cancel;
  }

  handle(key, binding, pressed, context) {
    if (!pressed) {
      this.#release(key);
      return;
    }
    if (!isGestureBinding(binding)) {
      if (binding) this.fire(binding, key, context);
      return;
    }
    this.#press(key, binding, context);
  }

  clear() {
    for (const state of this.#states.values()) {
      if (state.holdTimer) this.cancel(state.holdTimer);
      if (state.pending?.timer) this.cancel(state.pending.timer);
    }
    this.#states.clear();
  }

  #press(key, binding, context) {
    const state = this.#states.get(key) ?? {};
    if (state.down) return;
    state.down = true;
    state.binding = binding;
    state.context = context;
    state.held = false;
    state.doubleTap = null;
    if (state.pending) {
      this.cancel(state.pending.timer);
      state.doubleTap = state.pending.binding.doubleTap;
      state.context = state.pending.context;
      state.pending = null;
    }
    if (binding.hold) {
      state.holdTimer = this.schedule(() => {
        if (!state.down) return;
        state.held = true;
        state.doubleTap = null;
        this.fire(binding.hold, `${key} hold`, state.context);
      }, binding.holdMs ?? 500);
    }
    this.#states.set(key, state);
  }

  #release(key) {
    const state = this.#states.get(key);
    if (!state?.down) return;
    state.down = false;
    if (state.holdTimer) this.cancel(state.holdTimer);
    state.holdTimer = null;
    const binding = state.binding;
    if (binding.release) {
      this.fire(binding.release, `${key} release`, state.context);
    }
    if (state.held) {
      this.#states.delete(key);
    } else if (state.doubleTap) {
      this.fire(state.doubleTap, `${key} double-tap`, state.context);
      this.#states.delete(key);
    } else if (binding.doubleTap) {
      state.pending = {
        binding,
        context: state.context,
        timer: this.schedule(() => {
          if (binding.tap) {
            this.fire(binding.tap, `${key} tap`, state.context);
          }
          this.#states.delete(key);
        }, binding.doubleTapMs ?? 250),
      };
    } else {
      if (binding.tap) this.fire(binding.tap, `${key} tap`, state.context);
      this.#states.delete(key);
    }
  }
}
