//! `async-deferred` on embassy, running on the chip with `embedded-test`.
//!
//! Run with `cargo test` (after `source ~/export-esp.sh`).

#![no_std]
#![no_main]

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

const TASK_POOL_SIZE: usize = 4;

async_deferred::embassy_spawner!(Embassy, pool_size = TASK_POOL_SIZE);

async fn embassy() -> Embassy {
    // SAFETY: tests run inside the embassy executor.
    Embassy(unsafe { embassy_executor::Spawner::for_current_executor() }.await)
}

#[cfg(test)]
#[embedded_test::tests(executor = esp_rtos::embassy::Executor::new())]
mod tests {
    use super::*;
    use alloc::rc::Rc;
    use alloc::vec::Vec;
    use async_deferred::{Deferred, Error, State};
    use core::cell::Cell;
    use embassy_time::{Duration, Timer};

    /// Sets a flag when dropped, to see that a task was stopped.
    struct DropFlag(Rc<Cell<bool>>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    async fn sleep_ms(ms: u64) {
        Timer::after(Duration::from_millis(ms)).await;
    }

    #[init]
    fn init() {
        let peripherals = esp_hal::init(esp_hal::Config::default());
        esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);

        let timg0 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
        esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

        rtt_target::rtt_init_defmt!();
    }

    #[test]
    async fn join_returns_result() {
        let mut deferred = Deferred::start_local_on(&embassy().await, async { 42u32 });
        assert!(deferred.join().await == Ok(&42));
        assert!(deferred.state() == State::Completed);
    }

    #[test]
    async fn pending_then_ready_without_join() {
        let mut deferred = Deferred::start_local_on(&embassy().await, async {
            sleep_ms(50).await;
            42u32
        });
        assert!(deferred.state() == State::Pending);
        assert!(deferred.try_get().is_none());
        sleep_ms(100).await;
        assert!(deferred.try_get() == Some(&42));
    }

    #[test]
    async fn non_send_future_and_result() {
        let shared = Rc::new(41u32);
        let mut deferred =
            Deferred::start_local_on(&embassy().await, async move { Rc::new(*shared + 1) });
        assert!(deferred.join().await.map(|v| **v) == Ok(42));
    }

    #[test]
    async fn callback_receives_result() {
        let seen = Rc::new(Cell::new(0u32));
        let seen_in_callback = seen.clone();
        let mut deferred = Deferred::start_with_callback_local_on(
            &embassy().await,
            async { 42u32 },
            move |v| seen_in_callback.set(*v),
        );
        deferred.join().await.unwrap();
        assert!(seen.get() == 42);
    }

    #[test]
    async fn begin_rejected_while_running() {
        let spawner = embassy().await;
        let mut deferred = Deferred::new();
        assert!(deferred.begin_local_on(&spawner, async {
            sleep_ms(20).await;
            1u32
        }));
        assert!(!deferred.begin_local_on(&spawner, async { 2 }));
        assert!(deferred.join().await == Ok(&1));
    }

    #[test]
    async fn take_resets_and_restarts() {
        let spawner = embassy().await;
        let mut deferred = Deferred::start_local_on(&spawner, async { 1u32 });
        deferred.join().await.unwrap();
        assert!(deferred.take() == Some(1));
        assert!(deferred.state() == State::NotStarted);
        assert!(deferred.join().await == Err(Error::NotStarted));
        assert!(deferred.begin_local_on(&spawner, async { 2 }));
        assert!(deferred.join().await == Ok(&2));
    }

    #[test]
    async fn cancel_stops_the_task() {
        let dropped = Rc::new(Cell::new(false));
        let flag = DropFlag(dropped.clone());
        let mut deferred = Deferred::start_local_on(&embassy().await, async move {
            let _flag = flag;
            core::future::pending::<u32>().await
        });
        sleep_ms(10).await;
        assert!(deferred.cancel());
        assert!(deferred.state() == State::NotStarted);
        sleep_ms(10).await;
        assert!(dropped.get());
    }

    /// Cancelled tasks must free their embassy pool slot, or the pool fills up.
    #[test]
    async fn cancel_frees_pool_slots() {
        let spawner = embassy().await;
        for _ in 0..3 {
            let mut running: Vec<Deferred<u32>> = (0..TASK_POOL_SIZE)
                .map(|_| Deferred::start_local_on(&spawner, core::future::pending::<u32>()))
                .collect();
            for deferred in &mut running {
                assert!(deferred.cancel());
            }
            sleep_ms(10).await;
        }
    }

    /// Finished tasks must free their pool slot and their heap memory.
    #[test]
    async fn many_tasks_in_sequence() {
        let spawner = embassy().await;
        for i in 0..1000u32 {
            let mut deferred = Deferred::start_local_on(&spawner, async move { i * 2 });
            assert!(deferred.join().await == Ok(&(i * 2)));
        }
    }

    #[test]
    async fn concurrent_tasks() {
        let spawner = embassy().await;
        let mut running: Vec<Deferred<u32>> = (0..TASK_POOL_SIZE as u32)
            .map(|i| {
                Deferred::start_local_on(&spawner, async move {
                    sleep_ms(10 * u64::from(i)).await;
                    i
                })
            })
            .collect();
        for (i, deferred) in running.iter_mut().enumerate() {
            assert!(deferred.join().await == Ok(&(i as u32)));
        }
    }

    /// Dropping a `Deferred` leaves its task running.
    #[test]
    async fn dropping_deferred_keeps_task_running() {
        let done = Rc::new(Cell::new(false));
        let done_in_task = done.clone();
        let deferred = Deferred::start_local_on(&embassy().await, async move {
            sleep_ms(10).await;
            done_in_task.set(true);
        });
        drop(deferred);
        sleep_ms(50).await;
        assert!(done.get());
    }

    /// Starting more tasks than the pool holds panics with a clear message.
    #[test]
    #[should_panic]
    async fn too_many_tasks_panics() {
        let spawner = embassy().await;
        let _running: Vec<Deferred<u32>> = (0..=TASK_POOL_SIZE)
            .map(|_| Deferred::start_local_on(&spawner, core::future::pending::<u32>()))
            .collect();
    }

    /// Without `std`, a panic is not caught: it reaches the panic handler.
    #[test]
    #[should_panic]
    async fn task_panic_reaches_panic_handler() {
        let mut deferred: Deferred<u32> =
            Deferred::start_local_on(&embassy().await, async { panic!("boom") });
        let _ = deferred.join().await;
    }
}
