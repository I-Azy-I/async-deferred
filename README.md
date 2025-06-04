# async-deferred

A lightweight utility for fire-and-forget async computations in Rust. Start asynchronous tasks immediately and retrieve their results later without blocking.

## Features

- **Fire-and-forget pattern**: Start computations without waiting for results
- **Deferred retrieval**: Check results when convenient using non-blocking operations
- **Panic handling**: Detect and handle task panics gracefully
- **Callback support**: Execute cleanup or notification code after task completion


## Basic Usage

```rust
use async_deferred::Deferred;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() {
    // Start a computation immediately
    let mut deferred = Deferred::start(|| async {
        println!("Starting async work...");
        sleep(Duration::from_secs(2)).await;
        42 // Return result
    });

    // Do other work while computation runs
    println!("Doing other work...");
    sleep(Duration::from_millis(500)).await;

    // Get the result when ready
    let result = deferred.join().await.try_get();
    println!("Result: {:?}", result); // Result: Some(42)
}
```

## Advanced Examples

### Non-blocking Result Checking

```rust
use async_deferred::{Deferred, State};

#[tokio::main]
async fn main() {
    let deferred = Deferred::start(|| async {
        sleep(Duration::from_secs(1)).await;
        "Hello, World!"
    });

    // Check if ready without blocking
    match deferred.try_get() {
        Some(result) => println!("Result ready: {}", result),
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
        || async {
            // Your computation
            expensive_calculation().await
        },
        || {
            println!("Computation finished! Cleaning up...");
        }
    );

    let result = deferred.join().await.try_get();
}
```

### Manual Task Management

```rust
use async_deferred::Deferred;

#[tokio::main]
async fn main() {
    let mut deferred = Deferred::new();
    
    // Start when ready
    deferred.begin(|| async {
        // Your async work here
        process_data().await
    });

    // Check state
    match deferred.state() {
        State::Pending => println!("Task running..."),
        State::Completed => println!("Task done!"),
        State::TaskPanicked(msg) => println!("Task failed: {}", msg),
        _ => {}
    }
}
```

### Error Handling

```rust
use async_deferred::{Deferred, State};

#[tokio::main]
async fn main() {
    let mut deferred = Deferred::start(|| async {
        panic!("Something went wrong!");
    });

    // Wait for completion
    deferred.join().await;

    match deferred.state() {
        State::TaskPanicked(msg) => {
            println!("Task panicked: {}", msg);
        }
        State::Completed => {
            if let Some(result) = deferred.try_get() {
                println!("Result: {:?}", result);
            }
        }
        _ => {}
    }
}
```
