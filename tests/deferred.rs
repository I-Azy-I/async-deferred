//! Behaviour of `Deferred` that does not depend on the runtime, using a thread per task.
#![cfg(feature = "alloc")]

mod common;

use std::cell::Cell;
use std::sync::{Arc, Mutex};

use async_deferred::{BeginError, Deferred, Error, State};
use common::{pending_until_dropped, wait_until_finished, DroppingSpawner, Flag, ThreadSpawner};
use futures_channel::oneshot;
use futures_executor::block_on;
use futures_util::FutureExt;

/// Checks every query method against the expected state.
fn assert_state<T: std::fmt::Debug + PartialEq>(deferred: &mut Deferred<T>, expected: State) {
    assert_eq!(deferred.state(), expected);
    assert_eq!(deferred.is_not_started(), expected == State::NotStarted);
    assert_eq!(deferred.is_pending(), expected == State::Pending);
    assert_eq!(deferred.is_complete(), expected == State::Completed);
    assert_eq!(
        deferred.has_task_panicked(),
        expected == State::TaskPanicked
    );
    assert_eq!(
        deferred.has_callback_panicked(),
        expected == State::CallbackPanicked
    );
    assert_eq!(deferred.is_cancelled(), expected == State::Cancelled);
    let has_value = matches!(expected, State::Completed | State::CallbackPanicked);
    assert_eq!(deferred.is_ready(), has_value);
    assert_eq!(deferred.try_get().is_some(), has_value);
    let panicked = matches!(expected, State::TaskPanicked | State::CallbackPanicked);
    assert_eq!(deferred.panic_message().is_some(), panicked);
}

// --- construction ---

#[test]
fn new_is_not_started() {
    let mut deferred: Deferred<u32> = Deferred::new();
    assert_state(&mut deferred, State::NotStarted);
    assert_eq!(deferred.take(), None);
    assert!(!deferred.cancel());
    assert_eq!(block_on(deferred.join()), Err(Error::NotStarted));
}

#[test]
fn default_is_not_started() {
    let mut deferred: Deferred<u32> = Deferred::default();
    assert_state(&mut deferred, State::NotStarted);
}

// --- running and completing ---

#[test]
fn pending_then_completed() {
    let (tx, rx) = oneshot::channel();
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { rx.await.unwrap() }).unwrap();
    assert_state(&mut deferred, State::Pending);

    tx.send(42).unwrap();
    assert_eq!(block_on(deferred.join()), Ok(&42));
    assert_state(&mut deferred, State::Completed);
    assert_eq!(deferred.try_get(), Some(&42));
}

#[test]
fn begin_on_starts_an_empty_deferred() {
    let mut deferred = Deferred::new();
    assert_eq!(deferred.begin_on(&ThreadSpawner, async { 42 }), Ok(()));
    assert_eq!(block_on(deferred.join()), Ok(&42));
}

#[test]
fn spawner_behind_a_reference_or_pointer() {
    let mut by_ref = Deferred::start_on(&&ThreadSpawner, async { 1 }).unwrap();
    let mut boxed = Deferred::start_on(&Box::new(ThreadSpawner), async { 2 }).unwrap();
    let mut shared = Deferred::start_on(&Arc::new(ThreadSpawner), async { 3 }).unwrap();
    assert_eq!(block_on(by_ref.join()), Ok(&1));
    assert_eq!(block_on(boxed.join()), Ok(&2));
    assert_eq!(block_on(shared.join()), Ok(&3));
}

#[test]
fn result_available_without_join() {
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { 42 }).unwrap();
    wait_until_finished(&mut deferred);
    assert_eq!(deferred.try_get(), Some(&42));
}

#[test]
fn join_can_be_called_again() {
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { 42 }).unwrap();
    assert_eq!(block_on(deferred.join()), Ok(&42));
    assert_eq!(block_on(deferred.join()), Ok(&42));
}

#[test]
fn begin_is_rejected_while_running() {
    let (tx, rx) = oneshot::channel();
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { rx.await.unwrap() }).unwrap();
    assert_eq!(
        deferred.begin_on(&ThreadSpawner, async { 0 }),
        Err(BeginError::AlreadyStarted)
    );
    tx.send(42).unwrap();
    assert_eq!(block_on(deferred.join()), Ok(&42));
}

