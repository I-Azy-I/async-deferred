#![allow(dead_code)]

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

#[cfg(feature = "alloc")]
use async_deferred::Deferred;
use async_deferred::{LocalSpawner, SpawnError, Spawner};
use futures_channel::oneshot;

/// Runs each task on its own thread.
pub struct ThreadSpawner;

impl Spawner for ThreadSpawner {
    fn spawn<F>(&self, task: F) -> Result<(), SpawnError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        std::thread::spawn(move || futures_executor::block_on(task));
        Ok(())
    }
}

/// Drops every task without running it, like a runtime shutting down.
pub struct DroppingSpawner;

impl Spawner for DroppingSpawner {
    fn spawn<F>(&self, task: F) -> Result<(), SpawnError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        drop(task);
        Ok(())
    }
}

impl LocalSpawner for DroppingSpawner {
    fn spawn_local<F>(&self, task: F) -> Result<(), SpawnError>
    where
        F: Future<Output = ()> + 'static,
    {
        drop(task);
        Ok(())
    }
}

/// Spawns onto a single-threaded `LocalPool`.
pub struct PoolSpawner(pub futures_executor::LocalSpawner);

impl LocalSpawner for PoolSpawner {
    fn spawn_local<F>(&self, task: F) -> Result<(), SpawnError>
    where
        F: Future<Output = ()> + 'static,
    {
        use futures_util::task::LocalSpawnExt;
        self.0.spawn_local(task).unwrap();
        Ok(())
    }
}

/// Busy-waits until the task is no longer pending.
#[cfg(feature = "alloc")]
pub fn wait_until_finished<T>(deferred: &mut Deferred<T>) {
    while deferred.is_pending() {
        std::thread::yield_now();
    }
}

/// A future that never finishes and reports when it is dropped.
pub fn pending_until_dropped<T>() -> (impl Future<Output = T> + Send, oneshot::Receiver<()>) {
    let (dropped_tx, dropped_rx) = oneshot::channel::<()>();
    let future = async move {
        let _guard = dropped_tx; // closes `dropped_rx` when the future is dropped
        std::future::pending::<T>().await
    };
    (future, dropped_rx)
}

/// A flag a callback can set, to check whether it ran.
#[derive(Clone, Default)]
pub struct Flag(Arc<AtomicBool>);

impl Flag {
    pub fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Wraps values so tests can check each one is dropped exactly once.
#[derive(Clone, Default)]
pub struct DropTracker(Arc<std::sync::Mutex<Vec<Arc<AtomicUsize>>>>);

impl DropTracker {
    /// Wraps `value`; its drops are counted from now on.
    pub fn track<T>(&self, value: T) -> Tracked<T> {
        let drops = Arc::new(AtomicUsize::new(0));
        self.0.lock().unwrap().push(drops.clone());
        Tracked { value, drops }
    }

    /// How many values were wrapped.
    pub fn created(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    /// How many wrapped values have been dropped so far.
    pub fn dropped(&self) -> usize {
        let counts = self.0.lock().unwrap();
        counts
            .iter()
            .filter(|d| d.load(Ordering::SeqCst) > 0)
            .count()
    }

    /// Panics unless every wrapped value was dropped exactly once.
    pub fn assert_all_dropped_once(&self) {
        for (i, drops) in self.0.lock().unwrap().iter().enumerate() {
            let drops = drops.load(Ordering::SeqCst);
            assert_eq!(drops, 1, "tracked value #{i} was dropped {drops} times");
        }
    }
}

/// A value whose drops are counted by a [`DropTracker`].
#[derive(Debug)]
pub struct Tracked<T> {
    pub value: T,
    drops: Arc<AtomicUsize>,
}

impl<T> Drop for Tracked<T> {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

/// A single-threaded executor with a fixed number of task slots, like an embassy pool.
///
/// A slot is freed only when the executor polls the task to completion, so a task
/// cancelled with `cancel()` keeps its slot until the executor runs again.
#[derive(Clone)]
pub struct SlotPool {
    slots: std::rc::Rc<std::cell::RefCell<Vec<Slot>>>,
}

type LocalTask = std::pin::Pin<Box<dyn Future<Output = ()>>>;

enum Slot {
    Free,
    /// Holds a task, or `None` while that task is being polled, so a task it spawns meanwhile
    /// can't take its slot.
    Used(Option<LocalTask>),
}

impl SlotPool {
    pub fn new(slots: usize) -> Self {
        Self {
            slots: std::rc::Rc::new(std::cell::RefCell::new(
                (0..slots).map(|_| Slot::Free).collect(),
            )),
        }
    }

    /// How many slots hold a task.
    pub fn used(&self) -> usize {
        self.slots
            .borrow()
            .iter()
            .filter(|slot| matches!(slot, Slot::Used(_)))
            .count()
    }

    /// Polls every task once, freeing the slots of finished tasks.
    pub fn run_once(&self) {
        let mut cx = std::task::Context::from_waker(futures_util::task::noop_waker_ref());
        let count = self.slots.borrow().len();
        for i in 0..count {
            // Take the task out while polling it; the slot stays used.
            let task = match &mut self.slots.borrow_mut()[i] {
                Slot::Used(task) => task.take(),
                Slot::Free => None,
            };
            if let Some(mut task) = task {
                let pending = task.as_mut().poll(&mut cx).is_pending();
                self.slots.borrow_mut()[i] = if pending {
                    Slot::Used(Some(task))
                } else {
                    Slot::Free
                };
            }
        }
    }

    /// Runs `future` and the pool's tasks until `future` finishes.
    pub fn run_until<F: Future>(&self, future: F) -> F::Output {
        let mut cx = std::task::Context::from_waker(futures_util::task::noop_waker_ref());
        let mut future = std::pin::pin!(future);
        for _ in 0..10_000 {
            if let std::task::Poll::Ready(output) = future.as_mut().poll(&mut cx) {
                return output;
            }
            self.run_once();
        }
        panic!("the future did not finish");
    }
}

impl LocalSpawner for SlotPool {
    fn spawn_local<F>(&self, task: F) -> Result<(), SpawnError>
    where
        F: Future<Output = ()> + 'static,
    {
        let mut slots = self.slots.borrow_mut();
        let free = slots
            .iter_mut()
            .find(|slot| matches!(slot, Slot::Free))
            .ok_or(SpawnError::new("all slots are used"))?;
        *free = Slot::Used(Some(Box::pin(task)));
        Ok(())
    }
}
