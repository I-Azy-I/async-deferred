#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
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

#[cfg(feature = "alloc")]
pub use deferred::{Deferred, IntoResult};
pub use error::{BeginError, Error, SpawnError, State};
#[cfg(feature = "smol")]
pub use spawner::Smol;
#[cfg(feature = "tokio")]
pub use spawner::Tokio;
pub use spawner::{LocalSpawner, Spawner};
#[cfg(feature = "static-deferred")]
pub use static_deferred::{StaticDeferred, Ticket};
