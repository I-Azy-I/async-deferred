# async-deferred

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
    deferred.begin(async { 100 });
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
async-deferred = { version = "0.3", default-features = false, features = ["smol"] }
```

### smol

```rust,ignore
use async_deferred::{Deferred, Smol};

fn main() {
    let mut deferred = Deferred::start_on(&Smol, async { 42 });
    assert_eq!(smol::block_on(deferred.join()), Ok(&42));
}
```

### Your own runtime

Any runtime works by implementing `Spawner`:

```rust
use std::future::Future;
use async_deferred::{Deferred, Spawner};

struct MyRuntime;

impl Spawner for MyRuntime {
    fn spawn<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // Hand `task` to your runtime here.
        std::thread::spawn(move || futures_executor::block_on(task));
    }
}

let mut deferred = Deferred::start_on(&MyRuntime, async { 42 });
```

For single-threaded executors and futures that are not `Send`, implement `LocalSpawner`
and use the `*_local_on` methods.

### Embassy (`no_std`)

```toml
async-deferred = { version = "0.3", default-features = false }
```

Embassy only spawns tasks declared with `#[embassy_executor::task]`, so declare one task that
runs a boxed future and implement `LocalSpawner` with it. This needs a heap allocator such as
`esp-alloc` or `embedded-alloc`, and a target with atomic compare-and-swap.

```rust,ignore
#[embassy_executor::task(pool_size = 4)]
async fn run(task: Pin<Box<dyn Future<Output = ()>>>) {
    task.await
}

struct Embassy(embassy_executor::Spawner);

impl LocalSpawner for Embassy {
    fn spawn_local<F: Future<Output = ()> + 'static>(&self, task: F) {
        self.0.spawn(run(Box::pin(task)).expect("task pool is full"));
    }
}

let mut reading = Deferred::start_local_on(&Embassy(spawner), read_sensor());
```

Without the `std` feature, panics are not caught, so `TaskPanicked` and `CallbackPanicked`
are never reported. On embedded targets a panic usually halts the device anyway.
