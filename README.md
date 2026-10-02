# async-deferred

[![CI](https://github.com/I-Azy-I/async-deferred/actions/workflows/ci.yml/badge.svg)](https://github.com/I-Azy-I/async-deferred/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/async-deferred.svg)](https://crates.io/crates/async-deferred)
[![docs.rs](https://docs.rs/async-deferred/badge.svg)](https://docs.rs/async-deferred)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](#license)
[![no_std](https://img.shields.io/badge/no__std-alloc-green.svg)](https://docs.rust-embedded.org/book/intro/no-std.html)

A lightweight utility for fire-and-forget async computations in Rust. Start asynchronous tasks immediately and retrieve their results later without blocking. Works with any async runtime.

## Features

- **Fire-and-forget pattern**: start computations without waiting for results
- **Deferred retrieval**: check results when convenient using non-blocking operations
- **Panic handling**: detect if the task or its callback panicked, and why
- **Callback support**: run code with the result as soon as the task finishes
- **Cancellation**: stop a running task and reuse the `Deferred`
- **Any runtime**: Tokio by default, smol behind a feature, or your own through the `Spawner` trait
- **`no_std`**: works on embedded executors such as embassy, with a heap allocator

The result type only needs to be `Send + 'static`, the same as `tokio::spawn`.

## Examples

### Non-blocking Result Checking

```rust
use async_deferred::Deferred;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() {
    let mut deferred = Deferred::start(async {
        sleep(Duration::from_millis(100)).await;
        "Hello, World!"
    });

    // Check if ready without blocking
    match deferred.try_get() {
        Some(result) => println!("Result ready: {result}"),
        None => println!("Still computing..."),
    }
}
```

### With Completion Callbacks

```rust
use async_deferred::Deferred;

#[tokio::main]
async fn main() {
    let mut deferred = Deferred::start_with_callback(
        async { 6 * 7 },
        |result| println!("Computation finished with {result}"),
    );

    deferred.join().await.unwrap();
}
```

### Await for a result

```rust
use async_deferred::Deferred;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() {
    // Start a computation immediately
    let mut deferred = Deferred::start(async {
        sleep(Duration::from_millis(100)).await;
        42
    });

    // Do other work while computation runs
    println!("Doing other work...");

    // Get the result when ready
    let result = deferred.join().await;
    assert_eq!(result, Ok(&42));
}
```

### Error Handling

```rust
use async_deferred::{Deferred, Error};

#[tokio::main]
async fn main() {
    let mut deferred: Deferred<u32> = Deferred::start(async {
        panic!("Something went wrong!");
    });

    match deferred.join().await {
        Ok(result) => println!("Result: {result}"),
        Err(Error::Panicked(msg)) => println!("Task panicked: {msg}"),
        Err(err) => println!("No result: {err}"),
    }
}
```

### Cancellation

```rust
use async_deferred::{Deferred, State};

#[tokio::main]
async fn main() {
    let mut deferred = Deferred::start(std::future::pending::<u32>());

    assert!(deferred.cancel());
    assert_eq!(deferred.state(), State::NotStarted);

    // The Deferred can be reused
    deferred.begin(async { 100 }).unwrap();
    assert_eq!(deferred.join().await, Ok(&100));
}
```

## Runtimes

Tasks are started through a spawner, which hands them to an async runtime.

| Feature | Spawner | Notes |
|---|---|---|
| `tokio` (default) | `Tokio`, `tokio::runtime::Handle` | Enables the `start` and `begin` shortcuts used above |
| `smol` | `Smol`, `smol::Executor<'static>` | |
| `std` (default) | | Catches panics in the task and callback. Without it, the crate is `no_std` + `alloc` |

To use another runtime without pulling in Tokio:

```toml
async-deferred = { version = "0.4", default-features = false, features = ["smol"] }
```

### smol

```rust,ignore
use async_deferred::{Deferred, Smol};

fn main() {
    let mut deferred = Deferred::start_on(&Smol, async { 42 }).unwrap();
    assert_eq!(smol::block_on(deferred.join()), Ok(&42));
}
```

### Your own runtime

Any runtime works by implementing `Spawner`:

```rust
use std::future::Future;
use async_deferred::{Deferred, SpawnError, Spawner};

struct MyRuntime;

impl Spawner for MyRuntime {
    fn spawn<F>(&self, task: F) -> Result<(), SpawnError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // Hand `task` to your runtime here, or return a `SpawnError` if it can't take it.
        std::thread::spawn(move || futures_executor::block_on(task));
        Ok(())
    }
}

let mut deferred = Deferred::start_on(&MyRuntime, async { 42 }).unwrap();
```

For single-threaded executors and futures that are not `Send`, implement `LocalSpawner`
and use the `*_local_on` methods.

### Embassy (`no_std`)

```toml
async-deferred = { version = "0.4", default-features = false }
```

`embassy_spawner!` declares a spawner for embassy in one line. You also need a heap allocator,
such as `esp-alloc` or `embedded-alloc`, and a target with atomic compare-and-swap.

```rust,ignore
use async_deferred::{embassy_spawner, Deferred};
use embassy_time::{Duration, Instant, Timer};

// Lets up to 4 `Deferred` tasks run at the same time.
embassy_spawner!(DeferredSpawner, pool_size = 4);

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    // ... set up the heap allocator and the time driver ...
    let spawner = DeferredSpawner(spawner);

    let mut measurement = Deferred::start_local_on(&spawner, read_sensor()).unwrap();
    let mut started = Instant::now();

    loop {
        // The main loop's own work.
        Timer::after(Duration::from_millis(100)).await;

        // Check for the result without waiting.
        if let Some(value) = measurement.take() {
            defmt::info!("temperature: {}", value);
        } else if started.elapsed() > Duration::from_millis(500) {
            // The sensor hung: give up, and wait until its task has freed its slot.
            measurement.cancel_and_wait().await;
        } else {
            continue;
        }

        // `take` and `cancel` reset the `Deferred`, so it can start the next measurement.
        if measurement.begin_local_on(&spawner, read_sensor()).is_err() {
            defmt::error!("could not start a measurement");
        }
        started = Instant::now();
    }
}
```

The futures can hold values that are not `Send`, such as `Rc` or peripheral drivers.
Starting a task while `pool_size` tasks are running returns a `SpawnError` instead of
panicking. A cancelled task frees its slot the next time the executor runs it: use
`cancel_and_wait` to restart right away in a full pool.
Without the `std` feature, panics are not caught, so `TaskPanicked` and `CallbackPanicked`
are never reported. On embedded targets a panic usually halts the device anyway.

A complete example for the ESP32-S3, with tests that run on the chip, is in
[`embassy-esp32s3/`](https://github.com/I-Azy-I/async-deferred/tree/main/embassy-esp32s3).

## License

Licensed under the [MIT license](https://github.com/I-Azy-I/async-deferred/blob/main/LICENSE).
