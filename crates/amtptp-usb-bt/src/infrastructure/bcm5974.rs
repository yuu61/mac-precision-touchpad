//! Wellspring and Magic Trackpad 2 hardware formats shared by the GPLv2 drivers.
//!
//! The Wellspring definitions descend from Linux's [bcm5974 driver] through the original C port.
//! The Magic Trackpad 2 `TYPE5` definitions match the [magictrackpad2-dkms patch].
//!
//! [bcm5974 driver]: https://github.com/torvalds/linux/blob/master/drivers/input/mouse/bcm5974.c
//! [magictrackpad2-dkms patch]: https://github.com/robbi5/magictrackpad2-dkms/blob/master/usr/src/bcm5974-4.4%2Bmagictrackpad2/bcm5974-magictrackpad.diff
pub mod config;
pub mod input;
pub mod usb;
