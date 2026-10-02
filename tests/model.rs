//! Runs random sequences of operations on a `Deferred` and checks every result against
//! a simple model of how it should behave.
//!
//! Tasks run on a manual executor, so the test decides exactly when each task finishes,
//! panics, or is dropped by the runtime. The run is deterministic for a given sequence,
//! and proptest shrinks any failure to the shortest sequence that reproduces it.
#![cfg(feature = "std")]

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Once;
use std::task::{Context, Poll};

use async_deferred::{BeginError, Deferred, Error, LocalSpawner, SpawnError, State};
use futures_channel::oneshot;
use futures_util::FutureExt;
use proptest::prelude::*;

// --- the operations ---

#[derive(Debug, Clone, Copy, PartialEq)]
enum Callback {
    None,
    Runs,
    Panics,
}

#[derive(Debug, Clone)]
enum Op {
    Begin(Callback),
    /// `begin`, while the runtime refuses new tasks (e.g. its pool is full).
    BeginRefused(Callback),
    /// The running task returns this value.
    Finish(u32),
    /// The running task panics.
    Panic,
    /// The runtime drops the running task, like on shutdown.
    RuntimeDrop,
    Cancel,
    CancelAndWait,
    Take,
    TryGet,
    State,
    Join,
    PanicMessage,
    /// Consumes the `Deferred` with `into_result`, then continues with a new one.
    IntoResult,
}

fn op() -> impl Strategy<Value = Op> {
    let callback = prop_oneof![
        Just(Callback::None),
        Just(Callback::Runs),
        Just(Callback::Panics)
    ];
    prop_oneof![
        3 => callback.clone().prop_map(Op::Begin),
        1 => callback.prop_map(Op::BeginRefused),
        3 => any::<u32>().prop_map(Op::Finish),
        1 => Just(Op::Panic),
        1 => Just(Op::RuntimeDrop),
        2 => Just(Op::Cancel),
        2 => Just(Op::CancelAndWait),
        2 => Just(Op::Take),
        1 => Just(Op::TryGet),
        1 => Just(Op::State),
        2 => Just(Op::Join),
        1 => Just(Op::PanicMessage),
        1 => Just(Op::IntoResult),
    ]
}

// --- the model ---

#[derive(Debug, Clone, PartialEq)]
enum Model {
    NotStarted,
    Running(Callback),
    Done { value: u32, callback_panicked: bool },
    Panicked,
    Cancelled,
}

impl Model {
    /// A new task can start unless one is running or a result is waiting to be taken.
    fn can_begin(&self) -> bool {
        matches!(self, Model::NotStarted | Model::Panicked | Model::Cancelled)
    }

    fn state(&self) -> State {
        match self {
            Model::NotStarted => State::NotStarted,
            Model::Running(_) => State::Pending,
            Model::Done {
                callback_panicked: false,
                ..
            } => State::Completed,
            Model::Done {
                callback_panicked: true,
                ..
            } => State::CallbackPanicked,
            Model::Panicked => State::TaskPanicked,
            Model::Cancelled => State::Cancelled,
        }
    }

    fn value(&self) -> Option<u32> {
        match self {
            Model::Done { value, .. } => Some(*value),
            _ => None,
        }
    }

    fn panic_message(&self) -> Option<&'static str> {
        match self {
            Model::Panicked => Some(TASK_PANIC),
            Model::Done {
                callback_panicked: true,
                ..
            } => Some(CALLBACK_PANIC),
            _ => None,
        }
    }

    /// The result of a `join` that does not have to wait.
    fn join(&self) -> Result<u32, Error> {
        match self {
            Model::Done { value, .. } => Ok(*value),
            Model::Panicked => Err(Error::Panicked(TASK_PANIC.into())),
            Model::Cancelled => Err(Error::Cancelled),
            Model::NotStarted => Err(Error::NotStarted),
            Model::Running(_) => unreachable!("join would wait"),
        }
    }
}

const TASK_PANIC: &str = "task boom";
const CALLBACK_PANIC: &str = "callback boom";
const REFUSED: SpawnError = SpawnError::new("refused");

// --- the manual executor ---

type Task = Pin<Box<dyn Future<Output = ()>>>;

/// Holds spawned tasks; the test polls or drops them explicitly.
#[derive(Clone, Default)]
struct Manual {
    tasks: Rc<RefCell<Vec<Option<Task>>>>,
    /// When set, spawning fails, like a runtime whose task pool is full.
    refuse: Rc<Cell<bool>>,
}

