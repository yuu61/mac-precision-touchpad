//! Persistent contact identity and single-frame liftoff synthesis.
//!
//! Windows HID PTP expects each physical contact to keep a stable `ContactID` from touch-down
//! (`TipSwitch = 1`) through movement (`TipSwitch = 1`) to a single liftoff frame
//! (`TipSwitch = 0`), after which the contact disappears from subsequent reports (`ContactCount`
//! decreases). In addition, once a contact is flagged as an unintended palm (`Confidence = 0`),
//! `Confidence` must remain `0` for the rest of that contact's lifetime, including its liftoff
//! report, whereas normal fingers keep `Confidence = 1` on liftoff.
//!
//! Apple trackpads split into two families:
//! - **Position-tracked hardware** (Wellspring TYPE1–4, T2, and SPI): raw finger slots carry no
//!   persistent hardware ID and shift indices whenever an earlier finger lifts. [`ContactMatching::Position`]
//!   matches active slots to current touches within a jump threshold by maximizing continued
//!   contacts and minimizing total squared Euclidean distance, emits 1-frame `TipSwitch = 0`
//!   liftoff reports at the last reported coordinates for vanished contacts, and assigns unused
//!   slot IDs to new touch-downs without reusing a slot in the same report as its liftoff.
//! - **Hardware-ID hardware** (Magic Trackpad 2 over USB or Bluetooth): each finger record carries
//!   a 4-bit hardware `ContactID` (`0..=15`). [`ContactMatching::HardwareId`] tracks up to
//!   five concurrent hardware IDs, latches `Confidence = 0` for palms, suppresses pre-touch hover
//!   records (`TipSwitch = 0` with no prior touch-down), limits each `ContactID` to at most one
//!   output entry per frame, and emits a single `TipSwitch = 0` liftoff frame at the last reported
//!   touch coordinates whether the hardware reports `TipSwitch = 0` or drops the record outright.

mod hardware_id;
mod position;

pub use position::{DEFAULT_MAX_DISTANCE_SQ, DEFAULT_MAX_JUMP_UNITS};

use crate::domain::{Contact, MAX_CONTACTS};

/// How observations are matched to previously active contacts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactMatching {
    /// Assign IDs by matching nearby positions in successive frames.
    Position,
    /// Use persistent IDs supplied by the hardware.
    HardwareId,
}

/// Contacts to report for a single frame, including any liftoffs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackedContacts {
    contacts: [Contact; MAX_CONTACTS],
    count: u8,
}

impl TrackedContacts {
    /// Empty contact set (`ContactCount = 0`).
    pub const EMPTY: Self = Self {
        contacts: [Contact::EMPTY; MAX_CONTACTS],
        count: 0,
    };

    /// Contacts in slots `0..count`, followed by [`Contact::EMPTY`] in the unused slots.
    #[must_use]
    pub const fn contacts(&self) -> &[Contact; MAX_CONTACTS] {
        &self.contacts
    }

    /// Number of valid entries in [`Self::contacts`].
    #[must_use]
    pub const fn count(&self) -> u8 {
        self.count
    }
}

impl Default for TrackedContacts {
    fn default() -> Self {
        Self::EMPTY
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActiveSlot {
    contact_id: u32,
    x: u16,
    y: u16,
    confidence: bool,
}

impl ActiveSlot {
    const fn from_touch(contact_id: u32, obs: Contact) -> Self {
        Self {
            contact_id,
            x: obs.x,
            y: obs.y,
            confidence: obs.confidence,
        }
    }

    const fn continued(self, obs: Contact) -> Self {
        Self {
            contact_id: self.contact_id,
            x: obs.x,
            y: obs.y,
            confidence: self.confidence & obs.confidence,
        }
    }

    const fn into_contact(self, tip_switch: bool) -> Contact {
        Contact {
            confidence: self.confidence,
            tip_switch,
            contact_id: self.contact_id,
            x: self.x,
            y: self.y,
        }
    }
}

/// Fixed-capacity (`5`-slot), `no_std`, allocation-free contact lifecycle tracker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContactTracker {
    slots: [Option<ActiveSlot>; MAX_CONTACTS],
    max_distance_sq: u64,
}

impl Default for ContactTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ContactTracker {
    /// Creates an empty tracker using [`DEFAULT_MAX_DISTANCE_SQ`].
    #[must_use]
    pub const fn new() -> Self {
        Self::with_max_distance_sq(DEFAULT_MAX_DISTANCE_SQ)
    }

    /// Creates an empty tracker with a custom squared-distance threshold for
    /// [`ContactMatching::Position`].
    #[must_use]
    pub const fn with_max_distance_sq(max_distance_sq: u64) -> Self {
        Self {
            slots: [None; MAX_CONTACTS],
            max_distance_sq,
        }
    }

    /// Clears all tracked contacts immediately without emitting liftoff frames.
    ///
    /// The caller resets the tracker at the boundary of an input session.
    pub const fn reset(&mut self) {
        self.slots = [None; MAX_CONTACTS];
    }

    /// Returns `true` when at least one active contact is currently tracked.
    #[must_use]
    pub fn has_active_contacts(&self) -> bool {
        self.slots.iter().any(Option::is_some)
    }

