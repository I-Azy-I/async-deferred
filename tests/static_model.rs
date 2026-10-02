//! Runs random sequences of operations on a `StaticDeferred` and checks every result
//! against a simple model, like `model.rs` does for `Deferred`.
//!
//! Tasks run on an executor driven by hand, so the test decides when each task first
//! runs, finishes or is dropped, and when leftover tasks from earlier runs get to run.
#![cfg(feature = "static-deferred")]

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use async_deferred::{BeginError, Error, State, StaticDeferred};
use futures_channel::oneshot;
use futures_util::task::noop_waker_ref;
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    /// `begin`, then spawn the ticket's task.
    Begin,
    /// `begin`, then drop the ticket, as when the spawn fails.
    BeginNotSpawned,
    /// The executor runs every task once.
    RunTasks,
    /// The current job returns this value, and the executor runs.
    Finish(u32),
    /// The executor drops the current task.
    DropTask,
    Cancel,
    CancelAndWait,
    Take,
    WithResult,
    /// Polls `join` once without running the executor.
    Join,
    State,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => Just(Op::Begin),
        1 => Just(Op::BeginNotSpawned),
        2 => Just(Op::RunTasks),
        3 => any::<u32>().prop_map(Op::Finish),
        1 => Just(Op::DropTask),
        2 => Just(Op::Cancel),
        1 => Just(Op::CancelAndWait),
        2 => Just(Op::Take),
        1 => Just(Op::WithResult),
        2 => Just(Op::Join),
        1 => Just(Op::State),
    ]
}

#[derive(Debug, Clone, PartialEq)]
enum Model {
    NotStarted,
    Pending,
    Done(u32),
    Cancelled,
}

impl Model {
    fn state(&self) -> State {
        match self {
            Model::NotStarted => State::NotStarted,
            Model::Pending => State::Pending,
            Model::Done(_) => State::Completed,
            Model::Cancelled => State::Cancelled,
        }
    }
}

type Task<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

/// The task of the current run, which the test controls.
struct Current {
    index: usize,
    started: bool,
    finish: oneshot::Sender<u32>,
}

/// A job that returns whatever value the test sends it.
async fn controlled(finish: oneshot::Receiver<u32>) -> u32 {
    match finish.await {
        Ok(value) => value,
        Err(oneshot::Canceled) => std::future::pending().await,
    }
}

