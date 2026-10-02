//! [`StaticDeferred`]: a task's result kept in a `static`, without a heap.

use core::cell::RefCell;
use core::future::{poll_fn, Future};
use core::mem::{self, ManuallyDrop};
use core::pin::pin;
use core::task::{Poll, Waker};

use critical_section::Mutex;

use crate::{BeginError, Error, State};

/// A task's result, kept in a `static` instead of on the heap.
///
/// Use it on targets without a heap allocator, or to keep memory use fixed. Each kind of
/// job gets its own `static StaticDeferred` and its own task, which runs the job through a
/// [`Ticket`]. All methods take `&self`, so the `static` can be used from anywhere.
///
/// Needs a [`critical-section`](https://docs.rs/critical-section) implementation, which
/// embassy and the HALs provide. With `std`, add
/// `critical-section = { version = "1", features = ["std"] }` to your own dependencies.
///
/// Panics in the job are not caught. On targets where a panic unwinds instead of halting,
/// such as `std`, the `StaticDeferred` then reports [`State::Cancelled`].
///
/// Wait on it ([`join`](Self::join) or [`cancel_and_wait`](Self::cancel_and_wait)) from one
/// task at a time. Several waiting tasks still all finish, but they keep waking each other
/// until then, which costs CPU time, and a `join` that ends without a result may report
/// [`Error::Cancelled`] where [`Error::NotStarted`] would be accurate. A `join` that returns
/// `Ok` always has the result of the run it waited for.
///
/// # Examples
///
/// ```rust,ignore
/// use async_deferred::{StaticDeferred, Ticket};
///
/// static SENSOR: StaticDeferred<u16> = StaticDeferred::new();
///
/// #[embassy_executor::task]
/// async fn sensor_task(ticket: Ticket<'static, u16>) {
///     ticket.run(read_sensor()).await;
/// }
///
/// #[embassy_executor::main]
/// async fn main(spawner: embassy_executor::Spawner) {
///     spawner.spawn(sensor_task(SENSOR.begin().unwrap()).unwrap());
///
///     // ... do other work ...
///
///     if let Some(value) = SENSOR.take() {
///         // use the reading
///     }
/// }
/// ```
pub struct StaticDeferred<T> {
    shared: Mutex<RefCell<Shared<T>>>,
}

struct Shared<T> {
    phase: Phase<T>,
    /// Increases with every `begin`, so a task from an earlier run can't affect this one.
    run: u32,
    /// Tickets and running tasks that still exist, from this run or earlier ones.
    live: u32,
    /// The task of the current run, woken when it is cancelled.
    task: Option<Waker>,
    /// Whoever waits in `join` or `cancel_and_wait`.
    waiter: Option<Waker>,
    /// The run a `join` waits for, and whether its result was taken by someone else.
    /// Only that run's outcome is kept, so later runs can't change what the `join` reports.
    awaited: Option<Awaited>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Awaited {
    run: u32,
    taken: bool,
}

enum Phase<T> {
    NotStarted,
    Pending,
    Done(T),
    Cancelled,
}

impl<T> Shared<T> {
    fn is_current(&self, run: u32) -> bool {
        self.run == run && matches!(self.phase, Phase::Pending)
    }
}

/// Stores `waker` in `slot`. A different waker already there is returned so it can be woken
/// and register again: several waiters keep making progress, by polling in turn.
fn register(slot: &mut Option<Waker>, waker: &Waker) -> Option<Waker> {
    match slot {
        Some(current) if current.will_wake(waker) => None,
        _ => slot.replace(waker.clone()),
    }
}

fn wake(waker: Option<Waker>) {
    if let Some(waker) = waker {
        waker.wake();
    }
}

impl<T> StaticDeferred<T> {
    /// Creates a `StaticDeferred` with no task, for use in a `static`.
    pub const fn new() -> Self {
        Self {
            shared: Mutex::new(RefCell::new(Shared {
                phase: Phase::NotStarted,
                run: 0,
                live: 0,
                task: None,
                waiter: None,
                awaited: None,
            })),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Shared<T>) -> R) -> R {
        critical_section::with(|cs| f(&mut self.shared.borrow_ref_mut(cs)))
    }

    /// Starts a new run and returns its [`Ticket`]. Pass the ticket to the task that runs
    /// the job, and call [`Ticket::run`] there.
    ///
    /// Returns [`BeginError::AlreadyStarted`] if a run is in progress or its result has
    /// not been taken. If the ticket is dropped without running, for example because the
    /// task could not be spawned, the `StaticDeferred` goes back to
    /// [`State::NotStarted`].
    pub fn begin(&self) -> Result<Ticket<'_, T>, BeginError> {
        self.with(|shared| {
            if matches!(shared.phase, Phase::Pending | Phase::Done(_)) {
                return Err(BeginError::AlreadyStarted);
            }
            shared.run = shared.run.wrapping_add(1);
            shared.phase = Phase::Pending;
            shared.live += 1;
            Ok(Ticket {
                deferred: self,
                run: shared.run,
            })
        })
    }

