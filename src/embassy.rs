/// Declares a [`LocalSpawner`](crate::LocalSpawner) for the embassy executor.
///
/// The spawner can run up to `pool_size` [`Deferred`](crate::Deferred) tasks at once.
/// Starting a task while `pool_size` tasks are running returns a
/// [`SpawnError`](crate::SpawnError).
///
/// To stop a task and start a new one right away in a full pool, use
/// [`cancel_and_wait`](crate::Deferred::cancel_and_wait) rather than
/// [`cancel`](crate::Deferred::cancel).
///
/// Needs `embassy-executor` 0.10 and a heap allocator, such as `esp-alloc` or
/// `embedded-alloc`.
///
/// # Examples
///
/// ```rust,ignore
/// use async_deferred::{embassy_spawner, Deferred};
///
/// embassy_spawner!(DeferredSpawner, pool_size = 4);
///
/// #[embassy_executor::main]
/// async fn main(spawner: embassy_executor::Spawner) {
///     let spawner = DeferredSpawner(spawner);
///     let mut reading = Deferred::start_local_on(&spawner, read_sensor()).unwrap();
///     // ... do other work ...
///     let value = reading.join().await;
/// }
/// ```
#[cfg(feature = "alloc")]
#[macro_export]
macro_rules! embassy_spawner {
    ($vis:vis $name:ident, pool_size = $pool_size:expr) => {
        /// Runs `Deferred` tasks on the embassy executor.
        #[derive(Clone, Copy)]
        $vis struct $name(pub ::embassy_executor::Spawner);

        impl $crate::LocalSpawner for $name {
            fn spawn_local<F>(&self, task: F) -> ::core::result::Result<(), $crate::SpawnError>
            where
                F: ::core::future::Future<Output = ()> + 'static,
            {
                #[::embassy_executor::task(pool_size = $pool_size)]
                async fn run(
                    task: ::core::pin::Pin<
                        $crate::__private::Box<dyn ::core::future::Future<Output = ()>>,
                    >,
                ) {
                    task.await
                }

                let token = run($crate::__private::Box::pin(task)).map_err(|_| {
                    $crate::SpawnError::new(concat!(
                        stringify!($name),
                        ": all `pool_size` tasks are running"
                    ))
                })?;
                self.0.spawn(token);
                ::core::result::Result::Ok(())
            }
        }
    };
}

/// Without the `alloc` feature, `embassy_spawner!` only reports what is missing.
#[cfg(not(feature = "alloc"))]
#[doc(hidden)]
#[macro_export]
macro_rules! embassy_spawner {
    ($($input:tt)*) => {
        ::core::compile_error!(
            "`embassy_spawner!` needs the `alloc` feature of async-deferred, because `Deferred` \
             keeps its tasks on the heap. Without a heap, use `StaticDeferred` \
             (the `static-deferred` feature)."
        );
    };
}