#[test]
fn begin_is_rejected_until_result_is_taken() {
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { 42 }).unwrap();
    block_on(deferred.join()).unwrap();
    assert_eq!(
        deferred.begin_on(&ThreadSpawner, async { 0 }),
        Err(BeginError::AlreadyStarted)
    );
    assert_eq!(deferred.try_get(), Some(&42));
}

#[test]
fn non_sync_result() {
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { Cell::new(42) }).unwrap();
    assert_eq!(block_on(deferred.join()).map(Cell::get), Ok(42));
}

// --- status through a shared reference ---

fn describe(deferred: &Deferred<u32>) -> &'static str {
    if deferred.is_pending() {
        "busy"
    } else if deferred.is_ready() {
        "done"
    } else {
        "idle"
    }
}

#[test]
fn status_through_a_shared_reference() {
    let (tx, rx) = oneshot::channel();
    let mut deferred = Deferred::new();
    assert_eq!(describe(&deferred), "idle");
    deferred
        .begin_on(&ThreadSpawner, async { rx.await.unwrap() })
        .unwrap();
    assert_eq!(describe(&deferred), "busy");
    tx.send(42).unwrap();
    while describe(&deferred) == "busy" {
        std::thread::yield_now();
    }
    assert_eq!(describe(&deferred), "done");
    assert_eq!(deferred.take(), Some(42));
}

#[test]
fn status_matches_after_the_result_is_received() {
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { 42 }).unwrap();
    while deferred.is_pending() {
        std::thread::yield_now();
    }
    assert_eq!(deferred.state(), State::Completed);
    assert_eq!(deferred.try_get(), Some(&42));
    assert_eq!(deferred.state(), State::Completed);
}

// --- owned result ---

#[test]
fn into_result_returns_the_owned_value() {
    let deferred = Deferred::start_on(&ThreadSpawner, async { vec![1, 2, 3] }).unwrap();
    assert_eq!(block_on(deferred.into_result()), Ok(vec![1, 2, 3]));
}

#[test]
fn await_deferred_directly() {
    let deferred = Deferred::start_on(&ThreadSpawner, async { 42 }).unwrap();
    assert_eq!(block_on(async { deferred.await }), Ok(42));
}

#[test]
fn into_result_after_join() {
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { 42 }).unwrap();
    block_on(deferred.join()).unwrap();
    assert_eq!(block_on(deferred.into_result()), Ok(42));
}

#[test]
fn into_result_not_started_and_taken() {
    let unstarted: Deferred<u32> = Deferred::new();
    assert_eq!(block_on(unstarted.into_result()), Err(Error::NotStarted));

    let mut taken = Deferred::start_on(&ThreadSpawner, async { 42 }).unwrap();
    block_on(taken.join()).unwrap();
    taken.take();
    assert_eq!(block_on(taken.into_result()), Err(Error::NotStarted));
}

#[test]
fn into_result_task_dropped_by_runtime() {
    let deferred = Deferred::start_on(&DroppingSpawner, async { 42 }).unwrap();
    assert_eq!(block_on(deferred.into_result()), Err(Error::Cancelled));
}

#[test]
fn dropping_into_result_keeps_task_running() {
    let (start_tx, start_rx) = oneshot::channel::<()>();
    let (done_tx, done_rx) = oneshot::channel();
    let deferred = Deferred::start_on(&ThreadSpawner, async move {
        start_rx.await.unwrap();
        done_tx.send(42).unwrap();
    })
    .unwrap();
    assert!(deferred.into_result().now_or_never().is_none());
    start_tx.send(()).unwrap();
    assert_eq!(block_on(done_rx), Ok(42));
}

// --- take ---

#[test]
fn take_moves_result_out_and_resets() {
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { vec![1, 2, 3] }).unwrap();
    block_on(deferred.join()).unwrap();
    assert_eq!(deferred.take(), Some(vec![1, 2, 3]));
    assert_eq!(deferred.take(), None);
    assert_state(&mut deferred, State::NotStarted);
    assert_eq!(block_on(deferred.join()), Err(Error::NotStarted));
}