    /// Returns the current [`State`] without waiting.
    ///
    /// Never reports a panic: panics in the job are not caught.
    pub fn state(&self) -> State {
        self.with(|shared| match shared.phase {
            Phase::NotStarted => State::NotStarted,
            Phase::Pending => State::Pending,
            Phase::Done(_) => State::Completed,
            Phase::Cancelled => State::Cancelled,
        })
    }

    /// Returns `true` if no run has been started, or its result was taken.
    pub fn is_not_started(&self) -> bool {
        self.state() == State::NotStarted
    }

    /// Returns `true` if a run is in progress.
    pub fn is_pending(&self) -> bool {
        self.state() == State::Pending
    }

    /// Returns `true` if the result is available.
    pub fn is_ready(&self) -> bool {
        self.state() == State::Completed
    }

    /// Returns `true` if the task was dropped before it finished.
    pub fn is_cancelled(&self) -> bool {
        self.state() == State::Cancelled
    }

    /// Moves the result out if the run has finished, without waiting.
    ///
    /// On success, a new run can be started.
    pub fn take(&self) -> Option<T> {
        self.with(
            |shared| match mem::replace(&mut shared.phase, Phase::NotStarted) {
                Phase::Done(value) => {
                    if let Some(awaited) = &mut shared.awaited {
                        if awaited.run == shared.run {
                            awaited.taken = true;
                        }
                    }
                    Some(value)
                }
                other => {
                    shared.phase = other;
                    None
                }
            },
        )
    }

    /// Calls `f` with the result if the run has finished, without moving it out.
    ///
    /// `f` runs inside a critical section, so keep it short, and don't use this
    /// `StaticDeferred` from `f`: that panics because it is already borrowed.
    pub fn with_result<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        self.with(|shared| match &shared.phase {
            Phase::Done(value) => Some(f(value)),
            _ => None,
        })
    }

    /// Waits for the run to finish and moves its result out.
    ///
    /// Returns [`Error::NotStarted`] if no run is in progress and no result is waiting, or if
    /// another task took the result first. Returns [`Error::Cancelled`] if the run was
    /// cancelled or its task was dropped before it finished. A `join` only waits for the run
    /// that was in progress when it started, never for a later one. A [`Ticket`] dropped
    /// without running counts as a dropped task.
    ///
    /// With several tasks waiting at once, the error kind may be `Cancelled` where
    /// `NotStarted` would be accurate; see the type's docs.
    pub async fn join(&self) -> Result<T, Error> {
        // The run this `join` waits for, once it has seen one in progress.
        let mut waiting_for = None;
        poll_fn(|cx| {
            let (poll, to_wake) = self.with(|shared| match shared.phase {
                // The awaited run ended and a new run number was given out since.
                _ if waiting_for.map_or(false, |run| run != shared.run) => {
                    let taken = shared.awaited
                        == Some(Awaited {
                            run: waiting_for.unwrap_or_default(),
                            taken: true,
                        });
                    let error = if taken {
                        Error::NotStarted
                    } else {
                        Error::Cancelled
                    };
                    (Poll::Ready(Err(error)), None)
                }
                Phase::Pending => {
                    waiting_for = Some(shared.run);
                    if shared.awaited.map(|a| a.run) != Some(shared.run) {
                        shared.awaited = Some(Awaited {
                            run: shared.run,
                            taken: false,
                        });
                    }
                    (Poll::Pending, register(&mut shared.waiter, cx.waker()))
                }
                Phase::NotStarted => (Poll::Ready(Err(Error::NotStarted)), None),
                Phase::Cancelled => (Poll::Ready(Err(Error::Cancelled)), None),
                Phase::Done(_) => match mem::replace(&mut shared.phase, Phase::NotStarted) {
                    Phase::Done(value) => (Poll::Ready(Ok(value)), None),
                    _ => unreachable!("matched above"),
                },
            });
            wake(to_wake);
            poll
        })
        .await
    }

    /// Stops the run in progress and resets to [`State::NotStarted`], so a new run can begin.
    ///
    /// The task stops the next time its executor runs it, so it may still hold its executor
    /// resources, such as an embassy task pool slot, when this returns. Use
    /// [`cancel_and_wait`](Self::cancel_and_wait) to wait until it has stopped.
    /// Returns `false` and does nothing if no run is in progress.
    pub fn cancel(&self) -> bool {
        let (cancelled, task, waiter) = self.with(|shared| {
            if matches!(shared.phase, Phase::Pending) {
                shared.phase = Phase::NotStarted;
                // A new run number tells the task and any waiting `join` that this run ended.
                shared.run = shared.run.wrapping_add(1);
                (true, shared.task.take(), shared.waiter.take())
            } else {
                (false, None, None)
            }
        });
        wake(task);
        wake(waiter);
        cancelled
    }

    /// Like [`cancel`](Self::cancel), and waits until no task of this `StaticDeferred` is
    /// still running, including tasks cancelled earlier.
    ///
    /// On a single-threaded executor such as embassy, their task pool slots are free when
    /// this returns. It waits for every ticket from [`begin`](Self::begin) to be run or
    /// dropped, so don't start a new run from elsewhere while waiting. A ticket that is
    /// neither run nor dropped, for example passed to [`mem::forget`], makes this wait
    /// forever.
    /// Returns `true` if a run was in progress.
    pub async fn cancel_and_wait(&self) -> bool {
        let cancelled = self.cancel();
        poll_fn(|cx| {
            let (poll, to_wake) = self.with(|shared| {
                if shared.live == 0 {
                    (Poll::Ready(()), None)
                } else {
                    (Poll::Pending, register(&mut shared.waiter, cx.waker()))
                }
            });
            wake(to_wake);
            poll
        })
        .await;
        cancelled
    }
}

