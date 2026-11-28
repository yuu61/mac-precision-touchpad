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
//!   matches active slots to current touches by nearest-neighbor Euclidean distance
//!   within a jump threshold, emits 1-frame `TipSwitch = 0` liftoff reports for vanished contacts,
//!   and assigns unused slot IDs to new touch-downs without reusing a slot in the same report as
//!   its liftoff.
//! - **Hardware-ID hardware** (Magic Trackpad 2 over USB or Bluetooth): each finger record carries
//!   a 4-bit hardware `ContactID` (`0..=15`). [`ContactMatching::HardwareId`] tracks up to
//!   five concurrent hardware IDs, latches `Confidence = 0` for palms, suppresses pre-touch hover
//!   records (`TipSwitch = 0` with no prior touch-down), and emits exactly one `TipSwitch = 0`
//!   liftoff frame whether the hardware reports a `TipSwitch = 0` record or drops the record
//!   outright.

use crate::domain::{Contact, MAX_CONTACTS};

/// Default maximum movement per frame (`3200` logical units) accepted by nearest-neighbor matching
/// before a touch is treated as a distinct finger rather than the continuation of an old slot.
pub const DEFAULT_MAX_JUMP_UNITS: u32 = 3200;

/// Squared Euclidean distance limit corresponding to [`DEFAULT_MAX_JUMP_UNITS`].
pub const DEFAULT_MAX_DISTANCE_SQ: u64 =
    (DEFAULT_MAX_JUMP_UNITS as u64) * (DEFAULT_MAX_JUMP_UNITS as u64);

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
            ContactMatching::Position => self.update_tracked(observations),
            ContactMatching::HardwareId => self.update_hardware_ids(observations),
        }
    }

    /// Updates contact state for hardware without persistent contact IDs (Wellspring TYPE1–4, T2,
    /// and SPI).
    ///
    /// Only candidates with `tip_switch == true` (up to [`MAX_CONTACTS`]) are treated as
    /// active surface touches; candidates with `tip_switch == false` are ignored as hover/below
    /// threshold, allowing previously active slots to emit a clean 1-frame liftoff at their last
    /// touch coordinates. Assigned `contact_id`s are stable slot indices `0..=4`.
    fn update_tracked(&mut self, candidates: &[Contact]) -> TrackedContacts {
        let mut touches = [Contact::EMPTY; MAX_CONTACTS];
        let mut touch_len = 0_usize;
        for candidate in candidates.iter().copied() {
            if candidate.tip_switch {
                if let Some(dst) = touches.get_mut(touch_len) {
                    *dst = candidate;
                    touch_len += 1;
                } else {
                    break;
                }
            }
        }

        let mut was_occupied = [false; MAX_CONTACTS];
        for (flag, slot) in was_occupied.iter_mut().zip(self.slots.iter()) {
            *flag = slot.is_some();
        }

        let mut slot_matched = [false; MAX_CONTACTS];
        let mut touch_matched = [false; MAX_CONTACTS];
        let mut matched_obs: [Option<Contact>; MAX_CONTACTS] = [None; MAX_CONTACTS];

        for _ in 0..MAX_CONTACTS {
            let mut best: Option<(usize, usize, u64)> = None;
            for (s_idx, (slot_opt, &s_used)) in
                self.slots.iter().zip(slot_matched.iter()).enumerate()
            {
                let Some(slot) = slot_opt else {
                    continue;
                };
                if s_used {
                    continue;
                }
                for (t_idx, (&touch, &t_used)) in touches
                    .iter()
                    .zip(touch_matched.iter())
                    .take(touch_len)
                    .enumerate()
                {
                    if t_used {
                        continue;
                    }
                    let dist = distance_sq(slot.x, slot.y, touch.x, touch.y);
                    if dist <= self.max_distance_sq
                        && best.is_none_or(|(_, _, best_dist)| dist < best_dist)
                    {
                        best = Some((s_idx, t_idx, dist));
                    }
                }
            }

            let Some((best_s, best_t, _)) = best else {
                break;
            };
            if let Some(flag) = slot_matched.get_mut(best_s) {
                *flag = true;
            }
            if let Some(flag) = touch_matched.get_mut(best_t) {
                *flag = true;
            }
            if let (Some(dst), Some(&obs)) = (matched_obs.get_mut(best_s), touches.get(best_t)) {
                *dst = Some(obs);
            }
        }

        let mut slot_output: [Option<Contact>; MAX_CONTACTS] = [None; MAX_CONTACTS];

        // 1. Update matched active slots and emit liftoff for unmatched active slots.
        for (s_idx, ((slot_opt, &matched), (obs_opt, out_opt))) in self
            .slots
            .iter_mut()
            .zip(slot_matched.iter())
            .zip(matched_obs.iter().zip(slot_output.iter_mut()))
            .enumerate()
        {
            let contact_id = s_idx as u32;
            if matched {
                if let (Some(slot), &Some(obs)) = (slot_opt.as_mut(), obs_opt) {
                    slot.x = obs.x;
                    slot.y = obs.y;
                    slot.confidence &= obs.confidence;
                    *out_opt = Some(Contact {
                        confidence: slot.confidence,
                        tip_switch: true,
                        contact_id,
                        x: slot.x,
                        y: slot.y,
                    });
                }
            } else if let Some(slot) = slot_opt.take() {
                *out_opt = Some(Contact {
                    confidence: slot.confidence,
                    tip_switch: false,
                    contact_id,
                    x: slot.x,
                    y: slot.y,
                });
            } else {
                // Slot was already empty before this frame.
            }
        }

        // 2. Allocate empty slots (excluding slots emitting liftoff this frame) for new touches.
        for (&obs, &t_used) in touches.iter().zip(touch_matched.iter()).take(touch_len) {
            if t_used {
                continue;
            }
            for (s_idx, ((slot_opt, &occupied_before), out_opt)) in self
                .slots
                .iter_mut()
                .zip(was_occupied.iter())
                .zip(slot_output.iter_mut())
                .enumerate()
            {
                if !occupied_before && slot_opt.is_none() {
                    let contact_id = s_idx as u32;
                    *slot_opt = Some(ActiveSlot {
                        contact_id,
                        x: obs.x,
                        y: obs.y,
                        confidence: obs.confidence,
                    });
                    *out_opt = Some(Contact {
                        confidence: obs.confidence,
                        tip_switch: true,
                        contact_id,
                        x: obs.x,
                        y: obs.y,
                    });
                    break;
                }
            }
        }

        pack_slot_outputs(&slot_output)
    }

    /// Updates contact state for hardware that supplies persistent per-finger `ContactID`s
    /// (Magic Trackpad 2 over USB or Bluetooth).
    fn update_hardware_ids(&mut self, observations: &[Contact]) -> TrackedContacts {
        let mut was_occupied = [false; MAX_CONTACTS];
        for (flag, slot) in was_occupied.iter_mut().zip(self.slots.iter()) {
            *flag = slot.is_some();
        }

        let mut slot_seen = [false; MAX_CONTACTS];
        let mut slot_output: [Option<Contact>; MAX_CONTACTS] = [None; MAX_CONTACTS];
        let mut new_touches: [Option<Contact>; MAX_CONTACTS] = [None; MAX_CONTACTS];

        for obs in observations.iter().copied() {
            // Check whether `obs.contact_id` belongs to a slot that was active in the prior frame.
            let mut matched_existing = false;
            for ((slot_opt, (&occupied_before, seen)), out_opt) in self
                .slots
                .iter_mut()
                .zip(was_occupied.iter().zip(slot_seen.iter_mut()))
                .zip(slot_output.iter_mut())
            {
                if !occupied_before {
                    continue;
                }
                let matches_id = slot_opt
                    .as_ref()
                    .is_some_and(|s| s.contact_id == obs.contact_id);
                if matches_id {
                    matched_existing = true;
                    if !*seen {
                        *seen = true;
                        if obs.tip_switch {
                            if let Some(slot) = slot_opt.as_mut() {
                                slot.x = obs.x;
                                slot.y = obs.y;
                                slot.confidence &= obs.confidence;
                                *out_opt = Some(Contact {
                                    confidence: slot.confidence,
                                    tip_switch: true,
                                    contact_id: slot.contact_id,
                                    x: slot.x,
                                    y: slot.y,
                                });
                            }
                        } else if let Some(slot) = slot_opt.take() {
                            *out_opt = Some(Contact {
                                confidence: slot.confidence,
                                tip_switch: false,
                                contact_id: slot.contact_id,
                                x: obs.x,
                                y: obs.y,
                            });
                        } else {
                            // Unreachable: `matches_id` confirmed `slot_opt.is_some()`.
                        }
                    }
                    break;
                }
            }

            // Ignore `tip_switch == false` records that were not previously active (hover).
            if matched_existing || !obs.tip_switch {
                continue;
            }

            // Queue distinct new touch-downs for free slots.
            let already_queued = new_touches
                .iter()
                .flatten()
                .any(|queued| queued.contact_id == obs.contact_id);
            if !already_queued
                && let Some(dst) = new_touches.iter_mut().find(|entry| entry.is_none())
            {
                *dst = Some(obs);
            }
        }

        // Any previously active slot absent from this packet emits a 1-frame liftoff at its last
        // known position.
        for ((slot_opt, (&occupied_before, &seen)), out_opt) in self
            .slots
            .iter_mut()
            .zip(was_occupied.iter().zip(slot_seen.iter()))
            .zip(slot_output.iter_mut())
        {
            if occupied_before
                && !seen
                && let Some(slot) = slot_opt.take()
            {
                *out_opt = Some(Contact {
                    confidence: slot.confidence,
                    tip_switch: false,
                    contact_id: slot.contact_id,
                    x: slot.x,
                    y: slot.y,
                });
            }
        }

        // Assign genuinely new touch-downs to slots that were neither active nor lifting off.
        for obs in new_touches.into_iter().flatten() {
            for ((slot_opt, &occupied_before), out_opt) in self
                .slots
                .iter_mut()
                .zip(was_occupied.iter())
                .zip(slot_output.iter_mut())
            {
                if !occupied_before && slot_opt.is_none() {
                    *slot_opt = Some(ActiveSlot {
                        contact_id: obs.contact_id,
                        x: obs.x,
                        y: obs.y,
                        confidence: obs.confidence,
                    });
                    *out_opt = Some(Contact {
                        confidence: obs.confidence,
                        tip_switch: true,
                        contact_id: obs.contact_id,
                        x: obs.x,
                        y: obs.y,
                    });
                    break;
                }
            }
        }

        pack_slot_outputs(&slot_output)
    }
}

