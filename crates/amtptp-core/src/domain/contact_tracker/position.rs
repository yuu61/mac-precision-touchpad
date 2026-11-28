//! Position-based contact matching for hardware without persistent per-finger IDs
//! (Wellspring TYPE1–4, T2, and SPI).

use core::cmp::Reverse;

use crate::domain::{Contact, MAX_CONTACTS};

use super::{ActiveSlot, TrackedContacts, allocate_slot, pack_slot_outputs};

/// Default maximum movement per frame (`3200` logical units) accepted by position matching
/// before a touch is treated as a distinct finger rather than the continuation of an old slot.
pub const DEFAULT_MAX_JUMP_UNITS: u32 = 3200;

/// Squared Euclidean distance limit corresponding to [`DEFAULT_MAX_JUMP_UNITS`].
pub const DEFAULT_MAX_DISTANCE_SQ: u64 =
    (DEFAULT_MAX_JUMP_UNITS as u64) * (DEFAULT_MAX_JUMP_UNITS as u64);

/// Updates contact state for hardware without persistent contact IDs (Wellspring TYPE1–4, T2,
/// and SPI).
///
/// Only candidates with `tip_switch == true` (up to [`MAX_CONTACTS`]) are treated as active
/// surface touches; candidates with `tip_switch == false` are ignored as hover/below threshold,
/// allowing previously active slots to emit a clean 1-frame liftoff at their last touch
/// coordinates. Assigned `contact_id`s are stable slot indices `0..=4`.
pub(super) fn update(
    slots: &mut [Option<ActiveSlot>; MAX_CONTACTS],
    max_distance_sq: u64,
    candidates: &[Contact],
) -> TrackedContacts {
    let mut touches = [Contact::EMPTY; MAX_CONTACTS];
    let mut touch_len = 0_usize;
    for candidate in candidates.iter().copied().filter(|c| c.tip_switch) {
        let Some(dst) = touches.get_mut(touch_len) else {
            break;
        };
        *dst = candidate;
        touch_len += 1;
    }
    let touches = touches.get(..touch_len).unwrap_or_default();

    let prior_slots = *slots;
    let matched = match_positions(&prior_slots, touches, max_distance_sq);
    let mut touch_used = [false; MAX_CONTACTS];
    let mut slot_output = [None; MAX_CONTACTS];

    for ((slot_opt, (prior_opt, matched_t)), out_opt) in slots
        .iter_mut()
        .zip(prior_slots.iter().zip(matched.iter().copied()))
        .zip(slot_output.iter_mut())
    {
        let Some(prior) = *prior_opt else {
            continue;
        };
        if let Some(t_idx) = matched_t.map(usize::from)
            && let Some(&obs) = touches.get(t_idx)
        {
            if let Some(used) = touch_used.get_mut(t_idx) {
                *used = true;
            }
            let updated = prior.continued(obs);
            *slot_opt = Some(updated);
            *out_opt = Some(updated.into_contact(true));
        } else {
            *slot_opt = None;
            *out_opt = Some(prior.into_contact(false));
        }
    }

    for (&obs, &used) in touches.iter().zip(touch_used.iter()) {
        if !used {
            allocate_slot(slots, &prior_slots, &mut slot_output, obs, true);
        }
    }

    pack_slot_outputs(&slot_output)
}