#[test]
fn take_without_join() {
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { 42 }).unwrap();
    wait_until_finished(&mut deferred);
    assert_eq!(deferred.take(), Some(42));
}

#[test]
fn take_while_pending_does_not_consume() {
    let (tx, rx) = oneshot::channel();
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { rx.await.unwrap() }).unwrap();
    assert_eq!(deferred.take(), None);
    assert_state(&mut deferred, State::Pending);
    tx.send(42).unwrap();
    block_on(deferred.join()).unwrap();
    assert_eq!(deferred.take(), Some(42));
}

#[test]
fn restart_after_take() {
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { 1 }).unwrap();
    block_on(deferred.join()).unwrap();
    deferred.take();
    assert_eq!(deferred.begin_on(&ThreadSpawner, async { 2 }), Ok(()));
    assert_eq!(block_on(deferred.join()), Ok(&2));
}

// --- callbacks ---

#[test]
fn callback_receives_result_before_join_returns() {
    let seen = Arc::new(Mutex::new(None));
    let seen_in_callback = seen.clone();
    let mut deferred = Deferred::start_with_callback_on(&ThreadSpawner, async { 42 }, move |v| {
        *seen_in_callback.lock().unwrap() = Some(*v);
    })
    .unwrap();
    block_on(deferred.join()).unwrap();
    assert_eq!(*seen.lock().unwrap(), Some(42));
}

#[test]
fn begin_with_callback_on() {
    let ran = Flag::default();
    let ran_in_callback = ran.clone();
    let mut deferred = Deferred::new();
    assert_eq!(
        deferred.begin_with_callback_on(&ThreadSpawner, async { 42 }, move |_| {
            ran_in_callback.set()
        }),
        Ok(())
    );
    block_on(deferred.join()).unwrap();
    assert!(ran.is_set());
    assert_state(&mut deferred, State::Completed);
}

#[test]
fn begin_with_callback_is_rejected_while_running() {
    let (tx, rx) = oneshot::channel();
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { rx.await.unwrap() }).unwrap();
    let ran = Flag::default();
    let ran_in_callback = ran.clone();
    assert_eq!(
        deferred.begin_with_callback_on(&ThreadSpawner, async { 0 }, move |_| {
            ran_in_callback.set()
        }),
        Err(BeginError::AlreadyStarted)
    );
    tx.send(42).unwrap();
    assert_eq!(block_on(deferred.join()), Ok(&42));
    assert!(!ran.is_set());
}

// --- cancel ---

#[test]
fn cancel_stops_the_task_and_resets() {
    let (future, dropped) = pending_until_dropped::<u32>();
    let mut deferred = Deferred::start_on(&ThreadSpawner, future).unwrap();
    assert!(deferred.cancel());
    assert_state(&mut deferred, State::NotStarted);
    assert_eq!(block_on(dropped), Err(oneshot::Canceled));
}

#[test]
fn cancel_and_wait_returns_after_the_task_stopped() {
    let (future, mut dropped) = pending_until_dropped::<u32>();
    let mut deferred = Deferred::start_on(&ThreadSpawner, future).unwrap();
    assert!(block_on(deferred.cancel_and_wait()));
    // The task's future was already dropped when `cancel_and_wait` returned.
    assert_eq!(dropped.try_recv(), Err(oneshot::Canceled));
    assert_state(&mut deferred, State::NotStarted);
}

#[test]
fn cancel_and_wait_after_the_task_finished() {
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { 42 }).unwrap();
    wait_until_finished(&mut deferred);
    assert!(!block_on(deferred.cancel_and_wait()));
    assert_eq!(deferred.take(), Some(42));
}

#[test]
fn cancel_does_not_run_the_callback() {
    let (future, dropped) = pending_until_dropped::<u32>();
    let ran = Flag::default();
    let ran_in_callback = ran.clone();
    let mut deferred =
        Deferred::start_with_callback_on(&ThreadSpawner, future, move |_| ran_in_callback.set())
            .unwrap();
    deferred.cancel();
    block_on(dropped).unwrap_err();
    assert!(!ran.is_set());
}

