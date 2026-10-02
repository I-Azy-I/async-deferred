//! `LocalSpawner` and the `*_local_on` methods, using a single-threaded `LocalPool`.

mod common;

use std::cell::Cell;
use std::rc::Rc;

use async_deferred::{Deferred, Error, State};
use common::{pending_until_dropped, DroppingSpawner, PoolSpawner};
use futures_channel::oneshot;
use futures_executor::LocalPool;

#[test]
fn start_local_on_runs_non_send_future() {
    let mut pool = LocalPool::new();
    let shared = Rc::new(41);
    let mut deferred =
        Deferred::start_local_on(&PoolSpawner(pool.spawner()), async move { *shared + 1 });
    assert_eq!(deferred.state(), State::Pending);
    pool.run_until_stalled();
    assert_eq!(deferred.try_get(), Some(&42));
}

#[test]
fn non_send_result() {
    let mut pool = LocalPool::new();
    let mut deferred =
        Deferred::start_local_on(&PoolSpawner(pool.spawner()), async { Rc::new(42) });
    let result = pool.run_until(deferred.join()).map(|v| **v);
    assert_eq!(result, Ok(42));
}

#[test]
fn begin_local_on() {
    let mut pool = LocalPool::new();
    let spawner = PoolSpawner(pool.spawner());
    let mut deferred = Deferred::new();
    assert!(deferred.begin_local_on(&spawner, async { 42 }));
    assert!(!deferred.begin_local_on(&spawner, async { 0 }));
    assert_eq!(pool.run_until(deferred.join()), Ok(&42));
}

#[test]
fn start_with_callback_local_on_with_non_send_callback() {
    let mut pool = LocalPool::new();
    let seen = Rc::new(Cell::new(0));
    let seen_in_callback = seen.clone();
    let mut deferred = Deferred::start_with_callback_local_on(
        &PoolSpawner(pool.spawner()),
        async { 42 },
        move |v| seen_in_callback.set(*v),
    );
    pool.run_until(deferred.join()).unwrap();
    assert_eq!(seen.get(), 42);
}

#[test]
fn begin_with_callback_local_on() {
    let mut pool = LocalPool::new();
    let spawner = PoolSpawner(pool.spawner());
    let seen = Rc::new(Cell::new(0));
    let seen_in_callback = seen.clone();
    let mut deferred = Deferred::new();
    assert!(
        deferred.begin_with_callback_local_on(&spawner, async { 42 }, move |v| {
            seen_in_callback.set(*v)
        })
    );
    assert!(!deferred.begin_with_callback_local_on(&spawner, async { 0 }, |_| {}));
    pool.run_until(deferred.join()).unwrap();
    assert_eq!(seen.get(), 42);
}

#[test]
fn cancel_stops_local_task() {
    let mut pool = LocalPool::new();
    let (future, mut dropped) = pending_until_dropped::<u32>();
    let mut deferred = Deferred::start_local_on(&PoolSpawner(pool.spawner()), future);
    pool.run_until_stalled();
    assert!(deferred.cancel());
    pool.run_until_stalled();
    assert_eq!(dropped.try_recv(), Err(oneshot::Canceled));
    assert_eq!(deferred.state(), State::NotStarted);
}

#[test]
fn dropped_pool_reports_cancelled() {
    let pool = LocalPool::new();
    let mut deferred =
        Deferred::start_local_on(&PoolSpawner(pool.spawner()), std::future::pending::<u32>());
    drop(pool);
    assert_eq!(deferred.state(), State::Cancelled);
}

#[test]
fn task_dropped_by_local_spawner_reports_cancelled() {
    let mut deferred = Deferred::start_local_on(&DroppingSpawner, async { 42 });
    assert_eq!(deferred.state(), State::Cancelled);
    assert_eq!(
        futures_executor::block_on(deferred.join()),
        Err(Error::Cancelled)
    );
}

#[cfg(feature = "std")]
#[test]
fn local_task_panic() {
    let mut pool = LocalPool::new();
    let mut deferred: Deferred<u32> =
        Deferred::start_local_on(&PoolSpawner(pool.spawner()), async { panic!("boom") });
    assert_eq!(
        pool.run_until(deferred.join()),
        Err(Error::Panicked("boom".into()))
    );
}

#[cfg(feature = "std")]
#[test]
fn local_callback_panic_keeps_value() {
    let mut pool = LocalPool::new();
    let mut deferred =
        Deferred::start_with_callback_local_on(&PoolSpawner(pool.spawner()), async { 42 }, |_| {
            panic!("boom")
        });
    pool.run_until_stalled();
    assert_eq!(deferred.state(), State::CallbackPanicked);
    assert_eq!(deferred.try_get(), Some(&42));
}