/// Maximum-cardinality, minimum-squared-distance bipartite matching via 5-bit subset DP.
fn match_positions(
    slots: &[Option<ActiveSlot>; MAX_CONTACTS],
    touches: &[Contact],
    max_distance_sq: u64,
) -> [Option<u8>; MAX_CONTACTS] {
    type Entry = ((u8, Reverse<u64>), [Option<u8>; MAX_CONTACTS]);
    let mut dp: [Option<Entry>; 1 << MAX_CONTACTS] = [None; 1 << MAX_CONTACTS];
    if let Some(root) = dp.first_mut() {
        *root = Some(((0, Reverse(0)), [None; MAX_CONTACTS]));
    }

    let mask_limit = 1_usize << touches.len();
    for (s_idx, slot) in slots
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.map(|slot| (i, slot)))
    {
        for mask in (0..mask_limit).rev() {
            let Some(((matched, Reverse(cost)), assign)) = dp.get(mask).copied().flatten() else {
                continue;
            };
            for (t_idx, touch) in touches.iter().enumerate() {
                let bit = 1_usize << t_idx;
                let dist = distance_sq(slot.x, slot.y, touch.x, touch.y);
                if (mask & bit) != 0 || dist > max_distance_sq {
                    continue;
                }
                let next_key = (matched + 1, Reverse(cost + dist));
                if let Some(dst) = dp.get_mut(mask | bit)
                    && dst.is_none_or(|(prev_key, _)| next_key > prev_key)
                {
                    let mut next_assign = assign;
                    if let Some(slot_dst) = next_assign.get_mut(s_idx) {
                        *slot_dst = Some(t_idx as u8);
                    }
                    *dst = Some((next_key, next_assign));
                }
            }
        }
    }

    dp.iter()
        .take(mask_limit)
        .copied()
        .flatten()
        .fold(
            ((0_u8, Reverse(0_u64)), [None; MAX_CONTACTS]),
            |best, entry| if entry.0 > best.0 { entry } else { best },
        )
        .1
}

const fn distance_sq(x1: u16, y1: u16, x2: u16, y2: u16) -> u64 {
    let dx = (x1 as i32 - x2 as i32).unsigned_abs() as u64;
    let dy = (y1 as i32 - y2 as i32).unsigned_abs() as u64;
    dx * dx + dy * dy
}

#[cfg(test)]
mod tests {
    use crate::domain::Contact;
    use crate::domain::contact_tracker::{ContactMatching, ContactTracker, TrackedContacts, touch};

    fn step(tracker: &mut ContactTracker, candidates: &[Contact]) -> TrackedContacts {
        tracker.update(ContactMatching::Position, true, candidates)
    }

    #[test]
    fn tracked_two_finger_first_lifts_keeps_second_id_stable() {
        let mut tracker = ContactTracker::new();

        // Frame 1: two fingers touch down at (1000, 2000) and (5000, 3000).
        let f1 = step(
            &mut tracker,
            &[
                touch(0, 1000, 2000, true, true),
                touch(1, 5000, 3000, true, true),
            ],
        );
        assert_eq!(f1.count, 2);
        assert_eq!(f1.contacts[0], touch(0, 1000, 2000, true, true));
        assert_eq!(f1.contacts[1], touch(1, 5000, 3000, true, true));

        // Frame 2: finger 0 lifts; hardware shifts finger 1 into raw slot 0 at (5050, 3020).
        let f2 = step(&mut tracker, &[touch(0, 5050, 3020, true, true)]);
        assert_eq!(f2.count, 2);
        assert_eq!(f2.contacts[0], touch(0, 1000, 2000, false, true));
        assert_eq!(f2.contacts[1], touch(1, 5050, 3020, true, true));

        // Frame 3: finger 0 is gone; finger 1 still keeps ContactID = 1!
        let f3 = step(&mut tracker, &[touch(0, 5100, 3040, true, true)]);
        assert_eq!(f3.count, 1);
        assert_eq!(f3.contacts[0], touch(1, 5100, 3040, true, true));
        assert_eq!(f3.contacts[1], Contact::EMPTY);

        // Frame 4: finger 1 lifts (empty packet -> 1 liftoff report).
        let f4 = step(&mut tracker, &[]);
        assert_eq!(f4.count, 1);
        assert_eq!(f4.contacts[0], touch(1, 5100, 3040, false, true));

        // Frame 5: another empty packet -> ContactCount = 0.
        let f5 = step(&mut tracker, &[]);
        assert_eq!(f5, TrackedContacts::EMPTY);
    }

