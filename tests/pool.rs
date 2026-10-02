//! Spawning on a runtime with a fixed number of task slots, like an embassy task pool.

mod common;

use async_deferred::{BeginError, Deferred, SpawnError, State};
use common::{pending_until_dropped, SlotPool};
use futures_channel::oneshot;

const FULL: SpawnError = SpawnError::new("all slots are used");

#[test]
fn full_pool_returns_spawn_error() {
    let pool = SlotPool::new(1);
    let _running = Deferred::start_local_on(&pool, std::future::pending::<u32>()).unwrap();

    let started = Deferred::start_local_on(&pool, async { 42 });
    assert_eq!(started.err(), Some(FULL));

    let mut deferred = Deferred::new();
    assert_eq!(
        deferred.begin_local_on(&pool, async { 42 }),
        Err(BeginError::Spawn(FULL))
    );
    assert_eq!(deferred.state(), State::NotStarted);
}

#[test]
fn spawn_error_is_reported_by_begin_with_callback() {
    let pool = SlotPool::new(0);
    let mut deferred = Deferred::new();
    let result = deferred.begin_with_callback_local_on(&pool, async { 42 }, |_| {});
    assert_eq!(result, Err(BeginError::Spawn(FULL)));
    assert_eq!(
        Deferred::start_with_callback_local_on(&pool, async { 42 }, |_| {}).err(),
        Some(FULL)
    );
}

#[test]
fn failed_spawn_drops_the_future() {
    let pool = SlotPool::new(0);
    let (future, mut dropped) = pending_until_dropped::<u32>();
    assert!(Deferred::start_local_on(&pool, future).is_err());
    assert_eq!(dropped.try_recv(), Err(oneshot::Canceled));
}

#[test]
fn finished_task_frees_its_slot() {
    let pool = SlotPool::new(1);
    let mut deferred = Deferred::start_local_on(&pool, async { 1 }).unwrap();
    assert_eq!(pool.run_until(deferred.join()), Ok(&1));
    assert_eq!(pool.used(), 0);
    deferred.take();
    assert_eq!(deferred.begin_local_on(&pool, async { 2 }), Ok(()));
}

/// `cancel` returns before the executor has run the task again, so its slot is still
/// used and an immediate restart in a full pool fails.
#[test]
fn cancel_keeps_the_slot_until_the_executor_runs() {
    let pool = SlotPool::new(1);
    let mut deferred = Deferred::start_local_on(&pool, std::future::pending::<u32>()).unwrap();
    pool.run_once();

    assert!(deferred.cancel());
    assert_eq!(pool.used(), 1);
    assert_eq!(
        deferred.begin_local_on(&pool, async { 42 }),
        Err(BeginError::Spawn(FULL))
    );

    pool.run_once();
    assert_eq!(pool.used(), 0);
    assert_eq!(deferred.begin_local_on(&pool, async { 42 }), Ok(()));
}

#[test]
fn cancel_and_wait_frees_the_slot() {
    let pool = SlotPool::new(1);
    let mut deferred = Deferred::start_local_on(&pool, std::future::pending::<u32>()).unwrap();
    pool.run_once();

    let result = pool.run_until(async {
        assert!(deferred.cancel_and_wait().await);
        assert_eq!(deferred.state(), State::NotStarted);
        deferred.begin_local_on(&pool, async { 42 })?;
        deferred.join().await.copied().map_err(|_| unreachable!())
    });
    assert_eq!(result, Ok::<_, BeginError>(42));
}

#[test]
fn cancel_and_wait_before_the_task_first_runs() {
    let pool = SlotPool::new(1);
    let mut deferred = Deferred::start_local_on(&pool, std::future::pending::<u32>()).unwrap();
    assert!(pool.run_until(deferred.cancel_and_wait()));
    assert_eq!(pool.used(), 0);
}

#[test]
fn cancel_and_wait_does_nothing_when_not_running() {
    let pool = SlotPool::new(1);
    let mut unstarted: Deferred<u32> = Deferred::new();
    assert!(!pool.run_until(unstarted.cancel_and_wait()));

    let mut finished = Deferred::start_local_on(&pool, async { 42 }).unwrap();
    pool.run_until(finished.join()).unwrap();
    assert!(!pool.run_until(finished.cancel_and_wait()));
    assert_eq!(finished.take(), Some(42));
}

#[test]
fn errors_display() {
    assert_eq!(FULL.reason(), "all slots are used");
    assert_eq!(
        FULL.to_string(),
        "could not spawn the task: all slots are used"
    );
    assert_eq!(
        BeginError::Spawn(FULL).to_string(),
        "could not spawn the task: all slots are used"
    );
    assert_eq!(
        BeginError::AlreadyStarted.to_string(),
        "a task is running or its result has not been taken"
    );
}