impl<T> Default for StaticDeferred<T> {
    /// Same as [`StaticDeferred::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl<T> core::fmt::Debug for StaticDeferred<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StaticDeferred")
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

/// Permission to run one job for a [`StaticDeferred`], returned by
/// [`StaticDeferred::begin`].
///
/// Pass it to the task that does the work and call [`run`](Self::run) there. Dropping it
/// without running resets the `StaticDeferred` to [`State::NotStarted`].
#[must_use = "the job only runs once the ticket's `run` is awaited"]
pub struct Ticket<'a, T> {
    deferred: &'a StaticDeferred<T>,
    run: u32,
}

impl<T> core::fmt::Debug for Ticket<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ticket").field("run", &self.run).finish()
    }
}

impl<'a, T> Ticket<'a, T> {
    /// Runs `future` and stores its output in the [`StaticDeferred`].
    ///
    /// Returns early, without storing anything, if the run is cancelled. If this future is
    /// dropped before it finishes, the `StaticDeferred` reports [`State::Cancelled`].
    pub async fn run<F>(self, future: F)
    where
        F: Future<Output = T>,
    {
        // From here on, `Running` reports what happens to this run.
        let ticket = ManuallyDrop::new(self);
        let running = Running {
            deferred: ticket.deferred,
            run: ticket.run,
        };

        let mut future = pin!(future);
        let output = poll_fn(|cx| {
            let (current, to_wake) = running.deferred.with(|shared| {
                if shared.is_current(running.run) {
                    (true, register(&mut shared.task, cx.waker()))
                } else {
                    (false, None)
                }
            });
            wake(to_wake);
            if !current {
                return Poll::Ready(None);
            }
            future.as_mut().poll(cx).map(Some)
        })
        .await;

        if let Some(value) = output {
            let (leftover, waiter, task) = running.deferred.with(move |shared| {
                if shared.is_current(running.run) {
                    shared.phase = Phase::Done(value);
                    (None, shared.waiter.take(), shared.task.take())
                } else {
                    // Cancelled while finishing: discard the value.
                    (Some(value), None, None)
                }
            });
            // Dropped outside the critical section: their `Drop` may run any code.
            drop((leftover, task));
            wake(waiter);
        }
    }
}

impl<T> Drop for Ticket<'_, T> {
    fn drop(&mut self) {
        // Never ran: for example the task could not be spawned.
        let waiter = self.deferred.with(|shared| {
            shared.live -= 1;
            if shared.is_current(self.run) {
                shared.phase = Phase::NotStarted;
                // Like `cancel`: a waiting `join` sees that this run ended without a result.
                shared.run = shared.run.wrapping_add(1);
            }
            shared.waiter.take()
        });
        wake(waiter);
    }
}

/// Owned by a running job; reports a task dropped before finishing.
struct Running<'a, T> {
    deferred: &'a StaticDeferred<T>,
    run: u32,
}

impl<T> Drop for Running<'_, T> {
    fn drop(&mut self) {
        let (waiter, task) = self.deferred.with(|shared| {
            shared.live -= 1;
            let task = if shared.is_current(self.run) {
                shared.phase = Phase::Cancelled;
                shared.task.take()
            } else {
                None
            };
            (shared.waiter.take(), task)
        });
        // Dropped outside the critical section: its `Drop` may run any code.
        drop(task);
        wake(waiter);
    }
}
