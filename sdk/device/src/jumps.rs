//! `client.jump` from a device key, in the order the keys were pressed.

use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::{self, JoinHandle},
};

use herdr_client::{ClientJumpError, ClientJumped, follow::JumpTarget};

use crate::{Focus, Refusal};

#[derive(Debug, thiserror::Error)]
pub enum JumpError {
    #[error(transparent)]
    Refused(#[from] Refusal),
    /// A later press was made before this one was written, so it was never
    /// sent.
    #[error("a newer press replaced this jump before it was sent")]
    Replaced,
    /// This jump was written, and a later press's jump was written before
    /// the wait for this one's answer began. Its connection was closed
    /// unread, so it may still have happened.
    #[error("a newer press's jump was sent before this jump's answer was read")]
    Overtaken,
    #[error("jump to {endpoint_id}/{pane_id}: {source}")]
    Failed {
        endpoint_id: String,
        pane_id: String,
        #[source]
        source: ClientJumpError,
    },
}

impl JumpError {
    /// A newer jump took this one's place, here before it was sent or
    /// answered, or in the daemon afterwards: the outcome of an earlier
    /// press once another followed it, not a failure to report.
    pub fn is_superseded(&self) -> bool {
        matches!(
            self,
            Self::Replaced
                | Self::Overtaken
                | Self::Failed {
                    source: ClientJumpError::Superseded,
                    ..
                }
        )
    }

