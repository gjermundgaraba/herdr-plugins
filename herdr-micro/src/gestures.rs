use crate::config::{Action, Binding, GestureBinding};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq)]
pub struct GestureContext {
    pub session_key: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fired {
    pub action: Action,
    pub source: String,
    pub context: Option<GestureContext>,
}
#[derive(Clone)]
struct Pending {
    binding: GestureBinding,
    context: Option<GestureContext>,
    due: Instant,
}
#[derive(Clone)]
struct State {
    down: bool,
    binding: GestureBinding,
    context: Option<GestureContext>,
    held: bool,
    double_tap: Option<Action>,
    hold_due: Option<Instant>,
    pending: Option<Pending>,
}
#[derive(Default)]
pub struct GestureDispatcher {
    states: HashMap<String, State>,
}
impl GestureDispatcher {
    pub fn handle(
        &mut self,
        key: impl Into<String>,
        binding: Option<&Binding>,
        pressed: bool,
        context: Option<GestureContext>,
        now: Instant,
    ) -> Vec<Fired> {
        let key = key.into();
        if !pressed {
            return self.release(&key, now);
        }
        match binding {
            Some(Binding::Action(action)) => vec![Fired {
                action: action.clone(),
                source: key,
                context,
            }],
            Some(Binding::Gesture(binding)) => self.press(key, binding, context, now),
            _ => vec![],
        }
    }
    pub fn drain_due(&mut self, now: Instant) -> Vec<Fired> {
        let mut fired = Vec::new();
        self.states.retain(|key, state| {
            if state.down && state.hold_due.is_some_and(|due| due <= now) {
                state.hold_due = None;
                state.held = true;
                state.double_tap = None;
                if let Some(action) = state.binding.hold.clone() {
                    fired.push(Fired {
                        action,
                        source: format!("{key} hold"),
                        context: state.context.clone(),
                    });
                }
            }
            let tap_due = state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.due <= now);
            if tap_due {
                let pending = state.pending.take().expect("due pending tap");
                if let Some(action) = pending.binding.tap {
                    fired.push(Fired {
                        action,
                        source: format!("{key} tap"),
                        context: pending.context,
                    });
                }
            }
            state.down || state.pending.is_some()
        });
        fired
    }
    pub fn next_deadline(&self) -> Option<Instant> {
        self.states
            .values()
            .flat_map(|state| {
                [
                    state.hold_due,
                    state.pending.as_ref().map(|pending| pending.due),
                ]
            })
            .flatten()
            .min()
    }
    pub fn clear(&mut self) {
        self.states.clear();
    }
    fn press(
        &mut self,
        key: String,
        binding: &GestureBinding,
        context: Option<GestureContext>,
        now: Instant,
    ) -> Vec<Fired> {
        let state = self.states.entry(key).or_insert_with(|| State {
            down: false,
            binding: binding.clone(),
            context: context.clone(),
            held: false,
            double_tap: None,
            hold_due: None,
            pending: None,
        });
        if state.down {
            return vec![];
        }
        state.down = true;
        state.binding = binding.clone();
        state.context = context;
        state.held = false;
        state.double_tap = None;
        if let Some(pending) = state.pending.take() {
            state.double_tap = pending.binding.double_tap;
            state.context = pending.context;
        }
        state.hold_due = binding
            .hold
            .as_ref()
            .map(|_| now + Duration::from_millis(binding.hold_ms.unwrap_or(500)));
        vec![]
    }
    fn release(&mut self, key: &str, now: Instant) -> Vec<Fired> {
        let Some(state) = self.states.get_mut(key) else {
            return vec![];
        };
        if !state.down {
            return vec![];
        }
        state.down = false;
        state.hold_due = None;
        let mut fired = Vec::new();
        if let Some(action) = state.binding.release.clone() {
            fired.push(Fired {
                action,
                source: format!("{key} release"),
                context: state.context.clone(),
            });
        }
        if state.held {
            self.states.remove(key);
        } else if let Some(action) = state.double_tap.take() {
            fired.push(Fired {
                action,
                source: format!("{key} double-tap"),
                context: state.context.clone(),
            });
            self.states.remove(key);
        } else if state.binding.double_tap.is_some() {
            let due = now + Duration::from_millis(state.binding.double_tap_ms.unwrap_or(250));
            state.pending = Some(Pending {
                binding: state.binding.clone(),
                context: state.context.clone(),
                due,
            });
        } else if let Some(action) = state.binding.tap.clone() {
            fired.push(Fired {
                action,
                source: format!("{key} tap"),
                context: state.context.clone(),
            });
            self.states.remove(key);
        } else {
            self.states.remove(key);
        }
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Action;

    fn context(session_key: &str, generation: u64) -> Option<GestureContext> {
        Some(GestureContext {
            session_key: session_key.into(),
            generation,
        })
    }

    #[test]
    fn captures_first_session_for_double_tap() {
        let mut dispatcher = GestureDispatcher::default();
        let binding = Binding::Gesture(Box::new(GestureBinding {
            tap: Some(Action::Submit),
            double_tap: Some(Action::Diff),
            ..Default::default()
        }));
        let now = Instant::now();
        assert!(
            dispatcher
                .handle("key", Some(&binding), true, context("first", 1), now)
                .is_empty()
        );
        assert!(
            dispatcher
                .handle("key", Some(&binding), false, context("first", 1), now)
                .is_empty()
        );
        let fired = dispatcher.handle("key", Some(&binding), true, context("second", 2), now);
        assert!(fired.is_empty());
        let fired = dispatcher.handle("key", Some(&binding), false, context("second", 2), now);
        assert_eq!(fired[0].action, Action::Diff);
        assert_eq!(fired[0].context, context("first", 1));
    }

    #[test]
    fn deferred_tap_fires_at_its_deadline() {
        let mut dispatcher = GestureDispatcher::default();
        let binding = Binding::Gesture(Box::new(GestureBinding {
            tap: Some(Action::Submit),
            double_tap: Some(Action::Diff),
            double_tap_ms: Some(100),
            ..Default::default()
        }));
        let now = Instant::now();
        dispatcher.handle("key", Some(&binding), true, None, now);
        dispatcher.handle("key", Some(&binding), false, None, now);
        assert!(
            dispatcher
                .drain_due(now + Duration::from_millis(99))
                .is_empty()
        );
        let fired = dispatcher.drain_due(now + Duration::from_millis(100));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].action, Action::Submit);
        assert!(dispatcher.next_deadline().is_none());
    }

    #[test]
    fn deferred_hold_fires_once_at_its_deadline() {
        let mut dispatcher = GestureDispatcher::default();
        let binding = Binding::Gesture(Box::new(GestureBinding {
            hold: Some(Action::Fast),
            hold_ms: Some(100),
            ..Default::default()
        }));
        let now = Instant::now();
        dispatcher.handle("key", Some(&binding), true, None, now);
        assert!(
            dispatcher
                .drain_due(now + Duration::from_millis(99))
                .is_empty()
        );
        let fired = dispatcher.drain_due(now + Duration::from_millis(100));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].action, Action::Fast);
        assert!(
            dispatcher
                .drain_due(now + Duration::from_millis(200))
                .is_empty()
        );
    }
}
