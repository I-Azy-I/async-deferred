use std::sync::Arc;

use tokio::sync::OnceCell;
use tokio::task::JoinHandle;
use std::future::Future;

/// Represents the current state of an asynchronous task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// The task hasn't been started yet.
    NotInitialized,
    /// The task is currently running.
    Pending,
    /// The task has completed successfully and returned a value.
    Completed,
    /// The task panicked during execution.
    TaskPanicked(String),
    /// The callback panicked during execution.
    CallbackPanicked(String),

}

/// A handle to an asynchronous computation that allows for deferred result retrieval.
/// 
/// This struct supports the "fire-and-forget" pattern with the ability to later query the result
/// or detect if the task panicked. It's useful when you want to start a computation but don't
/// need the result immediately.
///
/// # Examples
///
/// ```rust
/// use async_deferred::Deferred;
/// 
/// # tokio_test::block_on(async {
/// // Start a computation immediately
/// let mut deferred = Deferred::start(|| async {
///     tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
///     42
/// });
///
/// // Later, get the result
/// let result = deferred.join().await.try_get();
/// assert_eq!(result, Some(&42));
/// # })
/// ```
///
/// ```
/// // Create and start manually
///  use async_deferred::Deferred;
/// 
/// # tokio_test::block_on(async {
/// let mut deferred = Deferred::new();
/// deferred.begin(|| async { "Hello, World!" });
///
/// // Check if ready without blocking (might not be ready yet)
/// match deferred.try_get() {
///     Some(result) => println!("Result: {}", result),
///     None => println!("Not ready yet"),
/// }
/// # })
/// ```
///
/// # Thread Safety
///
/// `Deferred<T>` is `Send` and `Sync` when `T` is `Send` and `Sync`, making it safe to
/// share across threads and async tasks.
///
/// # Memory Management
///
/// Internally, the task runs on Tokio's async runtime, and the result is stored in a `OnceCell`.
/// The computation runs independently and the result is cached until retrieved via `take()` or
/// the `Deferred` is dropped.
#[derive(Debug)]
pub struct Deferred<T> {
    value: Arc<OnceCell<T>>,
    task_handle: Option<JoinHandle<()>>,
    panic_message: Option<String>,
    is_callback_panic: bool,
}

