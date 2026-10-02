//! Behaviour of `StaticDeferred`, the heap-free variant kept in a `static`.
#![cfg(feature = "static-deferred")]

mod common;

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use async_deferred::{BeginError, Error, State, StaticDeferred, Ticket};
use common::{pending_until_dropped, DropTracker};
use futures_channel::oneshot;
use futures_executor::{block_on, LocalPool};
use futures_util::task::{noop_waker_ref, LocalSpawnExt};
use futures_util::FutureExt;

/// A single-threaded executor the test drives by hand, with a fixed number of task slots
/// like an embassy task pool. A finished task frees its slot when it is polled.
struct Executor<'a> {
    slots: usize,
    tasks: Vec<Option<Pin<Box<dyn Future<Output = ()> + 'a>>>>,
}

impl<'a> Executor<'a> {
    fn new(slots: usize) -> Self {
        Self {
            slots,
            tasks: Vec::new(),
        }
    }

    fn used(&self) -> usize {
        self.tasks.iter().filter(|t| t.is_some()).count()
    }

    /// Spawns `ticket.run(future)`, or drops the ticket if every slot is taken.
    fn spawn<T: 'a>(
        &mut self,
        ticket: Ticket<'a, T>,
        future: impl Future<Output = T> + 'a,
    ) -> Result<(), ()> {
        if self.used() == self.slots {
            return Err(());
        }
        self.tasks.push(Some(Box::pin(ticket.run(future))));
        Ok(())
    }

    /// Polls every task once.
    fn run(&mut self) {
        let mut cx = Context::from_waker(noop_waker_ref());
        for task in &mut self.tasks {
            if let Some(future) = task {
                if future.as_mut().poll(&mut cx).is_ready() {
                    *task = None;
                }
            }
        }
    }

    /// Polls `future`, running the tasks in between, until it finishes.
    fn run_until<T>(&mut self, future: impl Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        let mut cx = Context::from_waker(noop_waker_ref());
        for _ in 0..1000 {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return value;
            }
            self.run();
        }
        panic!("the future did not finish");
    }
}

/// Checks every query against the expected state.
fn assert_state<T>(deferred: &StaticDeferred<T>, expected: State) {
    assert_eq!(deferred.state(), expected);
    assert_eq!(deferred.is_not_started(), expected == State::NotStarted);
    assert_eq!(deferred.is_pending(), expected == State::Pending);
    assert_eq!(deferred.is_ready(), expected == State::Completed);
    assert_eq!(deferred.is_cancelled(), expected == State::Cancelled);
    assert_eq!(
        deferred.with_result(|_| ()).is_some(),
        expected == State::Completed
    );
}

#[test]
fn works_as_a_static() {
    static VALUE: StaticDeferred<u32> = StaticDeferred::new();
    let ticket = VALUE.begin().unwrap();
    let mut pool = LocalPool::new();
    pool.spawner()
        .spawn_local(ticket.run(async { 42 }))
        .unwrap();
    assert_state(&VALUE, State::Pending);
    pool.run_until_stalled();
    assert_state(&VALUE, State::Completed);
    assert_eq!(VALUE.take(), Some(42));
    assert_state(&VALUE, State::NotStarted);
}

#[test]
fn new_is_not_started() {
    let deferred: StaticDeferred<u32> = StaticDeferred::default();
    assert_state(&deferred, State::NotStarted);
    assert_eq!(deferred.take(), None);
    assert!(!deferred.cancel());
    assert!(!block_on(deferred.cancel_and_wait()));
    assert_eq!(block_on(deferred.join()), Err(Error::NotStarted));
}

#[test]
fn begin_is_rejected_while_running_or_done() {
    let deferred = StaticDeferred::new();
    let mut executor = Executor::new(4);
    executor
        .spawn(deferred.begin().unwrap(), async { 1 })
        .unwrap();
    assert!(matches!(deferred.begin(), Err(BeginError::AlreadyStarted)));
    executor.run();
    assert_state(&deferred, State::Completed);
    assert!(matches!(deferred.begin(), Err(BeginError::AlreadyStarted)));
    assert_eq!(deferred.take(), Some(1));
    assert!(deferred.begin().is_ok());
}