    #[test]
    fn tracked_rejects_cross_pad_jump_and_does_not_reuse_liftoff_id_same_frame() {
        let mut tracker = ContactTracker::new();

        let f1 = step(&mut tracker, &[touch(0, 500, 500, true, true)]);
        assert_eq!(f1.count, 1);
        assert_eq!(f1.contacts[0], touch(0, 500, 500, true, true));

        // Next frame: finger at (500, 500) lifts and a different finger touches at (9000, 6000).
        let f2 = step(&mut tracker, &[touch(0, 9000, 6000, true, true)]);
        assert_eq!(f2.count, 2);
        assert_eq!(f2.contacts[0], touch(0, 500, 500, false, true));
        assert_eq!(f2.contacts[1], touch(1, 9000, 6000, true, true));

        // Following frame: only ContactID = 1 remains active.
        let f3 = step(&mut tracker, &[touch(0, 9010, 6005, true, true)]);
        assert_eq!(f3.count, 1);
        assert_eq!(f3.contacts[0], touch(1, 9010, 6005, true, true));
    }

    #[test]
    fn tracked_latches_unintended_palm_confidence_through_liftoff() {
        let mut tracker = ContactTracker::new();

        // Starts normal, then expands into a palm, then shrinks before lifting.
        let down = step(&mut tracker, &[touch(0, 2000, 2000, true, true)]);
        assert_eq!(down.count, 1);
        let palm = step(&mut tracker, &[touch(0, 2010, 2010, true, false)]);
        assert_eq!(palm.contacts[0], touch(0, 2010, 2010, true, false));

        let still_latched = step(&mut tracker, &[touch(0, 2020, 2020, true, true)]);
        assert_eq!(still_latched.contacts[0], touch(0, 2020, 2020, true, false));

        let liftoff = step(&mut tracker, &[]);
        assert_eq!(liftoff.count, 1);
        assert_eq!(liftoff.contacts[0], touch(0, 2020, 2020, false, false));
    }

    #[test]
    fn tracked_ignores_initial_hover() {
        let mut tracker = ContactTracker::new();
        let hover = step(&mut tracker, &[touch(0, 1000, 1000, false, true)]);
        assert_eq!(hover, TrackedContacts::EMPTY);

        let down = step(
            &mut tracker,
            &[
                touch(0, 1000, 1000, true, true),
                touch(1, 2000, 2000, false, true),
                touch(2, 3000, 3000, true, true),
            ],
        );
        assert_eq!(down.count, 2);
        assert_eq!(down.contacts[0], touch(0, 1000, 1000, true, true));
        assert_eq!(down.contacts[1], touch(1, 3000, 3000, true, true));
    }

    #[test]
    fn tracked_bipartite_matching_avoids_greedy_steal_and_id_crossing() {
        // 1. Greedy would pair 6000 -> 5000 (dist 1000) and drop 3000 -> 8500 (dist 5500 > 3200).
        let mut tracker = ContactTracker::new();
        step(
            &mut tracker,
            &[
                touch(0, 3000, 2000, true, true),
                touch(1, 6000, 2000, true, true),
            ],
        );
        let f2 = step(
            &mut tracker,
            &[
                touch(0, 5000, 2000, true, true),
                touch(1, 8500, 2000, true, true),
            ],
        );
        assert_eq!(
            &f2.contacts[..usize::from(f2.count)],
            &[
                touch(0, 5000, 2000, true, true),
                touch(1, 8500, 2000, true, true)
            ]
        );

        // 2. Parallel swipe where all cross-distances are <= 3200 keeps order (min sum of d^2).
        let mut swipe = ContactTracker::new();
        step(
            &mut swipe,
            &[
                touch(0, 3000, 2000, true, true),
                touch(1, 4500, 2000, true, true),
            ],
        );
        let s2 = step(
            &mut swipe,
            &[
                touch(0, 5000, 2000, true, true),
                touch(1, 6100, 2000, true, true),
            ],
        );
        assert_eq!(
            &s2.contacts[..usize::from(s2.count)],
            &[
                touch(0, 5000, 2000, true, true),
                touch(1, 6100, 2000, true, true)
            ]
        );
    }
}
