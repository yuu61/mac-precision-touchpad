//! Apple USB and Bluetooth trackpad protocols and driver logic.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

pub mod application;
pub mod infrastructure;
pub mod interface;

pub use infrastructure::bcm5974;
pub use interface::{bluetooth, usb};
