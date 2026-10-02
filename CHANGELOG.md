# Changelog

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
- `State::Cancelled` and `Error::Cancelled`, reported when the runtime drops the task
  before it finishes, for example on shutdown. This used to be reported as a panic.
- `panic_message()` returns the panic message of the task or its callback.

### Fixed

- A panicking callback no longer hides the result, and `state()` reports it without needing `join`.
- The callback docs no longer claim it runs when the task panics.
- Dropping a `join()` future before it finishes no longer loses track of the task.
- Tokio is optional, and only its `rt` feature is used.