    fn failed(target: &JumpTarget, source: ClientJumpError) -> Self {
        Self::Failed {
            endpoint_id: target.endpoint_id.clone(),
            pane_id: target.pane_id.clone(),
            source,
        }
    }
}

/// Waits for the answer to a jump that was written. Dropping it closes the
/// jump's connection.
type Answer = Box<dyn FnOnce() -> Result<ClientJumped, ClientJumpError> + Send>;

/// Receives the one outcome of a press.
type Report = Box<dyn FnOnce(Result<ClientJumped, JumpError>) + Send>;

/// A press whose jump has not been written.
struct Press {
    target: JumpTarget,
    report: Report,
}

/// A jump that was written and whose answer has not been read.
struct Written {
    target: JumpTarget,
    answer: Answer,
    report: Report,
}

/// A place for one value between the thread that puts and the thread that
/// takes. A value not taken yet gives way to the next one, so nothing is
/// ever queued.
struct Slot<T> {
    place: Mutex<Place<T>>,
    changed: Condvar,
}

struct Place<T> {
    value: Option<T>,
    closed: bool,
}

impl<T> Slot<T> {
    fn new() -> Self {
        Self {
            place: Mutex::new(Place {
                value: None,
                closed: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn place(&self) -> MutexGuard<'_, Place<T>> {
        self.place.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Leaves `value` for the taking thread and returns the value it
    /// displaced, which that thread never saw.
    fn put(&self, value: T) -> Option<T> {
        let displaced = self.place().value.replace(value);
        self.changed.notify_one();
        displaced
    }

    /// Waits for a value. `None` once the slot is closed and empty.
    fn take(&self) -> Option<T> {
        let mut place = self.place();
        loop {
            if let Some(value) = place.value.take() {
                return Some(value);
            }
            if place.closed {
                return None;
            }
            place = self
                .changed
                .wait(place)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Nothing is put from now on. A value still there is taken first.
    fn close(&self) {
        self.place().closed = true;
        self.changed.notify_one();
    }
}

/// Sends the jumps of one device so that the newest press wins, on two
/// threads and at most three connections however fast keys are pressed.
///
/// A daemon lets the jump it receives last win and holds each one until its
/// client answers, up to twenty seconds. So one thread writes the jumps and
/// another reads their answers, and neither keeps a queue:
///
/// - [`Self::press`] leaves the press in a place for one. A press still
///   there when the next one is made was not written and ends as
///   [`JumpError::Replaced`]. The writing thread takes the press, checks
///   that its client is still the followed one, connects and writes. Jumps
///   are therefore written one at a time, in press order, and an older
///   press is never written after a newer one.
/// - The written jump is left in a second place for one, for the reading
///   thread, so the next jump is written while an earlier answer is still
///   awaited. A jump still there when the next one is written ends as
///   [`JumpError::Overtaken`] and its connection is closed unread. Answers
///   are read one at a time, so the outcome of a jump is reported after
///   that of the jump read before it.
///
/// The connections are the one being written, the one in the second place,
/// and the one being read. A write and a read each end within the socket
/// timeout of a jump ([`herdr_client::CLIENT_JUMP_TIMEOUT`]).
///
/// Dropping the dispatcher does not wait. A press that was made is still
/// carried through: written, if its client is still followed, and its
/// answer read. Then both threads end, at the latest when those last socket
/// timeouts have run out.
pub struct JumpDispatcher {
    presses: Arc<Slot<Press>>,
}

impl JumpDispatcher {
    /// Jumps are checked against `focus` when they are about to be written.
    pub fn new(focus: Focus) -> Self {
        Self::start(focus, |target| {
            let pending = target.start()?;
            Ok(Box::new(move || pending.wait()))
        })
        .0
    }

    /// The dispatcher, and its writing and reading thread.
    fn start(
        focus: Focus,
        write: impl FnMut(&JumpTarget) -> Result<Answer, ClientJumpError> + Send + 'static,
    ) -> (Self, [JoinHandle<()>; 2]) {
        let presses = Arc::new(Slot::new());
        let written = Arc::new(Slot::new());
        let writer = thread::spawn({
            let presses = Arc::clone(&presses);
            let written = Arc::clone(&written);
            move || write_presses(&presses, &written, &focus, write)
        });
        let reader = thread::spawn(move || read_answers(&written));
        (Self { presses }, [writer, reader])
    }

    /// Takes one press and returns at once. Call it from the loop that
    /// receives the device's events: the order of the calls is the order of
    /// the presses, which is why it takes `&mut self`.
    ///
    /// `report` receives the press's one outcome and must not block. It runs
    /// on one of the dispatcher's threads, or, for a press replaced before
    /// it was written, inside the later call that replaced it.
    ///
    /// The jump is sent if its client is still the followed one when it is
    /// about to be written; the client's window need not have focus. It is
    /// written once: after a lost answer it may still have happened, so it
    /// is never repeated.
    pub fn press(
        &mut self,
        target: JumpTarget,
        report: impl FnOnce(Result<ClientJumped, JumpError>) + Send + 'static,
    ) {
        let press = Press {
            target,
            report: Box::new(report),
        };
        if let Some(replaced) = self.presses.put(press) {
            (replaced.report)(Err(JumpError::Replaced));
        }
    }
}

impl Drop for JumpDispatcher {
    fn drop(&mut self) {
        self.presses.close();
    }
}

/// The writing thread: writes each press it takes if its client is still
/// followed, and leaves the written jump for the reading thread.
fn write_presses(
    presses: &Slot<Press>,
    written: &Slot<Written>,
    focus: &Focus,
    mut write: impl FnMut(&JumpTarget) -> Result<Answer, ClientJumpError>,
) {
    while let Some(Press { target, report }) = presses.take() {
        let answer = focus
            .check_followed(&target.client)
            .map_err(JumpError::from)
            .and_then(|()| write(&target).map_err(|source| JumpError::failed(&target, source)));
        let answer = match answer {
            Ok(answer) => answer,
            Err(error) => {
                report(Err(error));
                continue;
            }
        };
        let jump = Written {
            target,
            answer,
            report,
        };
        if let Some(Written { answer, report, .. }) = written.put(jump) {
            drop(answer);
            report(Err(JumpError::Overtaken));
        }
    }
    written.close();
}

/// The reading thread: waits for the answer of each written jump it takes.
fn read_answers(written: &Slot<Written>) {
    while let Some(Written {
        target,
        answer,
        report,
    }) = written.take()
    {
        report(answer().map_err(|source| JumpError::failed(&target, source)));
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc::{self, Receiver, RecvTimeoutError, Sender},
        },
        thread::ThreadId,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::focus::tests::{client, focused_on};
    use herdr_client::Error;

    /// Bounds every wait of a test that would otherwise hang.
    const WAIT: Duration = Duration::from_secs(10);

    fn target(pane: u64) -> JumpTarget {
        JumpTarget {
            client: client("work"),
            endpoint_id: "ssh:build".into(),
            boot_id: "boot-b".into(),
            pane_id: format!("w1:p{pane}"),
        }
    }

    fn jumped(pane_id: &str) -> ClientJumped {
        ClientJumped {
            client_id: 7,
            endpoint_id: "ssh:build".into(),
            pane_id: pane_id.into(),
        }
    }

    fn pane_of(target: &JumpTarget) -> u64 {
        target.pane_id.trim_start_matches("w1:p").parse().unwrap()
    }

    type Outcome = (u64, Result<ClientJumped, JumpError>);

    /// Presses `pane`'s key; the outcome arrives on `outcomes`.
    fn press(jumps: &mut JumpDispatcher, pane: u64, outcomes: &Sender<Outcome>) {
        let outcomes = outcomes.clone();
        jumps.press(target(pane), move |result| {
            let _ = outcomes.send((pane, result));
        });
    }

    fn next<T>(receiver: &Receiver<T>) -> T {
        receiver.recv_timeout(WAIT).expect("the dispatcher stalled")
    }

    /// The next two outcomes, which two threads may report in either order,
    /// by pane.
    fn next_two(results: &Receiver<Outcome>) -> [Outcome; 2] {
        let mut outcomes = [next(results), next(results)];
        outcomes.sort_by_key(|(pane, _)| *pane);
        outcomes
    }

    /// A jump that was written just before a newer one: its answer was
    /// read, or the newer jump was written before the reading thread got to
    /// it.
    fn assert_answered_or_overtaken(pane: u64, result: Result<ClientJumped, JumpError>) {
        match result {
            Ok(done) => assert_eq!(done, jumped(&format!("w1:p{pane}"))),
            Err(JumpError::Overtaken) => {}
            Err(error) => panic!("press {pane}: {error:?}"),
        }
    }

    /// Waits for the threads of a dropped dispatcher to end.
    fn ended(threads: [JoinHandle<()>; 2]) {
        let deadline = Instant::now() + WAIT;
        for thread in threads {
            while !thread.is_finished() {
                assert!(
                    Instant::now() < deadline,
                    "a dispatcher thread outlived its dispatcher"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            thread.join().expect("a dispatcher thread panicked");
        }
    }

    /// What a test's jumps hold: the connections open at once, and the
    /// threads that wrote and read.
    #[derive(Clone, Default)]
    struct Wire {
        open: Arc<AtomicUsize>,
        most_open: Arc<AtomicUsize>,
        writers: Arc<Mutex<HashSet<ThreadId>>>,
        readers: Arc<Mutex<HashSet<ThreadId>>>,
    }

    /// One written jump's connection, open until it is dropped.
    struct Connection(Wire);

    impl Drop for Connection {
        fn drop(&mut self) {
            self.0.open.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl Wire {
        /// Call it where a jump is written: opens the jump's connection,
        /// which `answer` answers and which closes with the returned wait.
        fn connect(
            &self,
            answer: impl FnOnce() -> Result<ClientJumped, ClientJumpError> + Send + 'static,
        ) -> Answer {
            let seen = |threads: &Mutex<HashSet<ThreadId>>| {
                threads.lock().unwrap().insert(thread::current().id());
            };
            seen(&self.writers);
            let open = self.open.fetch_add(1, Ordering::SeqCst) + 1;
            self.most_open.fetch_max(open, Ordering::SeqCst);
            let connection = Connection(self.clone());
            Box::new(move || {
                seen(&connection.0.readers);
                answer()
            })
        }

        fn open(&self) -> usize {
            self.open.load(Ordering::SeqCst)
        }

        fn most_open(&self) -> usize {
            self.most_open.load(Ordering::SeqCst)
        }

        /// Every write ran on one thread and every read on one other, and
        /// neither on the thread that presses.
        fn assert_two_threads(&self) {
            let writers = self.writers.lock().unwrap();
            let readers = self.readers.lock().unwrap();
            assert!(writers.len() <= 1, "{writers:?}");
            assert!(readers.len() <= 1, "{readers:?}");
            assert!(writers.is_disjoint(&readers));
            assert!(!writers.contains(&thread::current().id()));
            assert!(!readers.contains(&thread::current().id()));
        }
    }

    /// A dispatcher whose writes are recorded on the returned receiver and
    /// answered at once as focused.
    fn recording(focus: Focus) -> (JumpDispatcher, Receiver<String>) {
        let (written, writes) = mpsc::channel();
        let (jumps, _) = JumpDispatcher::start(focus, move |target| {
            let pane_id = target.pane_id.clone();
            let _ = written.send(pane_id.clone());
            Ok(Box::new(move || Ok(jumped(&pane_id))))
        });
        (jumps, writes)
    }

    /// A dispatcher with one step of its first jump held open.
    struct Held {
        jumps: JumpDispatcher,
        threads: [JoinHandle<()>; 2],
        /// The pane of every write.
        writes: Receiver<u64>,
        /// Lets the held step go on.
        release: Sender<()>,
    }

    /// A dispatcher on `wire` whose first write waits to be released. Every
    /// write is recorded as it starts.
    fn first_write_held(focus: Focus, wire: &Wire) -> Held {
        let (written, writes) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let wire = wire.clone();
        let (jumps, threads) = JumpDispatcher::start(focus, move |target| {
            let pane = pane_of(target);
            let _ = written.send(pane);
            if pane == 1 {
                let _ = released.recv_timeout(WAIT);
            }
            let pane_id = target.pane_id.clone();
            Ok(wire.connect(move || Ok(jumped(&pane_id))))
        });
        Held {
            jumps,
            threads,
            writes,
            release,
        }
    }

    /// A dispatcher on `wire` whose first answer waits to be released and
    /// then says a newer jump replaced it, as a daemon does. Every write is
    /// recorded once it is done, and the start of the wait for the first
    /// answer on the returned receiver.
    fn first_answer_held(wire: &Wire) -> (Held, Receiver<()>) {
        let (written, writes) = mpsc::channel();
        let (waiting, waits) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let mut held = Some((waiting, released));
        let wire = wire.clone();
        let (jumps, threads) =
            JumpDispatcher::start(focused_on(Some(client("work"))), move |target| {
                let pane_id = target.pane_id.clone();
                let answer = match held.take() {
                    Some((waiting, released)) => wire.connect(move || {
                        let _ = waiting.send(());
                        let _ = released.recv_timeout(WAIT);
                        Err(ClientJumpError::Superseded)
                    }),
                    None => wire.connect(move || Ok(jumped(&pane_id))),
                };
                let _ = written.send(pane_of(target));
                Ok(answer)
            });
        let held = Held {
            jumps,
            threads,
            writes,
            release,
        };
        (held, waits)
    }

    #[test]
    fn a_press_replaced_before_it_is_written_is_never_written() {
        let wire = Wire::default();
        let Held {
            mut jumps,
            threads,
            writes,
            release,
        } = first_write_held(focused_on(Some(client("work"))), &wire);
        let (outcomes, results) = mpsc::channel();

        press(&mut jumps, 1, &outcomes);
        assert_eq!(next(&writes), 1);
        // Two more presses while the first is still being written. Neither
        // waits for it, and the one in the middle is over as soon as the
        // third is made.
        press(&mut jumps, 2, &outcomes);
        press(&mut jumps, 3, &outcomes);
        let (pane, second) = next(&results);
        let second = second.unwrap_err();
        assert_eq!(pane, 2);
        assert!(matches!(second, JumpError::Replaced), "{second:?}");
        assert!(second.is_superseded());

        release.send(()).unwrap();
        let [(first_pane, first), (third_pane, third)] = next_two(&results);
        assert_eq!((first_pane, third_pane), (1, 3));
        assert_answered_or_overtaken(1, first);
        assert_eq!(third.unwrap(), jumped("w1:p3"));
        // The second never reached the daemon, and the newest was written
        // last.
        assert_eq!(next(&writes), 3);
        drop(jumps);
        ended(threads);
        assert_eq!(
            writes.recv_timeout(WAIT),
            Err(RecvTimeoutError::Disconnected)
        );
        assert_eq!(wire.open(), 0);
    }

    #[test]
    fn a_jump_is_written_while_an_earlier_answer_is_awaited() {
        let wire = Wire::default();
        let (held, waits) = first_answer_held(&wire);
        let Held {
            mut jumps,
            threads,
            writes,
            release,
        } = held;
        let (outcomes, results) = mpsc::channel();

        press(&mut jumps, 1, &outcomes);
        assert_eq!(next(&writes), 1);
        next(&waits);
        // The first jump's answer is held, as a daemon holds it while its
        // client navigates. The second is written all the same.
        press(&mut jumps, 2, &outcomes);
        assert_eq!(next(&writes), 2);
        assert!(results.try_recv().is_err());

        // The daemon answers the first as replaced, then the second.
        release.send(()).unwrap();
        let (pane, first) = next(&results);
        assert_eq!(pane, 1);
        let first = first.unwrap_err();
        assert!(matches!(first, JumpError::Failed { .. }), "{first:?}");
        assert!(first.is_superseded());
        let (pane, second) = next(&results);
        assert_eq!((pane, second.unwrap()), (2, jumped("w1:p2")));
        drop(jumps);
        ended(threads);
        wire.assert_two_threads();
    }

    #[test]
    fn a_written_jump_not_yet_awaited_gives_way_to_a_newer_one() {
        let wire = Wire::default();
        let (held, waits) = first_answer_held(&wire);
        let Held {
            mut jumps,
            threads,
            writes,
            release,
        } = held;
        let (outcomes, results) = mpsc::channel();

        press(&mut jumps, 1, &outcomes);
        assert_eq!(next(&writes), 1);
        next(&waits);
        // The second is written and waits its turn to be read. The third is
        // written too, and takes that turn.
        press(&mut jumps, 2, &outcomes);
        assert_eq!(next(&writes), 2);
        press(&mut jumps, 3, &outcomes);
        assert_eq!(next(&writes), 3);
        let (pane, second) = next(&results);
        let second = second.unwrap_err();
        assert_eq!(pane, 2);
        assert!(matches!(second, JumpError::Overtaken), "{second:?}");
        assert!(second.is_superseded());
        // Its connection is closed: the first's and the third's are left.
        assert_eq!(wire.open(), 2);

        release.send(()).unwrap();
        assert_eq!(next(&results).0, 1);
        let (pane, third) = next(&results);
        assert_eq!((pane, third.unwrap()), (3, jumped("w1:p3")));
        drop(jumps);
        ended(threads);
        assert_eq!(wire.open(), 0);
    }

    #[test]
    fn a_burst_against_a_stalled_write_is_two_writes_and_one_outcome_each() {
        const PRESSES: u64 = 500;
        let wire = Wire::default();
        let Held {
            mut jumps,
            threads,
            writes,
            release,
        } = first_write_held(focused_on(Some(client("work"))), &wire);
        let (outcomes, results) = mpsc::channel();

        // The daemon does not take the first jump, and the keys go on.
        // Every press returns at once.
        press(&mut jumps, 1, &outcomes);
        assert_eq!(next(&writes), 1);
        for pane in 2..=PRESSES {
            press(&mut jumps, pane, &outcomes);
        }
        // All but the newest are over already, in the order they were made.
        for pane in 2..PRESSES {
            let (replaced, result) = next(&results);
            assert_eq!(replaced, pane);
            assert!(matches!(result, Err(JumpError::Replaced)), "{result:?}");
        }
        assert!(results.try_recv().is_err());
        assert!(writes.try_recv().is_err());
        assert_eq!(wire.open(), 0);

        release.send(()).unwrap();
        let [(first_pane, first), (last_pane, last)] = next_two(&results);
        assert_eq!((first_pane, last_pane), (1, PRESSES));
        assert_answered_or_overtaken(1, first);
        assert_eq!(last.unwrap(), jumped(&format!("w1:p{PRESSES}")));
        drop(jumps);
        ended(threads);
        // Two writes for all those presses, the last press's last.
        assert_eq!(writes.iter().collect::<Vec<_>>(), [PRESSES]);
        assert!(results.try_recv().is_err());
        wire.assert_two_threads();
        assert!(wire.most_open() <= 2, "{}", wire.most_open());
        assert_eq!(wire.open(), 0);
    }

    #[test]
    fn a_burst_against_a_stalled_answer_holds_three_connections_at_most() {
        const PRESSES: u64 = 500;
        let wire = Wire::default();
        let (held, waits) = first_answer_held(&wire);
        let Held {
            mut jumps,
            threads,
            writes,
            release,
        } = held;
        let (outcomes, results) = mpsc::channel();

        // The daemon takes every jump and answers none. Each press here is
        // made once the one before it was written, so all are written.
        press(&mut jumps, 1, &outcomes);
        assert_eq!(next(&writes), 1);
        next(&waits);
        for pane in 2..=PRESSES {
            press(&mut jumps, pane, &outcomes);
            assert_eq!(next(&writes), pane);
        }
        // All but the first and the newest are over, in the order written.
        for pane in 2..PRESSES {
            let (overtaken, result) = next(&results);
            assert_eq!(overtaken, pane);
            assert!(matches!(result, Err(JumpError::Overtaken)), "{result:?}");
        }
        assert!(results.try_recv().is_err());
        assert_eq!(wire.open(), 2);

        release.send(()).unwrap();
        assert_eq!(next(&results).0, 1);
        let (pane, last) = next(&results);
        assert_eq!(
            (pane, last.unwrap()),
            (PRESSES, jumped(&format!("w1:p{PRESSES}")))
        );
        drop(jumps);
        ended(threads);
        assert!(results.try_recv().is_err());
        wire.assert_two_threads();
        // The one being read, the one waiting to be, and the one written.
        assert_eq!(wire.most_open(), 3);
        assert_eq!(wire.open(), 0);
    }

    #[test]
    fn jumps_are_written_in_press_order_and_the_last_press_always_is() {
        const PRESSES: u64 = 500;
        let wire = Wire::default();
        let (written, writes) = mpsc::channel();
        let (mut jumps, threads) = JumpDispatcher::start(focused_on(Some(client("work"))), {
            let wire = wire.clone();
            move |target| {
                let _ = written.send(pane_of(target));
                let pane_id = target.pane_id.clone();
                Ok(wire.connect(move || Ok(jumped(&pane_id))))
            }
        });
        let (outcomes, results) = mpsc::channel();
        for pane in 1..=PRESSES {
            press(&mut jumps, pane, &outcomes);
        }
        drop(outcomes);
        drop(jumps);
        ended(threads);

        // Every press reports once: answered, or given way to a newer one
        // before it was written or before its answer was awaited.
        let mut answered = Vec::new();
        let mut overtaken = Vec::new();
        let mut replaced = Vec::new();
        for (pane, result) in results.iter() {
            match result {
                Ok(done) => {
                    assert_eq!(done, jumped(&format!("w1:p{pane}")));
                    answered.push(pane);
                }
                Err(JumpError::Overtaken) => overtaken.push(pane),
                Err(JumpError::Replaced) => replaced.push(pane),
                Err(error) => panic!("press {pane}: {error:?}"),
            }
        }
        let written: Vec<u64> = writes.iter().collect();
        // However the threads were scheduled, no older press was written
        // after a newer one, and the newest was written and answered.
        assert!(
            written.is_sorted_by(|left, right| left < right),
            "{written:?}"
        );
        assert_eq!(written.last(), Some(&PRESSES));
        assert_eq!(answered.last(), Some(&PRESSES));
        assert!(answered.is_sorted_by(|left, right| left < right));
        let mut sent = [answered, overtaken].concat();
        sent.sort_unstable();
        assert_eq!(sent, written);
        assert_eq!((sent.len() + replaced.len()) as u64, PRESSES);
        wire.assert_two_threads();
        assert!(wire.most_open() <= 3, "{}", wire.most_open());
        assert_eq!(wire.open(), 0);
    }

    #[test]
    fn a_dropped_dispatcher_finishes_what_was_pressed_and_ends_its_threads() {
        // The tool is stopping, as Micro and Deck are when they drop it: a
        // press that was not written is refused.
        let wire = Wire::default();
        let focus = focused_on(Some(client("work")));
        let Held {
            mut jumps,
            threads,
            writes,
            release,
        } = first_write_held(focus.clone(), &wire);
        let (outcomes, results) = mpsc::channel();
        press(&mut jumps, 1, &outcomes);
        assert_eq!(next(&writes), 1);
        press(&mut jumps, 2, &outcomes);
        focus.stop();
        // Dropping does not wait for the write that is under way.
        drop(jumps);
        assert!(results.try_recv().is_err());
        release.send(()).unwrap();
        let [(first_pane, first), (second_pane, second)] = next_two(&results);
        assert_eq!((first_pane, second_pane), (1, 2));
        assert_eq!(first.unwrap(), jumped("w1:p1"));
        let second = second.unwrap_err();
        assert!(
            matches!(second, JumpError::Refused(Refusal::Stopping)),
            "{second:?}"
        );
        ended(threads);
        assert!(writes.try_recv().is_err());
        assert_eq!(wire.open(), 0);

        // Dropped while its client is still followed: the press is written.
        let wire = Wire::default();
        let Held {
            mut jumps,
            threads,
            writes,
            release,
        } = first_write_held(focused_on(Some(client("work"))), &wire);
        press(&mut jumps, 1, &outcomes);
        assert_eq!(next(&writes), 1);
        press(&mut jumps, 2, &outcomes);
        drop(jumps);
        release.send(()).unwrap();
        let [(first_pane, first), (second_pane, second)] = next_two(&results);
        assert_eq!((first_pane, second_pane), (1, 2));
        assert_answered_or_overtaken(1, first);
        assert_eq!(second.unwrap(), jumped("w1:p2"));
        ended(threads);
        assert_eq!(writes.iter().collect::<Vec<_>>(), [2]);

        // With nothing pressed, both threads end at once.
        let (jumps, threads) =
            JumpDispatcher::start(focused_on(None), |_| Err(ClientJumpError::Timeout));
        drop(jumps);
        ended(threads);
    }

    #[test]
    fn a_jump_is_written_once_and_every_outcome_is_typed() {
        let (reply, replies) = mpsc::channel::<Result<ClientJumped, ClientJumpError>>();
        let (written, writes) = mpsc::channel();
        let (mut jumps, threads) =
            JumpDispatcher::start(focused_on(Some(client("work"))), move |target| {
                let _ = written.send(target.params());
                let reply = replies.recv_timeout(WAIT).expect("a jump was retried");
                Ok(Box::new(move || reply))
            });
        let (outcomes, results) = mpsc::channel();
        let mut send = |outcome| {
            reply.send(outcome).unwrap();
            press(&mut jumps, 4, &outcomes);
            next(&results).1
        };
        assert_eq!(send(Ok(jumped("w1:p4"))).unwrap(), jumped("w1:p4"));
        let errors = [
            ClientJumpError::ClientNotFound,
            ClientJumpError::ClientNotReporting,
            ClientJumpError::Superseded,
            ClientJumpError::ClientDeparted,
            ClientJumpError::StaleBoot,
            ClientJumpError::TargetNotFound,
            ClientJumpError::EndpointUnavailable,
            ClientJumpError::Timeout,
            ClientJumpError::Failed("no such tab".into()),
            ClientJumpError::Other(Error::EmptyResponse),
        ];
        let count = errors.len();
        for error in errors {
            let text = error.to_string();
            let superseded = matches!(error, ClientJumpError::Superseded);
            let failed = send(Err(error)).unwrap_err();
            assert_eq!(failed.is_superseded(), superseded, "{failed:?}");
            assert!(matches!(failed, JumpError::Failed { .. }), "{failed:?}");
            assert_eq!(
                failed.to_string(),
                format!("jump to ssh:build/w1:p4: {text}")
            );
        }
        drop(jumps);
        ended(threads);
        assert_eq!(
            writes.iter().collect::<Vec<_>>(),
            vec![target(4).params(); count + 1]
        );
    }

    #[test]
    fn a_jump_that_cannot_be_written_fails_without_an_answer() {
        let (mut jumps, _) = JumpDispatcher::start(focused_on(Some(client("work"))), |_| {
            Err(ClientJumpError::Other(Error::EmptyResponse))
        });
        let (outcomes, results) = mpsc::channel();
        press(&mut jumps, 4, &outcomes);
        let failed = next(&results).1.unwrap_err();
        assert_eq!(
            failed.to_string(),
            format!("jump to ssh:build/w1:p4: {}", Error::EmptyResponse)
        );
        assert!(!failed.is_superseded());
    }

    #[test]
    fn a_jump_needs_its_client_followed_but_not_focused() {
        let (outcomes, results) = mpsc::channel();
        // Another app is in front: the jump is still sent.
        let behind = Focus::new(Default::default());
        behind.set(Some(client("work")), false);
        let (mut jumps, writes) = recording(behind);
        press(&mut jumps, 4, &outcomes);
        assert_eq!(next(&results).1.unwrap(), jumped("w1:p4"));
        assert_eq!(next(&writes), "w1:p4");

        let stopping = focused_on(Some(client("work")));
        stopping.stop();
        for (focus, refusal) in [
            (focused_on(None), Refusal::NotFollowed),
            (focused_on(Some(client("default"))), Refusal::NotFollowed),
            (stopping, Refusal::Stopping),
        ] {
            let (mut jumps, writes) = recording(focus);
            press(&mut jumps, 4, &outcomes);
            let refused = next(&results).1.unwrap_err();
            assert!(
                matches!(&refused, JumpError::Refused(reason) if *reason == refusal),
                "{refused:?}"
            );
            assert!(!refused.is_superseded());
            // Nothing was written for another client or while stopping.
            drop(jumps);
            assert_eq!(
                writes.recv_timeout(WAIT),
                Err(RecvTimeoutError::Disconnected)
            );
        }
    }
}
