use core::future::Future;

/// Runs a task in the background on an async runtime.
///
/// Implement this to use [`Deferred`](crate::Deferred) with any runtime. Implementations
/// for Tokio and smol are available behind the `tokio` (default) and `smol` features.
///
/// The runtime must keep polling `task` until it finishes. If the runtime stops first,
/// it must drop `task`, so the `Deferred` reports [`State::Cancelled`](crate::State::Cancelled)
/// instead of staying pending.
///
/// # Examples
///
/// ```rust
/// use std::future::Future;
/// use async_deferred::{Deferred, Spawner};
///
/// struct ThreadSpawner;
///
/// impl Spawner for ThreadSpawner {
///     fn spawn<F>(&self, task: F)
///     where
///         F: Future<Output = ()> + Send + 'static,
///     {
///         std::thread::spawn(move || futures_executor::block_on(task));
///     }
/// }
///
/// let mut deferred = Deferred::start_on(&ThreadSpawner, async { 42 });
/// assert_eq!(futures_executor::block_on(deferred.join()), Ok(&42));
/// ```
pub trait Spawner {
    /// Runs `task` in the background.
    fn spawn<F>(&self, task: F)
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
/// Embassy only spawns tasks declared with `#[embassy_executor::task]`, so declare one
/// task that runs a boxed future and spawn through it. This needs a heap allocator,
/// such as `esp-alloc` or `embedded-alloc`. The pool size limits how many tasks can run
/// at once.
///
/// ```rust,ignore
/// use core::{future::Future, pin::Pin};
/// use alloc::boxed::Box;
/// use async_deferred::{Deferred, LocalSpawner};
///
/// #[embassy_executor::task(pool_size = 4)]
/// async fn run(task: Pin<Box<dyn Future<Output = ()>>>) {
///     task.await
/// }
///
/// struct Embassy(embassy_executor::Spawner);
///
/// impl LocalSpawner for Embassy {
///     fn spawn_local<F>(&self, task: F)
///     where
///         F: Future<Output = ()> + 'static,
///     {
///         self.0.spawn(run(Box::pin(task)).expect("task pool is full"));
///     }
/// }
/// ```
pub trait LocalSpawner {
    /// Runs `task` in the background.
    fn spawn_local<F>(&self, task: F)
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
    fn spawn<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(task);
    }
}

/// Spawns on this Tokio runtime. Works from outside the runtime.
#[cfg(feature = "tokio")]
impl Spawner for tokio::runtime::Handle {
    fn spawn<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        tokio::runtime::Handle::spawn(self, task);
    }
}

/// Spawns on smol's global executor.
#[cfg(feature = "smol")]
#[derive(Debug, Clone, Copy, Default)]
pub struct Smol;

#[cfg(feature = "smol")]
impl Spawner for Smol {
    fn spawn<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        smol::spawn(task).detach();
    }
}

/// Spawns on this smol executor. The task only makes progress while the executor is run.
#[cfg(feature = "smol")]
impl Spawner for smol::Executor<'static> {
    fn spawn<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        smol::Executor::spawn(self, task).detach();
    }
}
