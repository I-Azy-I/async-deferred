//! Many `Deferred`s on a multi-threaded Tokio runtime, with `cancel` racing against tasks
//! finishing on other threads. Checks results, that nothing hangs, and that every value
//! is dropped exactly once.
#![cfg(feature = "tokio")]

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_deferred::{Deferred, State};
use common::{DropTracker, Tracked};

const TASKS: u64 = 20_000;

/// A small deterministic random number generator, so failures can be reproduced.
struct XorShift(u64);

impl XorShift {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

async fn yield_times(n: u64) {
    for _ in 0..n {
        tokio::task::yield_now().await;
    }
}

/// How often `cancel` won the race against the task finishing, and how often it lost.
static CANCEL_WON: AtomicUsize = AtomicUsize::new(0);
static TASK_WON: AtomicUsize = AtomicUsize::new(0);

/// Cancels, then checks the outcome is consistent whichever side won the race.
async fn cancel_and_check(deferred: &mut Deferred<Tracked<u64>>, expected: u64) {
    if deferred.cancel() {
        CANCEL_WON.fetch_add(1, Ordering::Relaxed);
        assert_eq!(deferred.state(), State::NotStarted);
        assert!(deferred.try_get().is_none());
    } else {
        TASK_WON.fetch_add(1, Ordering::Relaxed);
        // The task finished first, so its result must be there.
        assert_eq!(deferred.join().await.unwrap().value, expected);
    }
}

#[test]
fn many_tasks_with_racing_operations() {
    let tracker = DropTracker::default();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let mut drivers = Vec::new();
        for i in 0..TASKS {
            let tracker = tracker.clone();
            let restart_tracker = tracker.clone();
            drivers.push(tokio::spawn(async move {
                let mut rng = XorShift(i + 1);
                let task_yields = rng.below(4);
                let mut deferred = Deferred::start(async move {
                    yield_times(task_yields).await;
                    tracker.track(i)
                });

                match rng.below(6) {
                    // Wait for the result.
                    0 => assert_eq!(deferred.join().await.unwrap().value, i),
                    // Cancel immediately.
                    1 => cancel_and_check(&mut deferred, i).await,
                    // Cancel around the time the task finishes.
                    2 => {
                        yield_times(rng.below(4)).await;
                        cancel_and_check(&mut deferred, i).await;
                    }
                    // Poll without waiting until the result is there, then take it.
                    3 => {
                        // Read the state once per check: the task may finish at any moment.
                        loop {
                            match deferred.state() {
                                State::Pending => tokio::task::yield_now().await,
                                State::Completed => break,
                                state => panic!("unexpected state {state:?}"),
                            }
                        }
                        assert_eq!(deferred.try_get().map(|v| v.value), Some(i));
                        assert_eq!(deferred.take().unwrap().value, i);
                        assert_eq!(deferred.state(), State::NotStarted);
                    }
                    // Fire and forget.
                    4 => drop(deferred),
                    // Take the result, then reuse the `Deferred`.
                    _ => {
                        deferred.join().await.unwrap();
                        assert_eq!(deferred.take().unwrap().value, i);
                        let restarted = i + TASKS;
                        assert_eq!(
                            deferred.begin(async move { restart_tracker.track(restarted) }),
                            Ok(())
                        );
                        assert_eq!(deferred.join().await.unwrap().value, restarted);
                    }
                }
            }));
        }

        let all = async {
            for driver in drivers {
                driver.await.unwrap();
            }
        };
        tokio::time::timeout(Duration::from_secs(60), all)
            .await
            .expect("a task hung");
    });

    // Shutting down drops every task still running (the fire-and-forget ones).
    drop(runtime);
    assert!(tracker.created() > 0);
    // Both sides of the race must have happened, or the test isn't testing it.
    let (cancel_won, task_won) = (
        CANCEL_WON.load(Ordering::Relaxed),
        TASK_WON.load(Ordering::Relaxed),
    );
    eprintln!("cancel won {cancel_won} times, task won {task_won} times");
    assert!(cancel_won > 0 && task_won > 0);
    tracker.assert_all_dropped_once();
}
