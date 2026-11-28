//! Touchpad domain, Windows HID report formats and SPI driver logic.

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
pub mod domain;
pub mod infrastructure;
pub mod interface;

pub use infrastructure::hid;
pub use interface::spi;
