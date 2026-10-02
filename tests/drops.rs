//! Every value the crate holds must be dropped exactly once, on every path:
//! the result, the future's state, and the callback.
#![cfg(feature = "alloc")]

mod common;

use std::future::Future;
use std::sync::Mutex;
use std::thread::JoinHandle;

use async_deferred::{Deferred, SpawnError, Spawner};
use common::{DropTracker, DroppingSpawner};
use futures_channel::oneshot;
use futures_executor::block_on;

/// Runs each task on its own thread, and can wait for all of them to end.
#[derive(Default)]
struct JoiningSpawner(Mutex<Vec<JoinHandle<()>>>);

impl Spawner for JoiningSpawner {
    fn spawn<F>(&self, task: F) -> Result<(), SpawnError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let handle = std::thread::spawn(move || block_on(task));
        self.0.lock().unwrap().push(handle);
        Ok(())
    }
}

impl JoiningSpawner {
    /// Waits until every task has ended and been dropped.
    fn wait_all(&self) {
        for handle in self.0.lock().unwrap().drain(..) {
            handle.join().unwrap();
        }
    }
}

#[test]
fn result_moved_out_by_take() {
    let tracker = DropTracker::default();
    let spawner = JoiningSpawner::default();
    let t = tracker.clone();
    let mut deferred = Deferred::start_on(&spawner, async move { t.track(1) }).unwrap();
    block_on(deferred.join()).unwrap();
    let value = deferred.take().unwrap();
    assert_eq!(tracker.dropped(), 0);
    drop(value);
    drop(deferred);
    spawner.wait_all();
    tracker.assert_all_dropped_once();
}

#[test]
fn result_moved_out_by_into_result() {
    let tracker = DropTracker::default();
    let spawner = JoiningSpawner::default();
    let t = tracker.clone();
    let deferred = Deferred::start_on(&spawner, async move { t.track(1) }).unwrap();
    let value = block_on(deferred.into_result()).unwrap();
    assert_eq!(tracker.dropped(), 0);
    drop(value);
    spawner.wait_all();
    tracker.assert_all_dropped_once();
}

#[test]
fn result_dropped_with_deferred() {
    let tracker = DropTracker::default();
    let spawner = JoiningSpawner::default();
    let t = tracker.clone();
    let mut deferred = Deferred::start_on(&spawner, async move { t.track(1) }).unwrap();
    block_on(deferred.join()).unwrap();
    drop(deferred);
    spawner.wait_all();
    tracker.assert_all_dropped_once();
}

#[test]
fn result_dropped_when_deferred_was_dropped_first() {
    let tracker = DropTracker::default();
    let spawner = JoiningSpawner::default();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let t = tracker.clone();
    let deferred = Deferred::start_on(&spawner, async move {
        release_rx.await.unwrap();
        t.track(1)
    })
    .unwrap();
    drop(deferred);
    release_tx.send(()).unwrap();
    spawner.wait_all();
    assert_eq!(tracker.created(), 1);
    tracker.assert_all_dropped_once();
}

#[test]
fn result_dropped_when_cancelled_after_finishing() {
    let tracker = DropTracker::default();
    let spawner = JoiningSpawner::default();
    let t = tracker.clone();
    let mut deferred = Deferred::start_on(&spawner, async move { t.track(1) }).unwrap();
    spawner.wait_all(); // finished, but the `Deferred` has not looked yet
    assert!(!deferred.cancel()); // already finished: nothing to cancel
    drop(deferred);
    tracker.assert_all_dropped_once();
}

#[test]
fn future_state_dropped_on_completion() {
    let tracker = DropTracker::default();
    let spawner = JoiningSpawner::default();
    let held = tracker.track(0);
    let mut deferred = Deferred::start_on(&spawner, async move {
        let _held = held;
        1
    })
    .unwrap();
    block_on(deferred.join()).unwrap();
    spawner.wait_all();
    tracker.assert_all_dropped_once();
}

#[test]
fn future_state_dropped_on_cancel() {
    let tracker = DropTracker::default();
    let spawner = JoiningSpawner::default();
    let held = tracker.track(0);
    let mut deferred = Deferred::start_on(&spawner, async move {
        let _held = held;
        std::future::pending::<u32>().await
    })
    .unwrap();
    assert!(deferred.cancel());
    spawner.wait_all();
    tracker.assert_all_dropped_once();
}

#[test]
fn future_state_dropped_by_runtime() {
    let tracker = DropTracker::default();
    let held = tracker.track(0);
    let deferred = Deferred::start_on(&DroppingSpawner, async move {
        let _held = held;
        1
    })
    .unwrap();
    assert_eq!(deferred.state(), async_deferred::State::Cancelled);
    tracker.assert_all_dropped_once();
}

#[test]
fn callback_dropped_after_running() {
    let tracker = DropTracker::default();
    let spawner = JoiningSpawner::default();
    let held = tracker.track(0);
    let mut deferred = Deferred::start_with_callback_on(&spawner, async { 1 }, move |_| {
        let _held = &held;
    })
    .unwrap();
    block_on(deferred.join()).unwrap();
    spawner.wait_all();
    tracker.assert_all_dropped_once();
}

#[test]
fn callback_dropped_without_running_on_cancel() {
    let tracker = DropTracker::default();
    let spawner = JoiningSpawner::default();
    let held = tracker.track(0);
    let mut deferred =
        Deferred::start_with_callback_on(&spawner, std::future::pending::<u32>(), move |_| {
            let _held = &held;
        })
        .unwrap();
    deferred.cancel();
    spawner.wait_all();
    tracker.assert_all_dropped_once();
}

#[cfg(feature = "std")]
mod panics {
    use super::*;

    #[test]
    fn future_state_dropped_when_task_panics() {
        let tracker = DropTracker::default();
        let spawner = JoiningSpawner::default();
        let held = tracker.track(0);
        let mut deferred: Deferred<u32> = Deferred::start_on(&spawner, async move {
            let _held = held;
            panic!("boom")
        })
        .unwrap();
        block_on(deferred.join()).unwrap_err();
        spawner.wait_all();
        tracker.assert_all_dropped_once();
    }

    #[test]
    fn result_and_callback_dropped_when_callback_panics() {
        let tracker = DropTracker::default();
        let spawner = JoiningSpawner::default();
        let held = tracker.track(0);
        let t = tracker.clone();
        let mut deferred =
            Deferred::start_with_callback_on(&spawner, async move { t.track(1) }, move |_| {
                let _held = &held;
                panic!("boom")
            })
            .unwrap();
        assert_eq!(block_on(deferred.join()).unwrap().value, 1);
        drop(deferred);
        spawner.wait_all();
        assert_eq!(tracker.created(), 2);
        tracker.assert_all_dropped_once();
    }
}

#[test]
fn replaced_result_dropped_on_restart() {
    let tracker = DropTracker::default();
    let spawner = JoiningSpawner::default();
    let mut deferred = Deferred::new();
    for i in 0..10 {
        let t = tracker.clone();
        deferred
            .begin_on(&spawner, async move { t.track(i) })
            .unwrap();
        block_on(deferred.join()).unwrap();
        drop(deferred.take());
    }
    spawner.wait_all();
    assert_eq!(tracker.created(), 10);
    tracker.assert_all_dropped_once();
}
