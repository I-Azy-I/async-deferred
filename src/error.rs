//! Types shared by `Deferred` and `StaticDeferred`.

#[cfg(feature = "alloc")]
use alloc::string::String;
use core::fmt;

/// The state of a [`Deferred`](crate::Deferred) or [`StaticDeferred`](crate::StaticDeferred) task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum State {
    /// No task has been started, or its result was taken.
    NotStarted,
    /// The task is running.
    Pending,
    /// The task finished and its result is available.
    Completed,
    /// The task panicked. See [`Deferred::panic_message`](crate::Deferred::panic_message).
    ///
    /// Only reported with the `std` feature; without it, a panic is not caught.
    TaskPanicked,
    /// The task finished but its callback panicked. The result is still available.
    /// See [`Deferred::panic_message`](crate::Deferred::panic_message).
    ///
    /// Only reported with the `std` feature; without it, a panic is not caught.
    CallbackPanicked,
    /// The runtime dropped the task before it finished, for example on shutdown.
    Cancelled,
}

/// Why [`Deferred::join`](crate::Deferred::join) or
/// [`StaticDeferred::join`](crate::StaticDeferred::join) has no result.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// No task has been started, or its result was taken.
    NotStarted,
    /// The task panicked. Contains the panic message.
    #[cfg(feature = "alloc")]
    Panicked(String),
    /// The runtime dropped the task before it finished, for example on shutdown. For
    /// [`StaticDeferred::join`](crate::StaticDeferred::join), also returned when the run
    /// it waited for was cancelled.
    Cancelled,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotStarted => f.write_str("no task has been started"),
            #[cfg(feature = "alloc")]
            Error::Panicked(msg) => write!(f, "task panicked: {msg}"),
            Error::Cancelled => f.write_str("task was cancelled"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

/// Why a runtime could not start a task. Returned by [`Spawner`](crate::Spawner) and
/// [`LocalSpawner`](crate::LocalSpawner).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnError {
    reason: &'static str,
}

impl SpawnError {
    /// Creates an error with a short `reason`, such as `"task pool is full"`.
    pub const fn new(reason: &'static str) -> Self {
        Self { reason }
    }

    /// The reason given by the runtime.
    pub const fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for SpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "could not spawn the task: {}", self.reason)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for SpawnError {}

/// Why a `begin` method did not start a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BeginError {
    /// A task is running, or its result has not been taken yet.
    AlreadyStarted,
    /// The runtime could not start the task.
    Spawn(SpawnError),
}

impl From<SpawnError> for BeginError {
    fn from(err: SpawnError) -> Self {
        BeginError::Spawn(err)
    }
}

impl fmt::Display for BeginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BeginError::AlreadyStarted => {
                f.write_str("a task is running or its result has not been taken")
            }
            BeginError::Spawn(err) => err.fmt(f),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for BeginError {}

#[cfg(feature = "alloc")]
impl BeginError {
    /// The spawn error from a `begin` on a new `Deferred`, which can't be `AlreadyStarted`.
    pub(crate) fn into_spawn_error(self) -> SpawnError {
        match self {
            BeginError::Spawn(err) => err,
            BeginError::AlreadyStarted => unreachable!("a new `Deferred` has no task"),
        }
    }
}
