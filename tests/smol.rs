//! The `smol` feature: the `Smol` spawner and `smol::Executor`.
#![cfg(feature = "smol")]

mod common;

use async_deferred::{Deferred, Error, Smol, State};
use common::{pending_until_dropped, Flag};

#[test]
fn start_on_global_executor() {
    let mut deferred = Deferred::start_on(&Smol, async { 42 }).unwrap();
    assert_eq!(smol::block_on(deferred.join()), Ok(&42));
}

#[test]
fn callback_on_global_executor() {
    let ran = Flag::default();
    let ran_in_callback = ran.clone();
    let mut deferred =
        Deferred::start_with_callback_on(&Smol, async { 42 }, move |_| ran_in_callback.set())
            .unwrap();
    smol::block_on(deferred.join()).unwrap();
    assert!(ran.is_set());
}

#[test]
fn task_panic_on_global_executor() {
    let mut deferred: Deferred<u32> = Deferred::start_on(&Smol, async { panic!("boom") }).unwrap();
    assert_eq!(
        smol::block_on(deferred.join()),
        Err(Error::Panicked("boom".into()))
    );
}

#[test]
fn cancel_on_global_executor() {
    let (future, dropped) = pending_until_dropped::<u32>();
    let mut deferred = Deferred::start_on(&Smol, future).unwrap();
    assert!(deferred.cancel());
    assert!(smol::block_on(dropped).is_err());
}

#[test]
fn start_on_executor() {
    let executor = smol::Executor::new();
    let mut deferred = Deferred::start_on(&executor, async { 42 }).unwrap();
    assert_eq!(deferred.state(), State::Pending);
    assert_eq!(smol::block_on(executor.run(deferred.join())), Ok(&42));
}

#[test]
fn executor_drop_reports_cancelled() {
    let executor = smol::Executor::new();
    let mut deferred = Deferred::start_on(&executor, std::future::pending::<u32>()).unwrap();
    drop(executor);
    assert_eq!(deferred.state(), State::Cancelled);
}
