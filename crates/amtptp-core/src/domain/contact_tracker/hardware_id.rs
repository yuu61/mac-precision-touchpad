//! Persistent hardware-ID contact tracking for Magic Trackpad 2 (USB and Bluetooth).

use crate::domain::{Contact, MAX_CONTACTS};

use super::{ActiveSlot, TrackedContacts, allocate_slot, pack_slot_outputs};

/// Updates contact state for hardware that supplies persistent per-finger `ContactID`s
/// (Magic Trackpad 2 over USB or Bluetooth).
pub(super) fn update(
    slots: &mut [Option<ActiveSlot>; MAX_CONTACTS],
    observations: &[Contact],
) -> TrackedContacts {
    let prior_slots = *slots;
    let mut slot_output = [None; MAX_CONTACTS];

    // 1. Continue or lift off each previously active slot using its first observation.
    for ((slot_opt, prior_opt), out_opt) in slots
        .iter_mut()
        .zip(prior_slots.iter())
        .zip(slot_output.iter_mut())
    {
        let Some(prior) = *prior_opt else {
            continue;
        };
        let first = observations
            .iter()
            .copied()
            .find(|obs| obs.contact_id == prior.contact_id);
        if let Some(obs) = first.filter(|obs| obs.tip_switch) {
            let updated = prior.continued(obs);
            *slot_opt = Some(updated);
            *out_opt = Some(updated.into_contact(true));
        } else {
            *slot_opt = None;
            *out_opt = Some(prior.into_contact(false));
        }
    }

    // 2. Allocate free slots for distinct new touch-downs not present in `prior_slots`.
    for obs in observations.iter().copied().filter(|obs| obs.tip_switch) {
        let in_prior = prior_slots
            .iter()
            .flatten()
            .any(|prior| prior.contact_id == obs.contact_id);
        let in_current = slots
            .iter()
            .flatten()
            .any(|active| active.contact_id == obs.contact_id);
        if !in_prior && !in_current {
            allocate_slot(slots, &prior_slots, &mut slot_output, obs, false);
        }
    }

    pack_slot_outputs(&slot_output)
}

#[cfg(test)]
mod tests {
    use crate::domain::Contact;
    use crate::domain::contact_tracker::{ContactMatching, ContactTracker, TrackedContacts, touch};

    fn step(tracker: &mut ContactTracker, observations: &[Contact]) -> TrackedContacts {
        tracker.update(ContactMatching::HardwareId, true, observations)
    }

    #[test]
    fn hardware_ids_handle_explicit_and_implicit_liftoffs_without_duplicates() {
        let mut tracker = ContactTracker::new();

        // Pre-touch hover (TipSwitch=0) with no prior touch-down is ignored.
        assert_eq!(
            step(&mut tracker, &[touch(7, 1200, 1500, false, true)]),
            TrackedContacts::EMPTY
        );

        // Two hardware contacts (IDs 7 and 12) touch down.
        let f1 = step(
            &mut tracker,
            &[
                touch(7, 1200, 1500, true, true),
                touch(12, 4000, 2500, true, true),
            ],
        );
        assert_eq!(f1.count, 2);
        assert_eq!(f1.contacts[0], touch(7, 1200, 1500, true, true));
        assert_eq!(f1.contacts[1], touch(12, 4000, 2500, true, true));

        // ID 7 sends an explicit TipSwitch=0 liftoff at (1210, 1505); liftoff must report the
        // last reported touch coordinates (1200, 1500) while ID 12 continues.
        let f2 = step(
            &mut tracker,
            &[
                touch(7, 1210, 1505, false, true),
                touch(12, 4020, 2510, true, true),
            ],
        );
        assert_eq!(f2.count, 2);
        assert_eq!(f2.contacts[0], touch(7, 1200, 1500, false, true));
        assert_eq!(f2.contacts[1], touch(12, 4020, 2510, true, true));

        // Trailing TipSwitch=0 for ID 7 is ignored; ID 12 vanishes -> implicit liftoff.
        let f3 = step(&mut tracker, &[touch(7, 1210, 1505, false, true)]);
        assert_eq!(f3.count, 1);
        assert_eq!(f3.contacts[0], touch(12, 4020, 2510, false, true));

        // Next empty packet emits nothing.
        assert_eq!(step(&mut tracker, &[]), TrackedContacts::EMPTY);
    }

    #[test]
    fn hardware_ids_deduplicate_same_frame_liftoff_and_touch_and_keep_last_xy() {
        let mut tracker = ContactTracker::new();
        step(&mut tracker, &[touch(7, 1200, 1500, true, true)]);

        // Explicit liftoff + new touch for ID 7 in one frame -> single liftoff at (1200, 1500).
        let f2 = step(
            &mut tracker,
            &[
                touch(7, 1210, 1505, false, true),
                touch(7, 1300, 1600, true, true),
            ],
        );
        assert_eq!(
            &f2.contacts[..usize::from(f2.count)],
            &[touch(7, 1200, 1500, false, true)]
        );

        // Next frame re-touches cleanly; [tip=true, tip=false] in one frame also emits one entry.
        step(&mut tracker, &[touch(7, 1310, 1610, true, true)]);
        let f4 = step(
            &mut tracker,
            &[
                touch(7, 1350, 1650, true, true),
                touch(7, 1360, 1660, false, true),
            ],
        );
        assert_eq!(
            &f4.contacts[..usize::from(f4.count)],
            &[touch(7, 1350, 1650, true, true)]
        );
    }
}
