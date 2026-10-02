//! The `tokio` feature: the `Tokio` spawner, `Handle`, and the `start`/`begin` shortcuts.
#![cfg(feature = "tokio")]

mod common;

use async_deferred::{BeginError, Deferred, Error, State, Tokio};
use common::{pending_until_dropped, Flag};
use tokio::sync::oneshot;

#[tokio::test]
async fn start() {
    let mut deferred = Deferred::start(async { 42 });
    assert_eq!(deferred.join().await, Ok(&42));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_on_multi_thread_runtime() {
    let (tx, rx) = oneshot::channel();
    let mut deferred = Deferred::start(async { rx.await.unwrap() });
    assert_eq!(deferred.state(), State::Pending);
    tx.send(42).unwrap();
    assert_eq!(deferred.join().await, Ok(&42));
}

#[tokio::test]
async fn start_with_callback() {
    let ran = Flag::default();
    let ran_in_callback = ran.clone();
    let mut deferred = Deferred::start_with_callback(async { 42 }, move |v| {
        assert_eq!(*v, 42);
        ran_in_callback.set();
    });
    deferred.join().await.unwrap();
    assert!(ran.is_set());
}

#[tokio::test]
async fn begin() {
    let mut deferred = Deferred::new();
    assert_eq!(deferred.begin(async { 42 }), Ok(()));
    assert_eq!(deferred.begin(async { 0 }), Err(BeginError::AlreadyStarted));
    assert_eq!(deferred.join().await, Ok(&42));
}

#[tokio::test]
async fn begin_with_callback() {
    let ran = Flag::default();
    let ran_in_callback = ran.clone();
    let mut deferred = Deferred::new();
    assert_eq!(
        deferred.begin_with_callback(async { 42 }, move |_| ran_in_callback.set()),
        Ok(())
    );
    assert_eq!(
        deferred.begin_with_callback(async { 0 }, |_| {}),
        Err(BeginError::AlreadyStarted)
    );
    deferred.join().await.unwrap();
    assert!(ran.is_set());
}

#[tokio::test]
async fn start_on_tokio_spawner() {
    let mut deferred = Deferred::start_on(&Tokio, async { 42 }).unwrap();
    assert_eq!(deferred.join().await, Ok(&42));
}

#[tokio::test]
async fn result_available_without_join() {
    let mut deferred = Deferred::start(async { 42 });
    while deferred.is_pending() {
        tokio::task::yield_now().await;
    }
    assert_eq!(deferred.try_get(), Some(&42));
}

#[tokio::test]
async fn cancel_stops_the_task() {
    let (future, dropped) = pending_until_dropped::<u32>();
    let mut deferred = Deferred::start(future);
    assert!(deferred.cancel());
    assert!(dropped.await.is_err());
    assert_eq!(deferred.state(), State::NotStarted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_and_wait_returns_after_the_task_stopped() {
    for _ in 0..1000 {
        let (future, mut dropped) = pending_until_dropped::<u32>();
        let mut deferred = Deferred::start(future);
        tokio::task::yield_now().await;
        assert!(deferred.cancel_and_wait().await);
        assert!(dropped.try_recv().is_err(), "the task was still running");
        assert_eq!(deferred.state(), State::NotStarted);
    }
}

#[tokio::test]
async fn task_panic() {
    let mut deferred: Deferred<u32> = Deferred::start(async { panic!("boom") });
    assert_eq!(deferred.join().await, Err(Error::Panicked("boom".into())));
}

#[tokio::test]
async fn callback_panic_keeps_value() {
    let mut deferred = Deferred::start_with_callback(async { 42 }, |_| panic!("boom"));
    assert_eq!(deferred.join().await, Ok(&42));
    assert_eq!(deferred.state(), State::CallbackPanicked);
}

#[test]
fn start_on_handle_from_outside_runtime() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut deferred = Deferred::start_on(runtime.handle(), async { 42 }).unwrap();
    assert_eq!(runtime.block_on(deferred.join()), Ok(&42));
}

#[test]
fn runtime_shutdown_reports_cancelled() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut deferred = Deferred::start_on(runtime.handle(), std::future::pending::<u32>()).unwrap();
    drop(runtime);
    assert_eq!(deferred.state(), State::Cancelled);
    assert_eq!(
        futures_executor::block_on(deferred.join()),
        Err(Error::Cancelled)
    );
}

#[test]
fn start_outside_runtime_panics() {
    let result = std::panic::catch_unwind(|| Deferred::start(async { 42 }));
    assert!(result.is_err());
}

#[test]
fn begin_outside_runtime_panics_and_stays_not_started() {
    let mut deferred = Deferred::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = deferred.begin(async { 42 });
    }));
    assert!(result.is_err());
    assert_eq!(deferred.state(), State::NotStarted);
}
