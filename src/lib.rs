#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod spawner;

use alloc::string::String;
use core::fmt;
use core::future::Future;

use futures_channel::oneshot;
use futures_util::future::{AbortHandle, Abortable};

#[cfg(feature = "smol")]
pub use spawner::Smol;
#[cfg(feature = "tokio")]
pub use spawner::Tokio;
pub use spawner::{LocalSpawner, Spawner};

/// What the task sends back: the value and the callback's panic message, or the task's
/// panic message.
type Outcome<T> = Result<(T, Option<String>), String>;

/// The state of a [`Deferred`] task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No task has been started, or its result was taken.
    NotStarted,
    /// The task is running.
    Pending,
    /// The task finished and its result is available.
    Completed,
    /// The task panicked. See [`Deferred::panic_message`].
    ///
    /// Only reported with the `std` feature; without it, a panic is not caught.
    TaskPanicked,
    /// The task finished but its callback panicked. The result is still available.
    /// See [`Deferred::panic_message`].
    ///
    /// Only reported with the `std` feature; without it, a panic is not caught.
    CallbackPanicked,
    /// The runtime dropped the task before it finished, for example on shutdown.
    Cancelled,
}

/// Why [`Deferred::join`] has no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No task has been started, or its result was taken.
    NotStarted,
    /// The task panicked. Contains the panic message.
    Panicked(String),
    /// The runtime dropped the task before it finished, for example on shutdown.
    Cancelled,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotStarted => f.write_str("no task has been started"),
            Error::Panicked(msg) => write!(f, "task panicked: {msg}"),
            Error::Cancelled => f.write_str("task was cancelled"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

#[derive(Debug)]
enum Inner<T> {
    NotStarted,
    Running {
        receiver: oneshot::Receiver<Outcome<T>>,
        abort: AbortHandle,
    },
    Done {
        value: T,
        callback_panic: Option<String>,
    },
    Panicked(String),
    Cancelled,
}

/// A background task whose result you can check or collect later.
///
/// Tasks run on any async runtime through a [`Spawner`]. With the default `tokio`
/// feature, [`start`](Self::start) and [`begin`](Self::begin) spawn on the current
/// Tokio runtime.
///
/// The result is kept until you [`take`](Self::take) it or drop the `Deferred`.
/// Dropping a `Deferred` does not stop its task; use [`cancel`](Self::cancel) for that.
///
/// # Examples
///
/// ```rust
/// use async_deferred::Deferred;
///
/// # tokio_test::block_on(async {
/// let mut deferred = Deferred::start(async { 42 });
///
/// // ... do other work ...
///
/// assert_eq!(deferred.join().await, Ok(&42));
/// # })
/// ```
#[derive(Debug)]
pub struct Deferred<T> {
    inner: Inner<T>,
}

impl<T> Deferred<T> {
    /// Creates a `Deferred` with no task. Start one with [`begin_on`](Self::begin_on).
    pub fn new() -> Self {
        Self {
            inner: Inner::NotStarted,
        }
    }

    /// Spawns `future` with `spawner` and returns its `Deferred`.
    ///
    /// See [`Spawner`] for using your own runtime.
    ///
    /// ```rust
    /// use async_deferred::Deferred;
    ///
    /// // Start a task from outside a Tokio runtime
    /// let runtime = tokio::runtime::Runtime::new().unwrap();
    /// let mut deferred = Deferred::start_on(runtime.handle(), async { 42 });
    /// assert_eq!(runtime.block_on(deferred.join()), Ok(&42));
    /// ```
    pub fn start_on<S, F>(spawner: &S, future: F) -> Self
    where
        S: Spawner,
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let mut deferred = Self::new();
        deferred.begin_on(spawner, future);
        deferred
    }

    /// Like [`start_on`](Self::start_on), and runs `callback` with the result once
    /// `future` finishes.
    ///
    /// The callback is not run if `future` panics.
    pub fn start_with_callback_on<S, F, C>(spawner: &S, future: F, callback: C) -> Self
    where
        S: Spawner,
        F: Future<Output = T> + Send + 'static,
        C: FnOnce(&T) + Send + 'static,
        T: Send + 'static,
    {
        let mut deferred = Self::new();
        deferred.begin_with_callback_on(spawner, future, callback);
        deferred
    }

    /// Spawns `future` with `spawner`.
    ///
    /// Returns `false` and does nothing if a task was already started and its result
    /// has not been taken.
    pub fn begin_on<S, F>(&mut self, spawner: &S, future: F) -> bool
    where
        S: Spawner,
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        self.begin_with_callback_on(spawner, future, |_: &T| {})
    }

    /// Like [`begin_on`](Self::begin_on), and runs `callback` with the result once
    /// `future` finishes.
    ///
    /// The callback is not run if `future` panics.
    pub fn begin_with_callback_on<S, F, C>(&mut self, spawner: &S, future: F, callback: C) -> bool
    where
        S: Spawner,
        F: Future<Output = T> + Send + 'static,
        C: FnOnce(&T) + Send + 'static,
        T: Send + 'static,
    {
        if !self.is_not_started() {
            return false;
        }
        let (task, running) = task(future, callback);
        spawner.spawn(task);
        self.inner = running;
        true
    }

    /// Like [`start_on`](Self::start_on), for futures that are not `Send`.
    pub fn start_local_on<S, F>(spawner: &S, future: F) -> Self
    where
        S: LocalSpawner,
        F: Future<Output = T> + 'static,
        T: 'static,
    {
        let mut deferred = Self::new();
        deferred.begin_local_on(spawner, future);
        deferred
    }

    /// Like [`start_with_callback_on`](Self::start_with_callback_on), for futures that
    /// are not `Send`.
    pub fn start_with_callback_local_on<S, F, C>(spawner: &S, future: F, callback: C) -> Self
    where
        S: LocalSpawner,
        F: Future<Output = T> + 'static,
        C: FnOnce(&T) + 'static,
        T: 'static,
    {
        let mut deferred = Self::new();
        deferred.begin_with_callback_local_on(spawner, future, callback);
        deferred
    }

    /// Like [`begin_on`](Self::begin_on), for futures that are not `Send`.
    pub fn begin_local_on<S, F>(&mut self, spawner: &S, future: F) -> bool
    where
        S: LocalSpawner,
        F: Future<Output = T> + 'static,
        T: 'static,
    {
        self.begin_with_callback_local_on(spawner, future, |_: &T| {})
    }

    /// Like [`begin_with_callback_on`](Self::begin_with_callback_on), for futures that
    /// are not `Send`.
    pub fn begin_with_callback_local_on<S, F, C>(
        &mut self,
        spawner: &S,
        future: F,
        callback: C,
    ) -> bool
    where
        S: LocalSpawner,
        F: Future<Output = T> + 'static,
        C: FnOnce(&T) + 'static,
        T: 'static,
    {
        if !self.is_not_started() {
            return false;
        }
        let (task, running) = task(future, callback);
        spawner.spawn_local(task);
        self.inner = running;
        true
    }

    /// If the task has finished, stores its outcome. Never waits.
    fn poll_task(&mut self) {
        let Inner::Running { receiver, .. } = &mut self.inner else {
            return;
        };
        match receiver.try_recv() {
            Ok(None) => {}
            Ok(Some(outcome)) => self.inner = Inner::finished(Some(outcome)),
            Err(oneshot::Canceled) => self.inner = Inner::finished(None),
        }
    }

    /// Waits for the task to finish and returns its result.
    ///
    /// Returns immediately if the task already finished. If the task finished but its
    /// callback panicked, the result is still returned; check [`state`](Self::state)
    /// to detect that.
    ///
    /// ```rust
    /// use async_deferred::{Deferred, Error};
    ///
    /// # tokio_test::block_on(async {
    /// let mut deferred: Deferred<u32> = Deferred::start(async { panic!("boom") });
    /// assert_eq!(deferred.join().await, Err(Error::Panicked("boom".into())));
    /// # })
    /// ```
    pub async fn join(&mut self) -> Result<&T, Error> {
        if let Inner::Running { receiver, .. } = &mut self.inner {
            let outcome = receiver.await.ok();
            self.inner = Inner::finished(outcome);
        }
        match &self.inner {
            Inner::Done { value, .. } => Ok(value),
            Inner::Panicked(msg) => Err(Error::Panicked(msg.clone())),
            Inner::Cancelled => Err(Error::Cancelled),
            Inner::NotStarted => Err(Error::NotStarted),
            Inner::Running { .. } => unreachable!("the task was awaited above"),
        }
    }

    /// Returns the current [`State`] without waiting.
    pub fn state(&mut self) -> State {
        self.poll_task();
        match &self.inner {
            Inner::NotStarted => State::NotStarted,
            Inner::Running { .. } => State::Pending,
            Inner::Done {
                callback_panic: None,
                ..
            } => State::Completed,
            Inner::Done {
                callback_panic: Some(_),
                ..
            } => State::CallbackPanicked,
            Inner::Panicked(_) => State::TaskPanicked,
            Inner::Cancelled => State::Cancelled,
        }
    }

    /// Returns the panic message if the task or its callback panicked.
    pub fn panic_message(&mut self) -> Option<&str> {
        self.poll_task();
        match &self.inner {
            Inner::Panicked(msg)
            | Inner::Done {
                callback_panic: Some(msg),
                ..
            } => Some(msg),
            _ => None,
        }
    }

    /// Returns the result if the task has finished, without waiting.
    pub fn try_get(&mut self) -> Option<&T> {
        self.poll_task();
        match &self.inner {
            Inner::Done { value, .. } => Some(value),
            _ => None,
        }
    }

    /// Moves the result out if the task has finished, without waiting.
    ///
    /// On success, the `Deferred` is reset and can start a new task.
    ///
    /// ```rust
    /// use async_deferred::{Deferred, State};
    ///
    /// # tokio_test::block_on(async {
    /// let mut deferred = Deferred::start(async { vec![1, 2, 3] });
    /// deferred.join().await.unwrap();
    /// assert_eq!(deferred.take(), Some(vec![1, 2, 3]));
    /// assert_eq!(deferred.state(), State::NotStarted);
    /// # })
    /// ```
    pub fn take(&mut self) -> Option<T> {
        self.poll_task();
        if !matches!(self.inner, Inner::Done { .. }) {
            return None;
        }
        match core::mem::replace(&mut self.inner, Inner::NotStarted) {
            Inner::Done { value, .. } => Some(value),
            _ => unreachable!("checked above"),
        }
    }

    /// Stops a running task and resets the `Deferred` so it can start a new one.
    ///
    /// The task stops the next time the runtime polls it.
    /// Returns `false` and does nothing if no task is running.
    ///
    /// ```rust
    /// use async_deferred::{Deferred, State};
    ///
    /// # tokio_test::block_on(async {
    /// let mut deferred = Deferred::start(std::future::pending::<u32>());
    /// assert!(deferred.cancel());
    /// assert_eq!(deferred.state(), State::NotStarted);
    /// # })
    /// ```
    pub fn cancel(&mut self) -> bool {
        self.poll_task();
        let Inner::Running { abort, .. } = &self.inner else {
            return false;
        };
        abort.abort();
        self.inner = Inner::NotStarted;
        true
    }

    /// Returns `true` if the result is available, even if the callback panicked.
    pub fn is_ready(&mut self) -> bool {
        self.try_get().is_some()
    }

    /// Returns `true` if the task and its callback finished without panicking.
    pub fn is_complete(&mut self) -> bool {
        self.state() == State::Completed
    }

    /// Returns `true` if the task is still running.
    pub fn is_pending(&mut self) -> bool {
        self.state() == State::Pending
    }

    /// Returns `true` if no task has been started, or its result was taken.
    pub fn is_not_started(&mut self) -> bool {
        self.state() == State::NotStarted
    }

    /// Returns `true` if the task panicked.
    pub fn has_task_panicked(&mut self) -> bool {
        self.state() == State::TaskPanicked
    }
}

