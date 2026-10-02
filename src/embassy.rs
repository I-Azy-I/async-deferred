/// Declares a [`LocalSpawner`](crate::LocalSpawner) for the embassy executor.
///
/// Embassy only runs tasks declared with `#[embassy_executor::task]`, so this macro declares
/// one with room for `pool_size` tasks at once, and a spawner type that runs every
/// [`Deferred`](crate::Deferred) task through it. Starting more tasks than `pool_size` at
/// the same time panics.
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
///     let mut reading = Deferred::start_local_on(&spawner, read_sensor());
///     // ... do other work ...
///     let value = reading.join().await;
/// }
/// ```
#[macro_export]
macro_rules! embassy_spawner {
    ($vis:vis $name:ident, pool_size = $pool_size:expr) => {
        /// Runs `Deferred` tasks on the embassy executor.
        #[derive(Clone, Copy)]
        $vis struct $name(pub ::embassy_executor::Spawner);

        impl $crate::LocalSpawner for $name {
            fn spawn_local<F>(&self, task: F)
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

                self.0.spawn(run($crate::__private::Box::pin(task)).expect(concat!(
                    stringify!($name),
                    ": too many tasks running at once, increase `pool_size`"
                )));
            }
        }
    };
}