impl<T> Deferred<T>
where
    T: Send + Sync + 'static,
{
    /// Creates a new empty `Deferred` instance.
    ///
    /// The task must be started manually using [`begin`](Self::begin) or [`begin_with_callback`](Self::begin_with_callback).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use async_deferred::Deferred;
    /// 
    /// # tokio_test::block_on(async {
    /// let mut deferred = Deferred::new();
    /// assert!(deferred.is_not_initialized());
    /// 
    /// deferred.begin(|| async { 42 });
    /// assert!(!deferred.is_not_initialized());
    /// # })
    /// ```
    pub fn new() -> Self {
        Self {
            value: Arc::new(OnceCell::new()),
            task_handle: None,
            panic_message: None,
            is_callback_panic: false
        }
    }

    /// Starts a new asynchronous task and returns a `Deferred` handle.
    ///
    /// This is a convenient way to construct and start the deferred task in one step.
    ///
    /// # Arguments
    /// * `computation` - An async function or closure that returns the result of the task.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_deferred::Deferred;
    /// 
    /// # tokio_test::block_on(async {
    /// let deferred = Deferred::start(|| async {
    ///     // Some computation
    ///     42
    /// });
    /// 
    /// // Task is already running
    /// assert!(deferred.is_pending() || deferred.is_ready());
    /// # })
    /// ```
    pub fn start<F, Fut>(computation: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let mut deferred = Self::new();
        deferred.begin(computation);
        deferred
    }

    /// Starts a new asynchronous task with a callback that is called once the task completes.
    ///
    /// The callback is executed after the computation finishes, regardless of whether it
    /// succeeded or panicked. This is useful for logging, cleanup, or notifications.
    ///
    /// # Arguments
    /// * `computation` - An async function or closure that returns the result of the task.
    /// * `callback` - A closure that will be run after the task completes.
    ///
    /// # Examples
    ///
    /// ```rust 
    /// use async_deferred::Deferred;
    /// # tokio_test::block_on(async {
    /// let deferred = Deferred::start_with_callback(
    ///     || async { 42 },
    ///     || println!("Computation finished!")
    /// );
    /// # })
    /// ```
    pub fn start_with_callback<F, C, Fut>(computation: F, callback: C) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        C: FnOnce() + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let mut deferred = Self::new();
        deferred.begin_with_callback(computation, callback);
        deferred
    }
    
    /// Begins execution of the deferred task.
    ///
    /// Returns `true` if the task was successfully started, or `false` if it has already 
    /// been started or completed.
    ///
    /// # Arguments
    /// * `computation` - An async function or closure that returns the result of the task.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use async_deferred::Deferred;
    /// # tokio_test::block_on(async {
    /// let mut deferred = Deferred::new();
    /// 
    /// assert!(deferred.begin(|| async { 42 })); // Started successfully
    /// assert!(!deferred.begin(|| async { 24 })); // Already started, returns false
    /// 
    /// # })
    /// ```
    pub fn begin<F, Fut>(&mut self, computation: F) -> bool
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        self._begin(computation, None::<fn()>)
    }

    /// Begins execution of the deferred task and executes a callback after completion.
    ///
    /// Returns `true` if the task was successfully started, or `false` if it has already
    /// been started or completed.
    ///
    /// # Arguments
    /// * `computation` - An async function or closure that returns the result of the task.
    /// * `callback` - A closure that will be run after the task completes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use async_deferred::Deferred;
    /// 
    /// # tokio_test::block_on(async {
    /// let mut deferred = Deferred::new();
    /// 
    /// let started = deferred.begin_with_callback(
    ///     || async { 42 },
    ///     || println!("Task completed!")
    /// );
    /// assert!(started);
    /// # })
    /// ```
    pub fn begin_with_callback<F, C, Fut>(&mut self, computation: F, callback: C) -> bool
    where
        F: FnOnce() -> Fut + Send + 'static,
        C: FnOnce() + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        self._begin(computation, Some(callback))
    }

    /// Internal implementation for starting tasks with optional callbacks.
    fn _begin<F, C, Fut>(&mut self, computation: F, callback: Option<C>) -> bool
    where
        F: FnOnce() -> Fut + Send + 'static,
        C: FnOnce() + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        if !self.is_not_initialized() {
            return false;
        }

        let cell = self.value.clone();

        let handle = tokio::spawn(async move {
            let result = computation().await;
            let _ = cell.set(result);
            if let Some(callback) = callback {
                callback();
            }
        });

        self.task_handle = Some(handle);
        true
    }

    /// Returns the current state of the task.
    ///
    /// # Returns
    /// - `State::NotInitialized` - The task hasn't been started yet
    /// - `State::Pending` - The task is currently running
    /// - `State::Completed` - The task completed successfully or the callback panicked
    /// - `State::TaskPanicked` - The task panicked during execution
    /// 
    /// [`state`](Self::state) only detects if the task has panicked and not the callback.
    /// If you need this information, use [`join`](Self::join) or [`state_async`](Self::state_async).
    /// 
    /// # Examples
    ///
    /// ```rust
    /// use async_deferred::{Deferred, State};
    /// 
    /// # tokio_test::block_on(async {
    /// let mut deferred: Deferred<u32> = Deferred::new();
    /// assert!(matches!(deferred.state(), State::NotInitialized));
    /// deferred.begin(|| async { 
    ///     tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    ///     42
    /// });
    /// assert!(matches!(deferred.state(), State::Pending));
    /// tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    /// assert!(matches!(deferred.state(), State::Completed));
    /// # })
    /// ```
    /// If panicked due to the callback, [`state`](Self::state) will return State::Completed.
    /// ```rust
    /// use async_deferred::{Deferred, State};
    /// 
    /// # tokio_test::block_on(async {
    /// let mut deferred: Deferred<u32> = Deferred::start_with_callback(
    ///         || async {42},
    ///         || {panic!("the callback panicked");},
    /// );
    /// tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    /// 
    /// // state only detects if the task has panicked and not the callback
    /// assert!(matches!(deferred.state(), State::Completed));
    /// # })
    /// ```
   pub fn state(&self) -> State {
        // If we already detected a panic, return the appropriate state
        if let Some(panic_msg) = &self.panic_message {
            return if self.is_callback_panic {
                State::CallbackPanicked(panic_msg.clone())
            } else {
                State::TaskPanicked(panic_msg.clone())
            };
        }
        
        // Check if we have a task handle
        if let Some(handle) = &self.task_handle {
            if handle.is_finished() {
                // Task is finished, but we can't check for panic synchronously
                // We'll need to await the result in an async context
                // For now, if the task is finished but we don't have a result, assume it panicked
                if self.is_ready() {
                    State::Completed
                } else {
                    // Task finished but no result - likely panicked
                    // We'll set a default message until we can properly check
                    State::TaskPanicked("Task may have panicked (use join() to get details)".to_string())
                }
            } else if self.is_ready() {
                // Task is still running but result is available (shouldn't happen)
                State::Completed
            } else {
                // Task is still running
                State::Pending
            }
        } else if self.is_ready() {
            // handle may be none if the handle has been consumed by the join
            State::Completed
        } else {
            State::NotInitialized
        }
    }

   
    /// Returns the current state of the task. This method is able to correctly 
    /// diagnose when a given callback panics, as opposed to [`state`](Self::state).
    /// Though asynchronous, it will not block until the task completes.
    ///
    /// # Returns
    /// - `State::NotInitialized` - The task hasn't been started yet
    /// - `State::Pending` - The task is currently running
    /// - `State::Completed` - The task completed successfully
    /// - `State::TaskPanicked` - The task panicked during execution
    /// - `State::CallbackPanicked` - The callback panicked during execution
    /// 
    /// # Examples
    /// ```rust
    /// use async_deferred::{Deferred, State};
    /// 
    /// # tokio_test::block_on(async {
    /// let mut deferred: Deferred<u32> = Deferred::new();
    /// assert!(matches!(deferred.state_async().await, State::NotInitialized));
    /// deferred.begin(|| async { 
    ///     tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    ///     42
    /// });
    /// 
    /// // The task is not done yet
    /// assert!(matches!(deferred.state_async().await, State::Pending));
    /// 
    /// tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    /// 
    /// // The task is completed
    /// assert!(matches!(deferred.state_async().await, State::Completed));
    /// # })
    /// ```
    /// 
    /// ```rust
    /// use async_deferred::{Deferred, State};
    /// 
    /// # tokio_test::block_on(async {
    /// let mut deferred: Deferred<u32> = Deferred::start_with_callback(
    ///         || async { panic!("the task panicked"); 42 },
    ///         || { panic!("callback will not be called since the task panicked"); },
    /// );
    /// tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    /// 
    /// assert!(matches!(deferred.state_async().await, State::TaskPanicked(_)));
    /// # })
    /// ```
    /// 
    /// ```rust
    /// use async_deferred::{Deferred, State};
    /// 
    /// # tokio_test::block_on(async {
    /// let mut deferred: Deferred<u32> = Deferred::start_with_callback(
    ///         || async { 42 },
    ///         || { panic!("the callback panicked"); },
    /// );
    /// tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    /// 
    /// assert!(matches!(deferred.state_async().await, State::CallbackPanicked(_)));
    /// # })
    pub async fn state_async(&mut self) -> State {
        // If we already detected a panic, return the appropriate state
        if let Some(panic_msg) = &self.panic_message {
            return if self.is_callback_panic {
                State::CallbackPanicked(panic_msg.clone())
            } else {
                State::TaskPanicked(panic_msg.clone())
            };
        }
        
        // Check if we have a task handle
        if let Some(handle) = &self.task_handle {
            if handle.is_finished() {
                // We can safely await the finished task
                self.join().await;
                
                // Now check the state again
                if let Some(panic_msg) = &self.panic_message {
                    if self.is_callback_panic {
                        State::CallbackPanicked(panic_msg.clone())
                    } else {
                        State::TaskPanicked(panic_msg.clone())
                    }
                } else if self.is_ready() {
                    State::Completed
                } else {
                    // This shouldn't happen
                    State::Pending
                }
            } else if self.is_ready() {
                State::Completed
            } else {
                State::Pending
            }
        } else {
            State::NotInitialized
        }
    }
    /// Attempts to retrieve the result of the computation if available.
    ///
    /// Returns `Some(&T)` if the task has completed successfully, or `None` if the task
    /// is still pending, panicked, or not started.
    ///
    /// This is a non-blocking operation that returns immediately.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use async_deferred::Deferred;
    /// # tokio_test::block_on(async {
    /// let deferred = Deferred::start(|| async { 42 });
    /// 
    /// // Might return None if task hasn't finished yet
    /// if let Some(result) = deferred.try_get() {
    ///     println!("Result: {}", result);
    /// }
    /// # })
    /// ```
    pub fn try_get(&self) -> Option<&T> {
        self.value.get()
    }

    /// Waits for the task to complete and returns a reference to self.
    ///
    /// If the task is still pending, this will await its completion (either success or panic).
    /// If the task has already completed or panicked, this returns immediately.
    ///
    /// This method allows for method chaining: `deferred.join().await.try_get()`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use async_deferred::Deferred;
    /// 
    /// # tokio_test::block_on(async {
    /// let mut deferred = Deferred::start(|| async {
    ///     tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    ///     42
    /// });
    ///
    /// // Wait for completion and get result
    /// let result = deferred.join().await.try_get();
    /// assert_eq!(result, Some(&42));
    /// # })
    /// ```
    pub async fn join(&mut self) -> &Self {
        if let Some(handle) = std::mem::take(&mut self.task_handle) {
            let result = handle.await;
            match result {
                Ok(_) => {
                    // Task completed successfully
                },
                Err(join_err) => {
                    // Check if it's a callback panic and extract message
                    let (panic_msg, is_callback_panic) = if join_err.is_panic() {
                        if let Ok(panic_payload) = join_err.try_into_panic() {
                            let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                                format!("Panic message: {}", s)
                            } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                                format!("Panic message: {}", s)
                            } else {
                                "Panic occurred, but couldn't get the message.".to_string()
                            };
                            
                            (panic_msg, self.is_ready())
                        } else {
                            ("Panic occurred, but couldn't extract panic info.".to_string(), false)
                        }
                    } else if join_err.is_cancelled() {
                        ("Task was cancelled.".to_string(), false)
                    } else {
                        ("Unknown join error.".to_string(), false)
                    };
                    
                    self.panic_message = Some(panic_msg);
                    self.is_callback_panic = is_callback_panic;
                }
            }
        }
        self
    }

    /// Cancels the current task if it's pending.
    /// 
    /// If the task is not pending (completed, panicked, or not initialized), 
    /// this method does nothing and returns `false`. Returns `true` if the 
    /// task was successfully cancelled.
    ///
    /// After cancellation, the `Deferred` returns to the `NotInitialized` state
    /// and can be reused with a new task via `begin()` or `begin_with_callback()`.
    /// 
    /// # Returns
    /// - `true` if a pending task was cancelled
    /// - `false` if there was no pending task to cancel
    /// 
    /// # Examples
    /// 
    /// ```rust
    /// use async_deferred::{Deferred, State};
    /// 
    /// # tokio_test::block_on(async {
    /// let mut deferred = Deferred::start(|| async { 
    ///     tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    ///     42 
    /// });
    /// 
    /// let was_cancelled = deferred.cancel();
    /// assert!(was_cancelled);
    /// assert_eq!(deferred.state(), State::NotInitialized);
    /// 
    /// // Can reuse the deferred after cancellation
    /// deferred.begin(|| async { 100 });
    /// assert!(deferred.is_pending());
    /// # })
    /// ```
    /// 
    /// ```rust
    /// use async_deferred::{Deferred, State};
    /// 
    /// # tokio_test::block_on(async {
    /// // Task completes quickly
    /// let mut deferred = Deferred::start(|| async { 42 });
    /// tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    /// 
    /// // Try to cancel after completion - does nothing
    /// let was_cancelled = deferred.cancel();
    /// assert!(!was_cancelled);
    /// assert_eq!(deferred.state(), State::Completed);
    /// assert_eq!(deferred.take(), Some(42));
    /// # })
    /// ```
    /// 
    /// ```rust
    /// use async_deferred::{Deferred, State};
    /// 
    /// # tokio_test::block_on(async {
    /// // Not initialized yet
    /// let mut deferred: Deferred<i32> = Deferred::new();
    /// 
    /// // Try to cancel uninitialized task - does nothing
    /// let was_cancelled = deferred.cancel();
    /// assert!(!was_cancelled);
    /// assert_eq!(deferred.state(), State::NotInitialized);
    /// # })
    /// ```
    pub fn cancel(&mut self) -> bool{
        if self.is_pending() {
        if let Some(handle) = &self.task_handle {
            handle.abort();
        }
        // Reset to uninitialized state
        self.task_handle = None;
        self.value = Arc::new(OnceCell::new());
        self.panic_message = None;
        self.is_callback_panic = false;
        true
    } else {
        false
    }
    }

    /// Attempts to take ownership of the computed value.
    ///
    /// Returns `Some(T)` if the task has completed successfully, consuming the stored result.
    /// Returns `None` if the task is still pending, panicked, or not started.
    ///
    /// After calling this method successfully, subsequent calls will return `None` since
    /// the value has been moved out.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use async_deferred::Deferred;
    /// 
    /// # tokio_test::block_on(async {
    /// let mut deferred = Deferred::start(|| async { vec![1, 2, 3] });
    /// 
    /// // Wait for completion
    /// deferred.join().await;
    ///
    /// // Take ownership of the result
    /// let result = deferred.take();
    /// assert_eq!(result, Some(vec![1, 2, 3]));
    ///
    /// // Subsequent calls return None
    /// assert_eq!(deferred.take(), None);
    /// # })
    /// ```
    /// 
    /// [`take`](Self::take) will not consume if it is still pending
    /// ```rust
    /// use async_deferred::Deferred;
    /// 
    /// # tokio_test::block_on(async {
    /// let mut deferred = Deferred::start(|| async { 
    ///     tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    ///     42
    /// });
    /// // If still pending, doesn't consume it
    /// assert_eq!(deferred.take(), None);
    /// tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    /// assert!(matches!(deferred.try_get(), Some(42)));
    /// # })
    /// ```
    /// 
    pub fn take(&mut self) -> Option<T> {
        if self.is_ready() {
            let value = std::mem::take(&mut self.value);
            self.task_handle = None;
            Arc::try_unwrap(value).ok()?.into_inner()
        } else {
            None
        }
    }


    /// Returns `true` if the task panicked during execution.
    ///
    /// A task is considered panicked if it was started but the `JoinHandle` indicates
    /// it finished without storing a result in the `OnceCell`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use async_deferred::Deferred;
    /// 
    /// # tokio_test::block_on(async {
    /// let mut deferred = Deferred::start(|| async {
    ///     panic!("Something went wrong!");
    /// });
    /// 
    /// tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    /// 
    /// assert!(deferred.has_task_panicked());
    /// # })
    /// ```
    pub fn has_task_panicked(&self) -> bool {
       matches!(self.state(), State::TaskPanicked(_))
    }


    /// Returns `true` if the data is available.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use async_deferred::Deferred;
    /// 
    /// # tokio_test::block_on(async {
    /// let mut deferred = Deferred::start(|| async { 
    ///     tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    ///     42 
    /// });
    /// 
    /// // Initially not ready
    /// assert!(!deferred.is_ready());
    ///
    /// deferred.join().await;
    /// assert!(deferred.is_ready());
    /// # })
    /// ```
    pub fn is_ready(&self) -> bool {
        self.value.get().is_some()
    }

    /// Returns `true` if the task has completed successfully.
    ///
    /// This means the computation finished without panicking and a result is available
    /// via [`try_get`](Self::try_get) or [`take`](Self::take).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use async_deferred::Deferred;
    /// 
    /// # tokio_test::block_on(async {
    /// let mut deferred = Deferred::start(|| async { 
    ///     tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    ///     42 
    /// });
    /// 
    /// // Initially not ready
    /// assert!(!deferred.is_complete());
    ///
    /// deferred.join().await;
    /// println!("{:?}", deferred.state());
    /// assert!(deferred.is_complete());
    /// # })
    pub fn is_complete(&self) -> bool {
        matches!(self.state(), State::Completed)
    }

    /// Returns `true` if the task is currently running.
    ///
    /// A task is pending if it has been started but hasn't completed or panicked yet.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use async_deferred::Deferred;
    /// # tokio_test::block_on(async {
    /// let deferred = Deferred::start(|| async {
    ///     tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    ///     42
    /// });
    ///
    /// assert!(deferred.is_pending()); // Task is running
    /// # })
    /// ```
    pub fn is_pending(&self) -> bool {
        matches!(self.state(), State::Pending)
    }

    /// Returns `true` if the task hasn't been initialized yet.
    ///
    /// This is the initial state of a `Deferred` created with [`new`](Self::new).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use async_deferred::Deferred;
    /// # tokio_test::block_on(async {
    /// let deferred: Deferred<i32> = Deferred::new();
    /// assert!(deferred.is_not_initialized());
    ///
    /// let started_deferred = Deferred::start(|| async { 42 });
    /// assert!(!started_deferred.is_not_initialized());
    /// # })
    /// ```
    pub fn is_not_initialized(&self) -> bool {
        matches!(self.state(), State::NotInitialized)
    }
}

impl<T> Default for Deferred<T>
where
    T: Send + Sync + 'static,
{
    /// Creates a new empty `Deferred` instance.
    ///
    /// Equivalent to [`Deferred::new()`](Self::new).
    fn default() -> Self {
        Self::new()
    }
}