#[test]
fn restart_after_cancel() {
    let mut deferred = Deferred::start_on(&ThreadSpawner, std::future::pending::<u32>()).unwrap();
    deferred.cancel();
    assert_eq!(deferred.begin_on(&ThreadSpawner, async { 100 }), Ok(()));
    assert_eq!(block_on(deferred.join()), Ok(&100));
}

#[test]
fn cancel_does_nothing_when_not_running() {
    let mut unstarted: Deferred<u32> = Deferred::new();
    assert!(!unstarted.cancel());

    let mut finished = Deferred::start_on(&ThreadSpawner, async { 42 }).unwrap();
    block_on(finished.join()).unwrap();
    assert!(!finished.cancel());
    assert_eq!(finished.take(), Some(42));
}

// --- dropping ---

#[test]
fn dropping_deferred_keeps_task_running() {
    let (start_tx, start_rx) = oneshot::channel::<()>();
    let (done_tx, done_rx) = oneshot::channel();
    let deferred = Deferred::start_on(&ThreadSpawner, async move {
        start_rx.await.unwrap();
        done_tx.send(42).unwrap();
    })
    .unwrap();
    drop(deferred);
    start_tx.send(()).unwrap();
    assert_eq!(block_on(done_rx), Ok(42));
}

#[test]
fn dropped_join_future_keeps_tracking_the_task() {
    let (tx, rx) = oneshot::channel();
    let mut deferred = Deferred::start_on(&ThreadSpawner, async { rx.await.unwrap() }).unwrap();
    assert!(deferred.join().now_or_never().is_none());
    assert_state(&mut deferred, State::Pending);
    tx.send(42).unwrap();
    assert_eq!(block_on(deferred.join()), Ok(&42));
}

#[test]
fn task_dropped_by_runtime_reports_cancelled() {
    let mut deferred = Deferred::start_on(&DroppingSpawner, async { 42 }).unwrap();
    assert_state(&mut deferred, State::Cancelled);
    assert_eq!(block_on(deferred.join()), Err(Error::Cancelled));
    assert_eq!(deferred.take(), None);
    assert!(!deferred.cancel());
}

#[test]
fn restart_after_task_dropped_by_runtime() {
    let mut deferred = Deferred::start_on(&DroppingSpawner, async { 1 }).unwrap();
    assert_eq!(deferred.state(), State::Cancelled);
    assert_eq!(deferred.begin_on(&ThreadSpawner, async { 2 }), Ok(()));
    assert_eq!(block_on(deferred.join()), Ok(&2));
}

#[test]
fn task_dropped_by_runtime_does_not_run_callback() {
    let ran = Flag::default();
    let ran_in_callback = ran.clone();
    let deferred = Deferred::start_with_callback_on(&DroppingSpawner, async { 42 }, move |_| {
        ran_in_callback.set()
    })
    .unwrap();
    assert_eq!(deferred.state(), State::Cancelled);
    assert!(!ran.is_set());
}

// --- panics ---

#[cfg(feature = "std")]
mod panics {
    use super::*;

    #[test]
    fn task_panic_with_str() {
        let mut deferred: Deferred<u32> =
            Deferred::start_on(&ThreadSpawner, async { panic!("the task panicked") }).unwrap();
        assert_eq!(
            block_on(deferred.join()),
            Err(Error::Panicked("the task panicked".into()))
        );
        assert_state(&mut deferred, State::TaskPanicked);
        assert_eq!(deferred.panic_message(), Some("the task panicked"));
    }

    #[test]
    fn task_panic_with_formatted_message() {
        let code = 7;
        let mut deferred: Deferred<u32> =
            Deferred::start_on(&ThreadSpawner, async move { panic!("code {code}") }).unwrap();
        assert_eq!(
            block_on(deferred.join()),
            Err(Error::Panicked("code 7".into()))
        );
    }

    #[test]
    fn task_panic_with_other_payload() {
        let mut deferred: Deferred<u32> =
            Deferred::start_on(&ThreadSpawner, async { std::panic::panic_any(5u8) }).unwrap();
        assert_eq!(
            block_on(deferred.join()),
            Err(Error::Panicked("unknown panic payload".into()))
        );
    }

    #[test]
    fn task_panic_is_seen_without_join() {
        let mut deferred: Deferred<u32> =
            Deferred::start_on(&ThreadSpawner, async { panic!("boom") }).unwrap();
        wait_until_finished(&mut deferred);
        assert_state(&mut deferred, State::TaskPanicked);
    }

