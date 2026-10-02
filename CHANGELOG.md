# Changelog

## 0.4.0

### Breaking changes

- `Spawner::spawn` and `LocalSpawner::spawn_local` return `Result<(), SpawnError>`, so a
  runtime can refuse a task, for example when its task pool is full. Custom spawners return
  `Ok(())` after handing the task to their runtime.
- `start_on`, `start_with_callback_on`, `start_local_on` and
  `start_with_callback_local_on` return `Result<Deferred<T>, SpawnError>`.
- `begin`, `begin_on`, `begin_local_on` and their callback variants return
  `Result<(), BeginError>` instead of `bool`: `BeginError::AlreadyStarted` replaces `false`,
  and `BeginError::Spawn` reports a refused task.
- `begin` can now start a new task after the previous one panicked or was dropped by the
  runtime. Before, the `Deferred` stayed stuck in `TaskPanicked` or `Cancelled`.
  `AlreadyStarted` now only means a task is running or its result has not been taken.
- The `Tokio` spawner returns a `SpawnError` outside a Tokio runtime instead of panicking,
  so `begin` and `begin_with_callback` return an error there. `start` and
  `start_with_callback` still return `Deferred<T>` directly, and panic outside a runtime.
- `embassy_spawner!` spawners return a `SpawnError` when all `pool_size` tasks are running,
  instead of panicking.
- `State`, `Error` and `BeginError` are `#[non_exhaustive]`: a `match` on them needs a
  wildcard arm, so variants can be added later without a breaking change.

### Added

- `cancel_and_wait` cancels the task and waits until its future has been dropped. On a
  single-threaded executor such as embassy, its task pool slot is free when it returns.
- `Spawner` is implemented for `&S`, `&mut S`, `Box<S>` and `Arc<S>`, and `LocalSpawner` also
  for `Rc<S>`, where `S` is a spawner.
- `rust-version = "1.65"`, checked in CI.

### Fixed

- Restarting right after `cancel` in a full embassy task pool panicked, because the
  cancelled task keeps its slot until the executor runs it again. Use `cancel_and_wait`
  before restarting, or handle the `SpawnError` that `begin` now returns.
- The docs now say that callbacks don't run when the task is cancelled or dropped by the
  runtime, that cancelling is cooperative, and that caught panics are still reported by
  the panic hook.

## 0.3.0

### Breaking changes

- `start`, `begin` and their callback variants take a future instead of a closure:
  `Deferred::start(async { 42 })` instead of `Deferred::start(|| async { 42 })`.
- Callbacks receive the result: `FnOnce(&T)` instead of `FnOnce()`.
- `join` returns `Result<&T, Error>` instead of `&Self`.
- `try_get`, `state` and the `is_*` methods take `&mut self`.
- `State` no longer carries panic messages and is now `Copy`. Use `panic_message()` to get the message.
- `State::NotInitialized` is renamed to `State::NotStarted`, and `is_not_initialized` to `is_not_started`.
- `state_async` is removed. Use `state`.
- `take` resets the `Deferred`, so a new task can be started.
- Panic messages no longer have a `"Panic message: "` prefix.
- `start`, `begin`, `start_with_callback` and `begin_with_callback` require the
  `tokio` feature (enabled by default).

### Added

- The result type only needs `Send + 'static`; `Sync` is no longer required.
- Works with any async runtime through the `Spawner` trait, with `start_on`,
  `start_with_callback_on`, `begin_on` and `begin_with_callback_on`.
- `Tokio` spawner, and `Spawner` for `tokio::runtime::Handle` to start tasks from
  outside a runtime (`tokio` feature, default).
- `Smol` spawner, and `Spawner` for `smol::Executor` (`smol` feature).
- `no_std` support: without the default `std` feature, the crate only needs `alloc`.
  Panics are then not caught.
- `LocalSpawner` trait and `start_local_on`, `start_with_callback_local_on`,
  `begin_local_on` and `begin_with_callback_local_on`, for futures that are not `Send`
  and single-threaded executors such as embassy.
- `embassy_spawner!` declares a `LocalSpawner` for embassy in one line.
- `State::Cancelled` and `Error::Cancelled`, reported when the runtime drops the task
  before it finishes, for example on shutdown. This used to be reported as a panic.
- `panic_message()` returns the panic message of the task or its callback.

### Fixed

- A panicking callback no longer hides the result, and `state()` reports it without needing `join`.
- The callback docs no longer claim it runs when the task panics.
- Dropping a `join()` future before it finishes no longer loses track of the task.
- Tokio is optional, and only its `rt` feature is used.