#[test]
fn join_waits_and_moves_the_result_out() {
    let deferred = StaticDeferred::new();
    let mut executor = Executor::new(1);
    let (tx, rx) = oneshot::channel();
    executor
        .spawn(deferred.begin().unwrap(), async { rx.await.unwrap() })
        .unwrap();
    assert!(deferred.join().now_or_never().is_none());
    tx.send(42).unwrap();
    assert_eq!(executor.run_until(deferred.join()), Ok(42));
    assert_state(&deferred, State::NotStarted);
}

#[test]
fn with_result_reads_without_moving() {
    let deferred = StaticDeferred::new();
    let mut executor = Executor::new(1);
    executor
        .spawn(deferred.begin().unwrap(), async { String::from("done") })
        .unwrap();
    executor.run();
    assert_eq!(deferred.with_result(|v| v.len()), Some(4));
    assert_eq!(deferred.take().as_deref(), Some("done"));
}

#[test]
fn dropped_ticket_resets() {
    let deferred = StaticDeferred::<u32>::new();
    let ticket = deferred.begin().unwrap();
    assert_state(&deferred, State::Pending);
    drop(ticket); // e.g. the task could not be spawned
    assert_state(&deferred, State::NotStarted);
    assert!(deferred.begin().is_ok());
}

#[test]
fn full_pool_drops_the_ticket() {
    let deferred = StaticDeferred::<u32>::new();
    let mut executor = Executor::new(0);
    assert!(executor
        .spawn(deferred.begin().unwrap(), async { 1 })
        .is_err());
    assert_state(&deferred, State::NotStarted);
}

#[test]
fn task_dropped_while_running_reports_cancelled() {
    let deferred = StaticDeferred::<u32>::new();
    let mut executor = Executor::new(1);
    executor
        .spawn(deferred.begin().unwrap(), std::future::pending())
        .unwrap();
    executor.run();
    drop(executor);
    assert_state(&deferred, State::Cancelled);
    assert_eq!(block_on(deferred.join()), Err(Error::Cancelled));
    // A new run can start after a cancelled one.
    assert!(deferred.begin().is_ok());
}

#[test]
fn cancel_stops_the_task() {
    let deferred = StaticDeferred::<u32>::new();
    let mut executor = Executor::new(1);
    let (future, mut dropped) = pending_until_dropped::<u32>();
    executor.spawn(deferred.begin().unwrap(), future).unwrap();
    executor.run();
    assert!(deferred.cancel());
    assert_state(&deferred, State::NotStarted);
    assert_eq!(executor.used(), 1); // stops the next time it runs
    executor.run();
    assert_eq!(executor.used(), 0);
    assert_eq!(dropped.try_recv(), Err(oneshot::Canceled));
}

#[test]
fn cancel_before_the_task_runs_skips_the_job() {
    let deferred = StaticDeferred::<u32>::new();
    let mut executor = Executor::new(1);
    let polled = Rc::new(Cell::new(false));
    let polled_in_job = polled.clone();
    executor
        .spawn(deferred.begin().unwrap(), async move {
            polled_in_job.set(true);
            1
        })
        .unwrap();
    assert!(deferred.cancel());
    executor.run();
    assert_eq!(executor.used(), 0);
    assert!(!polled.get());
    assert_state(&deferred, State::NotStarted);
}

#[test]
fn cancel_and_wait_frees_the_slot() {
    let deferred = StaticDeferred::<u32>::new();
    let mut executor = Executor::new(1);
    executor
        .spawn(deferred.begin().unwrap(), std::future::pending())
        .unwrap();
    executor.run();

    // Right after `cancel`, the old task still holds the only slot.
    assert!(deferred.cancel());
    assert!(executor
        .spawn(deferred.begin().unwrap(), async { 2 })
        .is_err());

    assert!(!executor.run_until(deferred.cancel_and_wait()));
    assert_eq!(executor.used(), 0);
    executor
        .spawn(deferred.begin().unwrap(), async { 3 })
        .unwrap();
    assert_eq!(executor.run_until(deferred.join()), Ok(3));
}