#[cfg(feature = "tokio")]
impl<T> Deferred<T> {
    /// Spawns `future` on the current Tokio runtime and returns its `Deferred`.
    ///
    /// # Panics
    ///
    /// Panics if called outside a Tokio runtime. Use [`start_on`](Self::start_on) with a
    /// [`tokio::runtime::Handle`] there.
    pub fn start<F>(future: F) -> Self
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        Self::start_on(&Tokio, future)
    }

    /// Like [`start`](Self::start), and runs `callback` with the result once `future` finishes.
    ///
    /// The callback is not run if `future` panics.
    ///
    /// ```rust
    /// use async_deferred::Deferred;
    ///
    /// # tokio_test::block_on(async {
    /// let deferred = Deferred::start_with_callback(
    ///     async { 42 },
    ///     |result| println!("computed {result}"),
    /// );
    /// # })
    /// ```
    pub fn start_with_callback<F, C>(future: F, callback: C) -> Self
    where
        F: Future<Output = T> + Send + 'static,
        C: FnOnce(&T) + Send + 'static,
        T: Send + 'static,
    {
        Self::start_with_callback_on(&Tokio, future, callback)
    }

    /// Spawns `future` on the current Tokio runtime.
    ///
    /// Returns `false` and does nothing if a task was already started and its result
    /// has not been taken.
    ///
    /// # Panics
    ///
    /// Panics if called outside a Tokio runtime.
    ///
    /// ```rust
    /// use async_deferred::Deferred;
    ///
    /// # tokio_test::block_on(async {
    /// let mut deferred = Deferred::new();
    /// assert!(deferred.begin(async { 42 }));
    /// assert!(!deferred.begin(async { 24 }));
    /// # })
    /// ```
    pub fn begin<F>(&mut self, future: F) -> bool
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        self.begin_on(&Tokio, future)
    }

    /// Like [`begin`](Self::begin), and runs `callback` with the result once `future` finishes.
    ///
    /// The callback is not run if `future` panics.
    pub fn begin_with_callback<F, C>(&mut self, future: F, callback: C) -> bool
    where
        F: Future<Output = T> + Send + 'static,
        C: FnOnce(&T) + Send + 'static,
        T: Send + 'static,
    {
        self.begin_with_callback_on(&Tokio, future, callback)
    }
}

