//! `async-deferred` on embassy, running on the chip with `embedded-test`.
//!
//! Run with `cargo test` (after `source ~/export-esp.sh`).

#![no_std]
#![no_main]

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

const TASK_POOL_SIZE: usize = 4;

async_deferred::embassy_spawner!(Embassy, pool_size = TASK_POOL_SIZE);
async_deferred::embassy_spawner!(SingleSlot, pool_size = 1);

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
    use async_deferred::{BeginError, Deferred, Error, State};
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
        let mut deferred = Deferred::start_local_on(&embassy().await, async { 42u32 }).unwrap();
        assert!(deferred.join().await == Ok(&42));
        assert!(deferred.state() == State::Completed);
    }

    #[test]
    async fn await_deferred_directly() {
        let deferred = Deferred::start_local_on(&embassy().await, async {
            sleep_ms(10).await;
            Rc::new(42u32)
        })
        .unwrap();
        assert!(deferred.await.map(|v| *v) == Ok(42));
    }

    #[test]
    async fn pending_then_ready_without_join() {
        let mut deferred = Deferred::start_local_on(&embassy().await, async {
            sleep_ms(50).await;
            42u32
        }).unwrap();
        assert!(deferred.state() == State::Pending);
        assert!(deferred.try_get().is_none());
        sleep_ms(100).await;
        assert!(deferred.try_get() == Some(&42));
    }

    #[test]
    async fn non_send_future_and_result() {
        let shared = Rc::new(41u32);
        let mut deferred =
            Deferred::start_local_on(&embassy().await, async move { Rc::new(*shared + 1) }).unwrap();
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
        ).unwrap();
        deferred.join().await.unwrap();
        assert!(seen.get() == 42);
    }

    #[test]
    async fn begin_rejected_while_running() {
        let spawner = embassy().await;
        let mut deferred = Deferred::new();
        assert!(deferred
            .begin_local_on(&spawner, async {
                sleep_ms(20).await;
                1u32
            })
            .is_ok());
        assert!(deferred.begin_local_on(&spawner, async { 2 }) == Err(BeginError::AlreadyStarted));
        assert!(deferred.join().await == Ok(&1));
    }

    #[test]
    async fn take_resets_and_restarts() {
        let spawner = embassy().await;
        let mut deferred = Deferred::start_local_on(&spawner, async { 1u32 }).unwrap();
        deferred.join().await.unwrap();
        assert!(deferred.take() == Some(1));
        assert!(deferred.state() == State::NotStarted);
        assert!(deferred.join().await == Err(Error::NotStarted));
        assert!(deferred.begin_local_on(&spawner, async { 2 }).is_ok());
        assert!(deferred.join().await == Ok(&2));
    }

    #[test]
    async fn cancel_stops_the_task() {
        let dropped = Rc::new(Cell::new(false));
        let flag = DropFlag(dropped.clone());
        let mut deferred = Deferred::start_local_on(&embassy().await, async move {
            let _flag = flag;
            core::future::pending::<u32>().await
        }).unwrap();
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
                .map(|_| Deferred::start_local_on(&spawner, core::future::pending::<u32>()).unwrap())
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
            let mut deferred = Deferred::start_local_on(&spawner, async move { i * 2 }).unwrap();
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
                }).unwrap()
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
        }).unwrap();
        drop(deferred);
        sleep_ms(50).await;
        assert!(done.get());
    }

    async fn single_slot() -> SingleSlot {
        // SAFETY: tests run inside the embassy executor.
        SingleSlot(unsafe { embassy_executor::Spawner::for_current_executor() }.await)
    }

    /// `cancel` returns before the executor ran the task again: its slot is still used.
    #[test]
    async fn begin_right_after_cancel_reports_full_pool() {
        let spawner = single_slot().await;
        let mut deferred =
            Deferred::start_local_on(&spawner, core::future::pending::<u32>()).unwrap();
        sleep_ms(10).await;
        assert!(deferred.cancel());
        assert!(matches!(
            deferred.begin_local_on(&spawner, async { 42 }),
            Err(BeginError::Spawn(_))
        ));
        assert!(deferred.state() == State::NotStarted);
    }

    /// `cancel_and_wait` returns once the slot is free, so a restart always works.
    #[test]
    async fn restart_after_cancel_and_wait_with_one_slot() {
        let spawner = single_slot().await;
        for i in 0..100u32 {
            let mut deferred =
                Deferred::start_local_on(&spawner, core::future::pending::<u32>()).unwrap();
            if i % 2 == 0 {
                sleep_ms(1).await; // cancel both before and after the task first ran
            }
            assert!(deferred.cancel_and_wait().await);
            assert!(deferred.begin_local_on(&spawner, async move { i }).is_ok());
            assert!(deferred.join().await == Ok(&i));
        }
    }

    /// Starting a task in a full pool returns an error instead of panicking.
    #[test]
    async fn full_pool_returns_error() {
        let spawner = embassy().await;
        let mut running: Vec<Deferred<u32>> = (0..TASK_POOL_SIZE)
            .map(|_| Deferred::start_local_on(&spawner, core::future::pending::<u32>()).unwrap())
            .collect();
        let refused = Deferred::start_local_on(&spawner, async { 0u32 });
        assert!(refused.err().map(|e| e.reason())
            == Some("Embassy: all `pool_size` tasks are running"));
        for deferred in &mut running {
            assert!(deferred.cancel_and_wait().await);
        }
        assert!(Deferred::start_local_on(&spawner, async { 0u32 }).is_ok());
    }

    /// Without `std`, a panic is not caught: it reaches the panic handler.
    #[test]
    #[should_panic]
    async fn task_panic_reaches_panic_handler() {
        let mut deferred: Deferred<u32> =
            Deferred::start_local_on(&embassy().await, async { panic!("boom") }).unwrap();
        let _ = deferred.join().await;
    }
}