fn run_tasks(tasks: &mut [Option<Task<'_>>], current: &mut Option<Current>) {
    let mut cx = Context::from_waker(noop_waker_ref());
    for (index, slot) in tasks.iter_mut().enumerate() {
        if let Some(task) = slot {
            if let Some(current) = current.as_mut().filter(|c| c.index == index) {
                current.started = true;
            }
            if task.as_mut().poll(&mut cx).is_ready() {
                *slot = None;
            }
        }
    }
}

fn run_ops(ops: &[Op]) -> Result<(), TestCaseError> {
    let deferred = StaticDeferred::<u32>::new();
    let mut tasks: Vec<Option<Task<'_>>> = Vec::new();
    let mut current: Option<Current> = None;
    let mut model = Model::NotStarted;
    let mut cx = Context::from_waker(noop_waker_ref());

    for op in ops {
        match op {
            Op::Begin | Op::BeginNotSpawned => {
                let ticket = deferred.begin();
                let can_begin = matches!(model, Model::NotStarted | Model::Cancelled);
                prop_assert_eq!(
                    ticket.as_ref().err(),
                    (!can_begin).then_some(&BeginError::AlreadyStarted),
                    "begin"
                );
                if let Ok(ticket) = ticket {
                    if matches!(op, Op::Begin) {
                        let (finish, finish_rx) = oneshot::channel();
                        tasks.push(Some(Box::pin(ticket.run(controlled(finish_rx)))));
                        current = Some(Current {
                            index: tasks.len() - 1,
                            started: false,
                            finish,
                        });
                        model = Model::Pending;
                    } else {
                        drop(ticket);
                        model = Model::NotStarted;
                    }
                }
            }
            Op::RunTasks => run_tasks(&mut tasks, &mut current),
            Op::Finish(value) => {
                if let Some(task) = current.take() {
                    let _ = task.finish.send(*value);
                    run_tasks(&mut tasks, &mut None);
                    prop_assert!(tasks[task.index].is_none(), "the job should be done");
                    model = Model::Done(*value);
                }
            }
            Op::DropTask => {
                if let Some(task) = current.take() {
                    tasks[task.index] = None;
                    // A task dropped before it ever ran only drops its ticket.
                    model = if task.started {
                        Model::Cancelled
                    } else {
                        Model::NotStarted
                    };
                }
            }
            Op::Cancel => {
                let cancelled = deferred.cancel();
                prop_assert_eq!(cancelled, model == Model::Pending, "cancel");
                if cancelled {
                    current = None; // its task is now a leftover
                    model = Model::NotStarted;
                }
            }
            Op::CancelAndWait => {
                let was_pending = model == Model::Pending;
                let mut wait = std::pin::pin!(deferred.cancel_and_wait());
                let mut result = None;
                for _ in 0..10 {
                    if let Poll::Ready(cancelled) = wait.as_mut().poll(&mut cx) {
                        result = Some(cancelled);
                        break;
                    }
                    run_tasks(&mut tasks, &mut None);
                }
                prop_assert_eq!(result, Some(was_pending), "cancel_and_wait");
                prop_assert!(
                    tasks.iter().all(Option::is_none),
                    "a task is still alive after cancel_and_wait"
                );
                current = None;
                if was_pending {
                    model = Model::NotStarted;
                }
            }
            Op::Take => {
                let expected = match model {
                    Model::Done(value) => Some(value),
                    _ => None,
                };
                prop_assert_eq!(deferred.take(), expected, "take");
                if expected.is_some() {
                    model = Model::NotStarted;
                }
            }
            Op::WithResult => {
                let expected = match model {
                    Model::Done(value) => Some(value),
                    _ => None,
                };
                prop_assert_eq!(deferred.with_result(|v| *v), expected, "with_result");
            }
            Op::Join => {
                let mut join = std::pin::pin!(deferred.join());
                let polled = join.as_mut().poll(&mut cx);
                let expected = match model {
                    Model::Pending => Poll::Pending,
                    Model::Done(value) => Poll::Ready(Ok(value)),
                    Model::NotStarted => Poll::Ready(Err(Error::NotStarted)),
                    Model::Cancelled => Poll::Ready(Err(Error::Cancelled)),
                };
                prop_assert_eq!(polled, expected, "join");
                if matches!(model, Model::Done(_)) {
                    model = Model::NotStarted;
                }
            }
            Op::State => {}
        }

        prop_assert_eq!(deferred.state(), model.state(), "state after {:?}", op);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 2000,
        // Failing sequences are saved here and replayed first on every run.
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(
                "tests/static_model.proptest-regressions",
            ),
        )),
        ..ProptestConfig::default()
    })]

    #[test]
    fn static_deferred_matches_model(ops in prop::collection::vec(op(), 1..60)) {
        run_ops(&ops)?;
    }
}

/// A fixed sequence through every state and leftover-task case.
#[test]
fn every_state_is_reached() {
    let ops = [
        Op::State,
        Op::Begin,
        Op::Join,
        Op::Finish(1),
        Op::WithResult,
        Op::Join,
        Op::BeginNotSpawned,
        Op::Begin,
        Op::RunTasks,
        Op::Cancel,
        Op::Begin,
        Op::Finish(2),
        Op::Take,
        Op::Begin,
        Op::RunTasks,
        Op::DropTask,
        Op::Join,
        Op::Begin,
        Op::DropTask,
        Op::Begin,
        Op::RunTasks,
        Op::CancelAndWait,
    ];
    run_ops(&ops).unwrap();
}
