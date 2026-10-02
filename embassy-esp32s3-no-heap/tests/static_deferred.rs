//! `StaticDeferred` on embassy with no heap allocator, run on the chip with
//! `embedded-test`.
//!
//! Run with `cargo test` (after `source ~/export-esp.sh`).

#![no_std]
#![no_main]

use async_deferred::{StaticDeferred, Ticket};
use embassy_time::{Duration, Timer};

esp_bootloader_esp_idf::esp_app_desc!();

/// Runs one job: waits `ms` milliseconds, then returns `value`.
/// Its task pool has a single slot.
#[embassy_executor::task]
async fn job(ticket: Ticket<'static, u32>, ms: u64, value: u32) {
    ticket
        .run(async move {
            Timer::after(Duration::from_millis(ms)).await;
            value
        })
        .await;
}

async fn spawner() -> embassy_executor::SendSpawner {
    embassy_executor::SendSpawner::for_current_executor().await
}

#[cfg(test)]
#[embedded_test::tests(executor = esp_rtos::embassy::Executor::new())]
mod tests {
    use super::*;
    use async_deferred::{Error, State};

    #[init]
    fn init() {
        let peripherals = esp_hal::init(esp_hal::Config::default());

        let timg0 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
        esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

        rtt_target::rtt_init_defmt!();
    }

    #[test]
    async fn join_returns_the_result() {
        static VALUE: StaticDeferred<u32> = StaticDeferred::new();
        spawner()
            .await
            .spawn(job(VALUE.begin().unwrap(), 10, 42).unwrap());
        assert!(VALUE.state() == State::Pending);
        assert!(VALUE.join().await == Ok(42));
        assert!(VALUE.state() == State::NotStarted);
    }

    #[test]
    async fn take_without_waiting() {
        static VALUE: StaticDeferred<u32> = StaticDeferred::new();
        spawner()
            .await
            .spawn(job(VALUE.begin().unwrap(), 20, 7).unwrap());
        assert!(VALUE.take().is_none());
        Timer::after(Duration::from_millis(50)).await;
        assert!(VALUE.with_result(|v| *v) == Some(7));
        assert!(VALUE.take() == Some(7));
    }

    #[test]
    async fn cancel_and_wait_frees_the_task_slot() {
        static VALUE: StaticDeferred<u32> = StaticDeferred::new();
        let spawner = spawner().await;
        spawner.spawn(job(VALUE.begin().unwrap(), 10_000, 1).unwrap());
        Timer::after(Duration::from_millis(10)).await;

        assert!(VALUE.cancel_and_wait().await);
        // The only slot of `job` is free again.
        spawner.spawn(job(VALUE.begin().unwrap(), 0, 2).unwrap());
        assert!(VALUE.join().await == Ok(2));
    }

    #[test]
    async fn full_pool_gives_the_ticket_back() {
        static VALUE: StaticDeferred<u32> = StaticDeferred::new();
        let spawner = spawner().await;
        spawner.spawn(job(VALUE.begin().unwrap(), 10_000, 1).unwrap());
        Timer::after(Duration::from_millis(10)).await;

        assert!(VALUE.cancel());
        // The cancelled task still holds the slot: the spawn fails and drops the ticket.
        assert!(job(VALUE.begin().unwrap(), 0, 2).is_err());
        assert!(VALUE.state() == State::NotStarted);

        VALUE.cancel_and_wait().await;
        spawner.spawn(job(VALUE.begin().unwrap(), 0, 3).unwrap());
        assert!(VALUE.join().await == Ok(3));
    }

    #[test]
    async fn many_runs_in_sequence() {
        static VALUE: StaticDeferred<u32> = StaticDeferred::new();
        let spawner = spawner().await;
        for i in 0..1000 {
            spawner.spawn(job(VALUE.begin().unwrap(), 0, i).unwrap());
            assert!(VALUE.join().await == Ok(i));
        }
    }

    #[test]
    async fn join_without_a_run() {
        static VALUE: StaticDeferred<u32> = StaticDeferred::new();
        assert!(VALUE.join().await == Err(Error::NotStarted));
        assert!(!VALUE.cancel_and_wait().await);
    }
}