const fn distance_sq(x1: u16, y1: u16, x2: u16, y2: u16) -> u64 {
    let dx = (x1 as i32 - x2 as i32).unsigned_abs() as u64;
    let dy = (y1 as i32 - y2 as i32).unsigned_abs() as u64;
    dx * dx + dy * dy
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
mod tests {
    use super::*;

    const fn touch(id: u32, x: u16, y: u16, tip_switch: bool, confidence: bool) -> Contact {
        Contact {
            confidence,
            tip_switch,
            contact_id: id,
            x,
            y,
        }
    }

    #[test]
    fn tracked_two_finger_first_lifts_keeps_second_id_stable() {
        let mut tracker = ContactTracker::new();

        // Frame 1: two fingers touch down at (1000, 2000) and (5000, 3000).
        let f1 = tracker.update_tracked(&[
            touch(0, 1000, 2000, true, true),
            touch(1, 5000, 3000, true, true),
        ]);
        assert_eq!(f1.count, 2);
        assert_eq!(f1.contacts[0], touch(0, 1000, 2000, true, true));
        assert_eq!(f1.contacts[1], touch(1, 5000, 3000, true, true));

        // Frame 2: finger 0 lifts; hardware shifts finger 1 into raw slot 0 at (5050, 3020).
        let f2 = tracker.update_tracked(&[touch(0, 5050, 3020, true, true)]);
        assert_eq!(f2.count, 2);
        assert_eq!(f2.contacts[0], touch(0, 1000, 2000, false, true));
        assert_eq!(f2.contacts[1], touch(1, 5050, 3020, true, true));

        // Frame 3: finger 0 is gone; finger 1 still keeps ContactID = 1!
        let f3 = tracker.update_tracked(&[touch(0, 5100, 3040, true, true)]);
        assert_eq!(f3.count, 1);
        assert_eq!(f3.contacts[0], touch(1, 5100, 3040, true, true));
        assert_eq!(f3.contacts[1], Contact::EMPTY);

        // Frame 4: finger 1 lifts (empty packet -> 1 liftoff report).
        let f4 = tracker.update_tracked(&[]);
        assert_eq!(f4.count, 1);
        assert_eq!(f4.contacts[0], touch(1, 5100, 3040, false, true));

        // Frame 5: another empty packet -> ContactCount = 0.
        let f5 = tracker.update_tracked(&[]);
        assert_eq!(f5, TrackedContacts::EMPTY);
    }

    #[test]
    fn tracked_rejects_cross_pad_jump_and_does_not_reuse_liftoff_id_same_frame() {
        let mut tracker = ContactTracker::new();

        let f1 = tracker.update_tracked(&[touch(0, 500, 500, true, true)]);
        assert_eq!(f1.count, 1);
        assert_eq!(f1.contacts[0], touch(0, 500, 500, true, true));

        // Next frame: finger at (500, 500) lifts and a different finger touches at (9000, 6000).
        let f2 = tracker.update_tracked(&[touch(0, 9000, 6000, true, true)]);
        assert_eq!(f2.count, 2);
        assert_eq!(f2.contacts[0], touch(0, 500, 500, false, true));
        assert_eq!(f2.contacts[1], touch(1, 9000, 6000, true, true));

        // Following frame: only ContactID = 1 remains active.
        let f3 = tracker.update_tracked(&[touch(0, 9010, 6005, true, true)]);
        assert_eq!(f3.count, 1);
        assert_eq!(f3.contacts[0], touch(1, 9010, 6005, true, true));
    }

    #[test]
    fn tracked_latches_unintended_palm_confidence_through_liftoff() {
        let mut tracker = ContactTracker::new();

        // Starts normal, then expands into a palm, then shrinks before lifting.
        let down = tracker.update_tracked(&[touch(0, 2000, 2000, true, true)]);
        assert_eq!(down.count, 1);
        let palm = tracker.update_tracked(&[touch(0, 2010, 2010, true, false)]);
        assert_eq!(palm.contacts[0], touch(0, 2010, 2010, true, false));

        let still_latched = tracker.update_tracked(&[touch(0, 2020, 2020, true, true)]);
        assert_eq!(still_latched.contacts[0], touch(0, 2020, 2020, true, false));

        let liftoff = tracker.update_tracked(&[]);
        assert_eq!(liftoff.count, 1);
        assert_eq!(liftoff.contacts[0], touch(0, 2020, 2020, false, false));
    }

    #[test]
    fn tracked_ignores_initial_hover() {
        let mut tracker = ContactTracker::new();
        let hover = tracker.update_tracked(&[touch(0, 1000, 1000, false, true)]);
        assert_eq!(hover, TrackedContacts::EMPTY);

        let down = tracker.update_tracked(&[
            touch(0, 1000, 1000, true, true),
            touch(1, 2000, 2000, false, true),
            touch(2, 3000, 3000, true, true),
        ]);
        assert_eq!(down.count, 2);
        assert_eq!(down.contacts[0], touch(0, 1000, 1000, true, true));
        assert_eq!(down.contacts[1], touch(1, 3000, 3000, true, true));
    }

    #[test]
    fn hardware_ids_handle_explicit_and_implicit_liftoffs_without_duplicates() {
        let mut tracker = ContactTracker::new();

        // Pre-touch hover (TipSwitch=0) with no prior touch-down is ignored.
        assert_eq!(
            tracker.update_hardware_ids(&[touch(7, 1200, 1500, false, true)]),
            TrackedContacts::EMPTY
        );

        // Two hardware contacts (IDs 7 and 12) touch down.
        let f1 = tracker.update_hardware_ids(&[
            touch(7, 1200, 1500, true, true),
            touch(12, 4000, 2500, true, true),
        ]);
        assert_eq!(f1.count, 2);
        assert_eq!(f1.contacts[0], touch(7, 1200, 1500, true, true));
        assert_eq!(f1.contacts[1], touch(12, 4000, 2500, true, true));

        // ID 7 sends an explicit TipSwitch=0 liftoff at (1210, 1505); ID 12 continues.
        let f2 = tracker.update_hardware_ids(&[
            touch(7, 1210, 1505, false, true),
            touch(12, 4020, 2510, true, true),
        ]);
        assert_eq!(f2.count, 2);
        assert_eq!(f2.contacts[0], touch(7, 1210, 1505, false, true));
        assert_eq!(f2.contacts[1], touch(12, 4020, 2510, true, true));

        // Trailing TipSwitch=0 for ID 7 is ignored; ID 12 vanishes -> implicit liftoff.
        let f3 = tracker.update_hardware_ids(&[touch(7, 1210, 1505, false, true)]);
        assert_eq!(f3.count, 1);
        assert_eq!(f3.contacts[0], touch(12, 4020, 2510, false, true));

        // Next empty packet emits nothing.
        assert_eq!(tracker.update_hardware_ids(&[]), TrackedContacts::EMPTY);
    }

    #[test]
    fn reset_clears_active_state_without_liftoff() {
        let mut tracker = ContactTracker::new();
        let down = tracker.update_tracked(&[touch(0, 1000, 1000, true, true)]);
        assert_eq!(down.count, 1);
        assert!(tracker.has_active_contacts());
        tracker.reset();
        assert!(!tracker.has_active_contacts());
        assert_eq!(tracker.update_tracked(&[]), TrackedContacts::EMPTY);
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
}