impl LocalSpawner for Manual {
    fn spawn_local<F>(&self, task: F) -> Result<(), SpawnError>
    where
        F: Future<Output = ()> + 'static,
    {
        if self.refuse.get() {
            return Err(REFUSED);
        }
        self.tasks.borrow_mut().push(Some(Box::pin(task)));
        Ok(())
    }
}

impl Manual {
    /// Polls every unfinished task once.
    fn run(&self) {
        let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
        let count = self.tasks.borrow().len();
        for i in 0..count {
            let task = self.tasks.borrow_mut()[i].take();
            if let Some(mut task) = task {
                if task.as_mut().poll(&mut cx).is_pending() {
                    self.tasks.borrow_mut()[i] = Some(task);
                }
            }
        }
    }

    fn last_index(&self) -> usize {
        self.tasks.borrow().len() - 1
    }

    fn is_finished(&self, index: usize) -> bool {
        self.tasks.borrow()[index].is_none()
    }

    fn drop_task(&self, index: usize) {
        self.tasks.borrow_mut()[index] = None;
    }
}

/// What the test tells the running task to do.
enum Action {
    Finish(u32),
    Panic,
}

/// A task that waits for the test to tell it how to end.
async fn controlled_task(action: oneshot::Receiver<Action>) -> u32 {
    match action.await {
        Ok(Action::Finish(value)) => value,
        Ok(Action::Panic) => panic!("{TASK_PANIC}"),
        // The test lost track of this task (it was cancelled); never finish.
        Err(oneshot::Canceled) => std::future::pending().await,
    }
}

// --- the test ---

/// The task the test currently controls, if the model says one is running.
struct Running {
    index: usize,
    action: oneshot::Sender<Action>,
}

fn run_ops(ops: &[Op]) -> Result<(), TestCaseError> {
    let executor = Manual::default();
    let mut deferred: Deferred<u32> = Deferred::new();
    let mut model = Model::NotStarted;
    let mut running: Option<Running> = None;
    // What the last `Callback::Runs` callback received.
    let callback_saw = Rc::new(Cell::new(None));

    for op in ops {
        match op {
            Op::Begin(callback) | Op::BeginRefused(callback) => {
                let refused = matches!(op, Op::BeginRefused(_));
                executor.refuse.set(refused);
                let (action_tx, action_rx) = oneshot::channel();
                let task = controlled_task(action_rx);
                let started = match callback {
                    Callback::None => deferred.begin_local_on(&executor, task),
                    Callback::Runs => {
                        let saw = callback_saw.clone();
                        deferred.begin_with_callback_local_on(&executor, task, move |v| {
                            saw.set(Some(*v))
                        })
                    }
                    Callback::Panics => {
                        deferred.begin_with_callback_local_on(&executor, task, |_| {
                            panic!("{CALLBACK_PANIC}")
                        })
                    }
                };
                executor.refuse.set(false);
                let expected = if !model.can_begin() {
                    Err(BeginError::AlreadyStarted)
                } else if refused {
                    Err(BeginError::Spawn(REFUSED))
                } else {
                    Ok(())
                };
                prop_assert_eq!(started, expected, "begin");
                if started.is_ok() {
                    model = Model::Running(*callback);
                    running = Some(Running {
                        index: executor.last_index(),
                        action: action_tx,
                    });
                    callback_saw.set(None);
                }
            }
            Op::Finish(value) => {
                if let Some(task) = running.take() {
                    let Model::Running(callback) = model else {
                        unreachable!()
                    };
                    let _ = task.action.send(Action::Finish(*value));
                    executor.run();
                    model = Model::Done {
                        value: *value,
                        callback_panicked: matches!(callback, Callback::Panics),
                    };
                    if let Callback::Runs = callback {
                        prop_assert_eq!(callback_saw.get(), Some(*value), "callback value");
                    }
                }
            }
            Op::Panic => {
                if let Some(task) = running.take() {
                    let _ = task.action.send(Action::Panic);
                    executor.run();
                    model = Model::Panicked;
                    prop_assert_eq!(callback_saw.get(), None, "callback ran after panic");
                }
            }
            Op::RuntimeDrop => {
                if let Some(task) = running.take() {
                    executor.drop_task(task.index);
                    model = Model::Cancelled;
                }
            }
            Op::Cancel => {
                let cancelled = deferred.cancel();
                prop_assert_eq!(cancelled, matches!(model, Model::Running(_)), "cancel");
                if cancelled {
                    running = None;
                    model = Model::NotStarted;
                    executor.run(); // let the aborted task stop
                }
            }
            Op::CancelAndWait => {
                let was_running = running.as_ref().map(|task| task.index);
                let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
                let mut wait = std::pin::pin!(deferred.cancel_and_wait());
                let mut result = wait.as_mut().poll(&mut cx);
                if result.is_pending() {
                    executor.run(); // the aborted task stops
                    result = wait.as_mut().poll(&mut cx);
                }
                prop_assert_eq!(
                    result,
                    Poll::Ready(was_running.is_some()),
                    "cancel_and_wait"
                );
                if let Some(index) = was_running {
                    prop_assert!(
                        executor.is_finished(index),
                        "task still running after cancel_and_wait"
                    );
                    running = None;
                    model = Model::NotStarted;
                }
            }
            Op::Take => {
                let taken = deferred.take();
                prop_assert_eq!(taken, model.value(), "take");
                if taken.is_some() {
                    model = Model::NotStarted;
                }
            }
            Op::TryGet => {
                prop_assert_eq!(deferred.try_get().copied(), model.value(), "try_get");
            }
            Op::State => {
                prop_assert_eq!(deferred.state(), model.state(), "state");
            }
            Op::Join => {
                let joined = deferred.join().now_or_never().map(|r| r.copied());
                if matches!(model, Model::Running(_)) {
                    prop_assert_eq!(joined, None, "join should wait");
                } else {
                    prop_assert_eq!(joined, Some(model.join()), "join");
                }
            }
            Op::IntoResult => {
                let owned = std::mem::take(&mut deferred);
                let mut future = owned.into_result();
                let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
                let polled = Pin::new(&mut future).poll(&mut cx);
                if matches!(model, Model::Running(_)) {
                    prop_assert!(polled.is_pending(), "into_result should wait");
                    // Dropping the future leaves the task running on its own.
                    drop(future);
                    running = None;
                } else {
                    prop_assert_eq!(polled, Poll::Ready(model.join()), "into_result");
                }
                model = Model::NotStarted;
            }
            Op::PanicMessage => {
                prop_assert_eq!(
                    deferred.panic_message(),
                    model.panic_message(),
                    "panic_message"
                );
            }
        }

        // The flag read through `&self` must agree with the channel, read through `&mut`.
        let observed = deferred.state();
        let _ = deferred.panic_message(); // receives the outcome if there is one
        prop_assert_eq!(observed, deferred.state(), "flag vs channel after {:?}", op);

        // After every operation, all queries must agree with the model.
        prop_assert_eq!(deferred.state(), model.state(), "state after {:?}", op);
        prop_assert_eq!(deferred.is_pending(), model.state() == State::Pending);
        prop_assert_eq!(deferred.is_ready(), model.value().is_some());
        prop_assert_eq!(deferred.try_get().copied(), model.value());
    }
    Ok(())
}

