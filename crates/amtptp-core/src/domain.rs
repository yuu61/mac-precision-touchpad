//! Contact tracking and surface-reporting rules for a touchpad session.
//!
//! Packet decoders supply contacts in logical coordinates. The tracker keeps contact identities
//! and emits a frame that the caller can encode for its report format.

mod contact_tracker;

pub use contact_tracker::{
    ContactMatching, ContactTracker, DEFAULT_MAX_DISTANCE_SQ, DEFAULT_MAX_JUMP_UNITS,
    TrackedContacts,
};

/// Maximum number of contacts reported in one frame, including liftoffs.
pub const MAX_CONTACTS: usize = 5;

/// A contact in the trackpad's logical coordinate space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Contact {
    /// Whether the contact is intended input.
    pub confidence: bool,
    /// Whether the contact is touching the surface.
    pub tip_switch: bool,
    /// The observed hardware ID or the ID assigned by the tracker.
    pub contact_id: u32,
    /// Logical X coordinate.
    pub x: u16,
    /// Logical Y coordinate.
    pub y: u16,
}

impl Contact {
    /// The value used to fill an unused slot.
    pub const EMPTY: Self = Self {
        confidence: false,
        tip_switch: false,
        contact_id: 0,
        x: 0,
        y: 0,
    };
}