impl<T> Default for Deferred<T> {
    /// Same as [`Deferred::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Inner<T> {
    /// `None` means the task was dropped before sending its outcome.
    fn finished(outcome: Option<Outcome<T>>) -> Self {
        match outcome {
            Some(Ok((value, callback_panic))) => Inner::Done {
                value,
                callback_panic,
            },
            Some(Err(msg)) => Inner::Panicked(msg),
            None => Inner::Cancelled,
        }
    }
}

/// Builds the task to spawn, and the `Running` state that tracks it.
///
/// The task is `Send` whenever `F`, `C` and `T` are.
fn task<T, F, C>(future: F, callback: C) -> (impl Future<Output = ()>, Inner<T>)
where
    F: Future<Output = T> + 'static,
    C: FnOnce(&T) + 'static,
    T: 'static,
{
    let (sender, receiver) = oneshot::channel();
    let (abort, abort_registration) = AbortHandle::new_pair();
    let task = Abortable::new(
        async move {
            let outcome = match catch_unwind(future).await {
                Ok(value) => {
                    // Catch a callback panic so the computed value is not lost with it.
                    let callback_panic = call_catching_panic(callback, &value).err();
                    Ok((value, callback_panic))
                }
                Err(msg) => Err(msg),
            };
            // The `Deferred` may have been dropped; the result is then discarded.
            let _ = sender.send(outcome);
        },
        abort_registration,
    );
    let task = async move {
        let _ = task.await;
    };
    (task, Inner::Running { receiver, abort })
}

/// Awaits `future`, turning a panic into its message.
#[cfg(feature = "std")]
async fn catch_unwind<F: Future>(future: F) -> Result<F::Output, String> {
    use futures_util::FutureExt;
    std::panic::AssertUnwindSafe(future)
        .catch_unwind()
        .await
        .map_err(|payload| panic_message(&*payload))
}

/// Awaits `future`. Without `std`, panics cannot be caught.
#[cfg(not(feature = "std"))]
async fn catch_unwind<F: Future>(future: F) -> Result<F::Output, String> {
    Ok(future.await)
}

/// Calls `callback`, turning a panic into its message.
#[cfg(feature = "std")]
fn call_catching_panic<T, C: FnOnce(&T)>(callback: C, value: &T) -> Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(value)))
        .map_err(|payload| panic_message(&*payload))
}

/// Calls `callback`. Without `std`, panics cannot be caught.
#[cfg(not(feature = "std"))]
fn call_catching_panic<T, C: FnOnce(&T)>(callback: C, value: &T) -> Result<(), String> {
    callback(value);
    Ok(())
}

/// Extracts a readable message from a panic payload.
#[cfg(feature = "std")]
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}