/// Hides the panics tasks and callbacks raise on purpose; any other panic is still printed.
fn silence_intended_panics() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let message = info
                .payload()
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| info.payload().downcast_ref::<&str>().copied());
            if !matches!(message, Some(TASK_PANIC | CALLBACK_PANIC)) {
                default_hook(info);
            }
        }));
    });
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 2000,
        // Failing sequences are saved here and replayed first on every run.
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct("tests/model.proptest-regressions"),
        )),
        ..ProptestConfig::default()
    })]

    #[test]
    fn deferred_matches_model(ops in prop::collection::vec(op(), 1..60)) {
        silence_intended_panics();
        run_ops(&ops)?;
    }
}

/// A fixed sequence through every state, so a broken harness can't pass silently.
#[test]
fn every_state_is_reached() {
    silence_intended_panics();
    let ops = [
        Op::State,
        Op::Begin(Callback::Runs),
        Op::Join,
        Op::Finish(1),
        Op::Join,
        Op::Take,
        Op::Begin(Callback::Panics),
        Op::Finish(2),
        Op::PanicMessage,
        Op::Take,
        Op::Begin(Callback::None),
        Op::Cancel,
        Op::BeginRefused(Callback::None),
        Op::State,
        Op::Begin(Callback::Runs),
        Op::CancelAndWait,
        Op::Begin(Callback::None),
        Op::Panic,
        Op::PanicMessage,
        Op::Join,
        Op::IntoResult,
        Op::Begin(Callback::None),
        Op::IntoResult,
        Op::Begin(Callback::None),
        Op::Finish(3),
        Op::IntoResult,
    ];
    run_ops(&ops).unwrap();

    let ops = [
        Op::Begin(Callback::None),
        Op::RuntimeDrop,
        Op::Join,
        Op::IntoResult,
    ];
    run_ops(&ops).unwrap();
}
