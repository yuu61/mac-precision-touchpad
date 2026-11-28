//! Use cases that turn decoded touchpad input into frames for reporting.

use crate::domain::{Contact, ContactMatching, ContactTracker, TrackedContacts};

/// The independently controlled surface and button reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reporting {
    /// Include surface contacts in the frame.
    pub surface: bool,
    /// Include the button state in the frame.
    pub button: bool,
}

/// Contacts and button state ready for the output adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputFrame {
    pub(crate) contacts: TrackedContacts,
    pub(crate) button: u8,
}

/// Applies the reporting settings to one set of contact observations.
///
/// The input adapter supplies the button value in its own representation. The value is
/// preserved when button reporting is enabled.
#[must_use]
pub fn report_input(
    tracker: &mut ContactTracker,
    matching: ContactMatching,
    observations: &[Contact],
    button: u8,
    reporting: Reporting,
) -> InputFrame {
    InputFrame {
        contacts: tracker.update(matching, reporting.surface, observations),
        button: if reporting.button { button } else { 0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_and_button_reporting_are_independent() {
        let mut tracker = ContactTracker::new();
        let touch = Contact {
            tip_switch: true,
            confidence: true,
            ..Contact::EMPTY
        };
        let surface = report_input(
            &mut tracker,
            ContactMatching::Position,
            &[touch],
            0x80,
            Reporting {
                surface: true,
                button: false,
            },
        );
        assert_eq!(surface.contacts.count(), 1);
        assert_eq!(surface.button, 0);

        let button = report_input(
            &mut tracker,
            ContactMatching::Position,
            &[touch],
            0x80,
            Reporting {
                surface: false,
                button: true,
            },
        );
        assert_eq!(button.contacts, TrackedContacts::EMPTY);
        assert_eq!(button.button, 0x80);
        assert!(!tracker.has_active_contacts());
    }
}