    /// Processes one frame. Disabling surface reporting clears active contacts without generating
    /// liftoffs, so touch reporting resumes with a fresh set of contacts.
    #[must_use]
    pub fn update(
        &mut self,
        matching: ContactMatching,
        surface_enabled: bool,
        observations: &[Contact],
    ) -> TrackedContacts {
        if !surface_enabled {
            self.reset();
            return TrackedContacts::EMPTY;
        }
        match matching {
            ContactMatching::Position => {
                position::update(&mut self.slots, self.max_distance_sq, observations)
            }
            ContactMatching::HardwareId => hardware_id::update(&mut self.slots, observations),
        }
    }
}

fn allocate_slot(
    slots: &mut [Option<ActiveSlot>; MAX_CONTACTS],
    prior_slots: &[Option<ActiveSlot>; MAX_CONTACTS],
    slot_output: &mut [Option<Contact>; MAX_CONTACTS],
    obs: Contact,
    use_slot_index_as_id: bool,
) {
    for (s_idx, ((slot_opt, prior_opt), out_opt)) in slots
        .iter_mut()
        .zip(prior_slots.iter())
        .zip(slot_output.iter_mut())
        .enumerate()
    {
        if prior_opt.is_none() && slot_opt.is_none() {
            let id = if use_slot_index_as_id {
                s_idx as u32
            } else {
                obs.contact_id
            };
            let created = ActiveSlot::from_touch(id, obs);
            *slot_opt = Some(created);
            *out_opt = Some(created.into_contact(true));
            break;
        }
    }
}

fn pack_slot_outputs(slot_output: &[Option<Contact>; MAX_CONTACTS]) -> TrackedContacts {
    let mut contacts = [Contact::EMPTY; MAX_CONTACTS];
    let mut count = 0_usize;
    for entry in slot_output.iter().copied().flatten() {
        if let Some(dst) = contacts.get_mut(count) {
            *dst = entry;
            count += 1;
        }
    }
    TrackedContacts {
        contacts,
        count: count as u8,
    }
}

#[cfg(test)]
const fn touch(id: u32, x: u16, y: u16, tip_switch: bool, confidence: bool) -> Contact {
    Contact {
        confidence,
        tip_switch,
        contact_id: id,
        x,
        y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_clears_active_state_without_liftoff() {
        let mut tracker = ContactTracker::new();
        let down = tracker.update(
            ContactMatching::Position,
            true,
            &[touch(0, 1000, 1000, true, true)],
        );
        assert_eq!(down.count, 1);
        assert!(tracker.has_active_contacts());
        tracker.reset();
        assert!(!tracker.has_active_contacts());
        assert_eq!(
            tracker.update(ContactMatching::Position, true, &[]),
            TrackedContacts::EMPTY
        );
    }

    #[test]
    fn surface_reporting_suppression_ends_active_contacts() {
        let observations = [touch(7, 1000, 1000, true, false)];
        let resumed = [touch(7, 1010, 1010, true, true)];
        for (matching, expected_id) in [
            (ContactMatching::Position, 0),
            (ContactMatching::HardwareId, 7),
        ] {
            let mut tracker = ContactTracker::new();
            let down = tracker.update(matching, true, &observations);
            assert_eq!(
                down.contacts()[0],
                touch(expected_id, 1000, 1000, true, false)
            );
            assert_eq!(
                tracker.update(matching, false, &observations),
                TrackedContacts::EMPTY
            );
            assert!(!tracker.has_active_contacts());
            assert_eq!(tracker.update(matching, true, &[]), TrackedContacts::EMPTY);
            let down = tracker.update(matching, true, &resumed);
            assert_eq!(
                down.contacts()[0],
                touch(expected_id, 1010, 1010, true, true)
            );
        }
    }

    #[test]
    fn random_streams_preserve_ptp_lifecycle_invariants() {
        let mut rng = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };

        for matching in [ContactMatching::Position, ContactMatching::HardwareId] {
            let mut tracker = ContactTracker::new();
            let mut prev = TrackedContacts::EMPTY;

            for _ in 0..500 {
                let len = (next() % 8) as usize;
                let mut obs = [Contact::EMPTY; 8];
                for dst in obs.iter_mut().take(len) {
                    *dst = touch(
                        (next() % 8) as u32,
                        (next() % 12_000) as u16,
                        (next() % 8_000) as u16,
                        !next().is_multiple_of(4),
                        !next().is_multiple_of(5),
                    );
                }

                let cur = tracker.update(matching, true, &obs[..len]);
                let cur_slice = &cur.contacts()[..usize::from(cur.count())];
                let prev_slice = &prev.contacts()[..usize::from(prev.count())];

                for (i, a) in cur_slice.iter().enumerate() {
                    assert!(
                        cur_slice[i + 1..]
                            .iter()
                            .all(|b| b.contact_id != a.contact_id)
                    );
                    if !a.tip_switch {
                        let p = prev_slice
                            .iter()
                            .find(|p| p.contact_id == a.contact_id && p.tip_switch)
                            .expect("liftoff must follow an active contact");
                        assert_eq!((a.x, a.y, a.confidence), (p.x, p.y, p.confidence));
                    }
                }
                for p in prev_slice.iter().filter(|p| p.tip_switch) {
                    let a = cur_slice
                        .iter()
                        .find(|a| a.contact_id == p.contact_id)
                        .expect("active contact must continue or lift off");
                    assert!(p.confidence || !a.confidence);
                }
                prev = cur;
            }
        }
    }
}
