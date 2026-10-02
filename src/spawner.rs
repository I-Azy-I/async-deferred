use core::future::Future;

use crate::SpawnError;

/// Runs a task in the background on an async runtime.
///
/// Implement this to use [`Deferred`](crate::Deferred) with any runtime. Implementations
/// for Tokio and smol are available behind the `tokio` (default) and `smol` features.
///
/// The runtime must keep polling `task` until it finishes. If the runtime stops first,
/// it must drop `task`, so the `Deferred` reports [`State::Cancelled`](crate::State::Cancelled)
/// instead of staying pending. If the runtime can't take the task, for example because its
/// task pool is full, return a [`SpawnError`].
///
/// # Examples
///
/// ```rust
/// use std::future::Future;
/// use async_deferred::{Deferred, SpawnError, Spawner};
///
/// struct ThreadSpawner;
///
/// impl Spawner for ThreadSpawner {
///     fn spawn<F>(&self, task: F) -> Result<(), SpawnError>
///     where
///         F: Future<Output = ()> + Send + 'static,
///     {
///         std::thread::spawn(move || futures_executor::block_on(task));
///         Ok(())
///     }
/// }
///
/// let mut deferred = Deferred::start_on(&ThreadSpawner, async { 42 }).unwrap();
/// assert_eq!(futures_executor::block_on(deferred.join()), Ok(&42));
/// ```
pub trait Spawner {
    /// Runs `task` in the background.
    fn spawn<F>(&self, task: F) -> Result<(), SpawnError>
    where
        F: Future<Output = ()> + Send + 'static;
}

/// Runs a task that is not `Send` in the background, on a single-threaded executor.
///
/// Use it with the `*_local_on` methods of [`Deferred`](crate::Deferred). The same
/// requirements as [`Spawner`] apply.
///
/// # Embassy
///
/// Use [`embassy_spawner!`](crate::embassy_spawner) to declare a `LocalSpawner` for embassy.
pub trait LocalSpawner {
    /// Runs `task` in the background.
    fn spawn_local<F>(&self, task: F) -> Result<(), SpawnError>
    where
        F: Future<Output = ()> + 'static;
}

/// Spawns on the current Tokio runtime.
///
/// Spawning panics outside a Tokio runtime. Use a [`tokio::runtime::Handle`] there.
#[cfg(feature = "tokio")]
#[derive(Debug, Clone, Copy, Default)]
pub struct Tokio;

#[cfg(feature = "tokio")]
impl Spawner for Tokio {
    fn spawn<F>(&self, task: F) -> Result<(), SpawnError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(task);
        Ok(())
    }
}

/// Spawns on this Tokio runtime. Works from outside the runtime.
#[cfg(feature = "tokio")]
impl Spawner for tokio::runtime::Handle {
    fn spawn<F>(&self, task: F) -> Result<(), SpawnError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        tokio::runtime::Handle::spawn(self, task);
        Ok(())
    }
}

/// Spawns on smol's global executor.
#[cfg(feature = "smol")]
#[derive(Debug, Clone, Copy, Default)]
pub struct Smol;

#[cfg(feature = "smol")]
impl Spawner for Smol {
    fn spawn<F>(&self, task: F) -> Result<(), SpawnError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        smol::spawn(task).detach();
        Ok(())
    }
}

/// Spawns on this smol executor. The task only makes progress while the executor is run.
#[cfg(feature = "smol")]
impl Spawner for smol::Executor<'static> {
    fn spawn<F>(&self, task: F) -> Result<(), SpawnError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        smol::Executor::spawn(self, task).detach();
        Ok(())
    }
}