    #[test]
    fn task_panic_skips_callback() {
        let ran = Flag::default();
        let ran_in_callback = ran.clone();
        let mut deferred: Deferred<u32> =
            Deferred::start_with_callback_on(&ThreadSpawner, async { panic!("boom") }, move |_| {
                ran_in_callback.set()
            })
            .unwrap();
        block_on(deferred.join()).unwrap_err();
        assert!(!ran.is_set());
    }

    #[test]
    fn restart_after_task_panic() {
        let mut deferred: Deferred<u32> =
            Deferred::start_on(&ThreadSpawner, async { panic!("boom") }).unwrap();
        block_on(deferred.join()).unwrap_err();
        assert_eq!(deferred.begin_on(&ThreadSpawner, async { 2 }), Ok(()));
        assert_eq!(deferred.panic_message(), None);
        assert_eq!(block_on(deferred.join()), Ok(&2));
    }

    #[test]
    fn take_and_cancel_after_task_panic() {
        let mut deferred: Deferred<u32> =
            Deferred::start_on(&ThreadSpawner, async { panic!("boom") }).unwrap();
        block_on(deferred.join()).unwrap_err();
        assert_eq!(deferred.take(), None);
        assert!(!deferred.cancel());
        assert_state(&mut deferred, State::TaskPanicked);
    }

    #[test]
    fn callback_panic_keeps_value() {
        let mut deferred = Deferred::start_with_callback_on(&ThreadSpawner, async { 42 }, |_| {
            panic!("the callback panicked")
        })
        .unwrap();
        wait_until_finished(&mut deferred);
        assert_state(&mut deferred, State::CallbackPanicked);
        assert_eq!(deferred.panic_message(), Some("the callback panicked"));
        assert_eq!(block_on(deferred.join()), Ok(&42));
    }

    #[test]
    fn into_result_after_task_panic() {
        let deferred: Deferred<u32> =
            Deferred::start_on(&ThreadSpawner, async { panic!("boom") }).unwrap();
        assert_eq!(
            block_on(deferred.into_result()),
            Err(Error::Panicked("boom".into()))
        );
    }

    #[test]
    fn into_result_after_callback_panic_returns_value() {
        let deferred =
            Deferred::start_with_callback_on(&ThreadSpawner, async { 42 }, |_| panic!("boom"))
                .unwrap();
        assert_eq!(block_on(deferred.into_result()), Ok(42));
    }

    #[test]
    fn take_after_callback_panic_resets() {
        let mut deferred =
            Deferred::start_with_callback_on(&ThreadSpawner, async { 42 }, |_| panic!("boom"))
                .unwrap();
        block_on(deferred.join()).unwrap();
        assert!(!deferred.cancel());
        assert_eq!(deferred.take(), Some(42));
        assert_state(&mut deferred, State::NotStarted);
    }
}

/// Without `std`, panics are not caught: the thread running the task unwinds and drops it.
#[cfg(not(feature = "std"))]
#[test]
fn task_panic_without_std_reports_cancelled() {
    let mut deferred: Deferred<u32> =
        Deferred::start_on(&ThreadSpawner, async { panic!("boom") }).unwrap();
    assert_eq!(block_on(deferred.join()), Err(Error::Cancelled));
}

// --- Error ---

#[test]
fn error_display() {
    assert_eq!(Error::NotStarted.to_string(), "no task has been started");
    assert_eq!(
        Error::Panicked("boom".into()).to_string(),
        "task panicked: boom"
    );
    assert_eq!(Error::Cancelled.to_string(), "task was cancelled");
}

#[cfg(feature = "std")]
#[test]
fn error_is_std_error() {
    let err: Box<dyn std::error::Error> = Box::new(Error::Cancelled);
    assert_eq!(err.to_string(), "task was cancelled");
}

// --- auto traits ---

#[test]
fn auto_traits() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    assert_send::<Deferred<u32>>();
    assert_sync::<Deferred<u32>>();
    assert_send::<Deferred<Cell<u32>>>();
    assert_send::<Error>();
    assert_sync::<Error>();
}