#[test]
fn cancel_and_wait_on_a_running_task() {
    let deferred = StaticDeferred::<u32>::new();
    let mut executor = Executor::new(1);
    executor
        .spawn(deferred.begin().unwrap(), std::future::pending())
        .unwrap();
    executor.run();
    assert!(executor.run_until(deferred.cancel_and_wait()));
    assert_eq!(executor.used(), 0);
    assert_state(&deferred, State::NotStarted);
}

#[test]
fn stale_task_cannot_overwrite_a_new_run() {
    let deferred = StaticDeferred::new();
    let mut executor = Executor::new(2);
    let (old_tx, old_rx) = oneshot::channel::<u32>();
    executor
        .spawn(deferred.begin().unwrap(), async { old_rx.await.unwrap() })
        .unwrap();
    executor.run();
    deferred.cancel();

    let (new_tx, new_rx) = oneshot::channel::<u32>();
    executor
        .spawn(deferred.begin().unwrap(), async { new_rx.await.unwrap() })
        .unwrap();
    let _ = old_tx.send(1); // the old job would finish now
    executor.run();
    assert_state(&deferred, State::Pending);
    new_tx.send(2).unwrap();
    executor.run();
    assert_eq!(deferred.take(), Some(2));
}

#[test]
fn two_waiters_both_finish() {
    let deferred = StaticDeferred::new();
    let mut executor = Executor::new(1);
    let (tx, rx) = oneshot::channel();
    executor
        .spawn(deferred.begin().unwrap(), async { rx.await.unwrap() })
        .unwrap();
    let both = futures_util::future::join(deferred.join(), deferred.join());
    let mut both = std::pin::pin!(both);
    let mut cx = Context::from_waker(noop_waker_ref());
    assert!(both.as_mut().poll(&mut cx).is_pending());
    tx.send(42).unwrap();
    let (a, b) = executor.run_until(both);
    // One waiter gets the value; the other finds it already taken.
    let mut results = [a, b];
    results.sort_by_key(|r| r.is_err());
    assert_eq!(results, [Ok(42), Err(Error::NotStarted)]);
}

#[test]
fn runs_across_threads() {
    static VALUE: StaticDeferred<u64> = StaticDeferred::new();
    for i in 0..200 {
        let ticket = VALUE.begin().unwrap();
        let handle = std::thread::spawn(move || block_on(ticket.run(async move { i * 2 })));
        assert_eq!(block_on(VALUE.join()), Ok(i * 2));
        handle.join().unwrap();
    }
}

#[test]
fn every_value_is_dropped_once() {
    let tracker = DropTracker::default();
    {
        let deferred = StaticDeferred::new();
        let mut executor = Executor::new(2);

        // Taken.
        let t = tracker.clone();
        executor
            .spawn(deferred.begin().unwrap(), async move { t.track(1) })
            .unwrap();
        executor.run();
        drop(deferred.take());

        // Finished after being cancelled: discarded.
        let (tx, rx) = oneshot::channel::<()>();
        let t = tracker.clone();
        executor
            .spawn(deferred.begin().unwrap(), async move {
                let value = t.track(2);
                rx.await.unwrap();
                value
            })
            .unwrap();
        executor.run();
        deferred.cancel();
        tx.send(()).unwrap();
        executor.run();

        // Left in the `StaticDeferred` when it is dropped.
        let t = tracker.clone();
        executor
            .spawn(deferred.begin().unwrap(), async move { t.track(3) })
            .unwrap();
        executor.run();
        assert!(deferred.is_ready());
    }
    assert_eq!(tracker.created(), 3);
    tracker.assert_all_dropped_once();
}

#[test]
fn debug_shows_the_state() {
    let deferred = StaticDeferred::<u32>::new();
    assert_eq!(
        format!("{deferred:?}"),
        "StaticDeferred { state: NotStarted, .. }"
    );
}
