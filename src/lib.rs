#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]
// On docs.rs, label each item with the features it needs.
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(feature = "alloc")]
extern crate alloc;

// `Deferred` uses atomic compare-and-swap, through `Arc` and the `futures` channels.
#[cfg(all(feature = "alloc", not(target_has_atomic = "ptr")))]
compile_error!(
    "`Deferred` (the `alloc` feature of async-deferred) needs atomic compare-and-swap, which \
     this target does not have (for example Cortex-M0 or ESP32-C3). Use `StaticDeferred` \
     instead: disable `alloc` and enable the `static-deferred` feature."
);

#[cfg(all(feature = "alloc", target_has_atomic = "ptr"))]
mod deferred;
mod embassy;
mod error;
mod spawner;
#[cfg(feature = "static-deferred")]
mod static_deferred;

#[cfg(feature = "alloc")]
#[doc(hidden)]
pub mod __private {
    pub use alloc::boxed::Box;
}

#[cfg(all(feature = "alloc", target_has_atomic = "ptr"))]
#[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
pub use deferred::{Deferred, IntoResult};
pub use error::{BeginError, Error, SpawnError, State};
#[cfg(feature = "smol")]
#[cfg_attr(docsrs, doc(cfg(feature = "smol")))]
pub use spawner::Smol;
#[cfg(feature = "tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
pub use spawner::Tokio;
pub use spawner::{LocalSpawner, Spawner};
#[cfg(feature = "static-deferred")]
#[cfg_attr(docsrs, doc(cfg(feature = "static-deferred")))]
pub use static_deferred::{StaticDeferred, Ticket};
