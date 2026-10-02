//! Using `async-deferred` with embassy on an ESP32-S3.
//!
//! A slow sensor measurement runs in the background while the main loop keeps doing its
//! own work. The loop picks up each result when it is ready, without waiting, and gives
//! up on a measurement that takes too long.
//!
//! Run with `cargo run` (after `source ~/export-esp.sh`).

#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use async_deferred::{embassy_spawner, Deferred};
use defmt::{error, info, warn};
use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

// One measurement runs at a time, so one task slot is enough.
embassy_spawner!(DeferredSpawner, pool_size = 1);

/// Gives up on a measurement that takes longer than this.
const MEASUREMENT_TIMEOUT: Duration = Duration::from_millis(500);

/// Simulates a slow sensor: every fourth measurement hangs.
async fn measure(n: u32) -> u32 {
    let duration = if n % 4 == 3 { 2_000 } else { 200 + 50 * (n % 3) as u64 };
    Timer::after(Duration::from_millis(duration)).await;
    20 + n % 5
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    rtt_target::rtt_init_defmt!();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let spawner = DeferredSpawner(spawner);

    let mut n = 0;
    let mut measurement = Deferred::start_with_callback_local_on(&spawner, measure(n), |v| {
        info!("measurement finished: {}", v)
    })
    .expect("the task pool is empty at startup");
    let mut started = Instant::now();

    loop {
        // The main loop's own work: here, just a tick every 100 ms.
        Timer::after(Duration::from_millis(100)).await;

        // Check for the result without waiting.
        if let Some(value) = measurement.take() {
            info!("#{}: {} °C after {} ms", n, value, started.elapsed().as_millis());
        } else if started.elapsed() > MEASUREMENT_TIMEOUT {
            // Wait until the hung task has stopped, so its pool slot is free again.
            if measurement.cancel_and_wait().await {
                warn!("#{}: no answer after {} ms, cancelled", n, started.elapsed().as_millis());
            } else {
                // Starting it failed, so nothing was running: just try the next one.
                warn!("#{}: was not running", n);
            }
        } else {
            continue;
        }

        // `take` and `cancel_and_wait` reset the `Deferred`, so it can start the next one.
        n += 1;
        let begun = measurement.begin_with_callback_local_on(&spawner, measure(n), |v| {
            info!("measurement finished: {}", v)
        });
        if begun.is_err() {
            error!("#{}: could not start the measurement", n);
        }
        started = Instant::now();
    }
}
