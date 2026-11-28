use super::*;
use crate::infrastructure::bcm5974::config::MAGIC_TRACKPAD_2;

#[test]
fn read_events_back_off_and_reset_after_success() {
    let state = DeviceState::new();
    state.enter_d0();
    let failed = LowerReadClass::Failed { status: -1 };
    for _ in 0..MAX_IMMEDIATE_READ_RETRIES {
        assert_eq!(state.on_read(failed), ReadAction::Retry);
    }
    assert_eq!(state.on_read(failed), ReadAction::ArmTimer);
    assert_eq!(
        state.on_read(LowerReadClass::OtherReport { report_id: 1 }),
        ReadAction::Retry
    );
    assert!(state.take_mode_switch_request());
    assert_eq!(state.on_read(failed), ReadAction::Retry);
    assert_eq!(
        state.on_read(LowerReadClass::Report { length: 13 }),
        ReadAction::Deliver(13)
    );
}

#[test]
fn state_round_trip() {
    let s = DeviceSnapshot {
        vendor_id: 0x4C,
        product_id: 0x265,
        input_header_size: 4,
        x: MAGIC_TRACKPAD_2.x,
        qpc_frequency: -5,
        ptp_report_touch: true,
        ..DeviceSnapshot::default()
    };
    assert_eq!(DeviceState::with(&s).snapshot(), s);
    assert_eq!(
        DeviceState::new().snapshot(),
        DeviceSnapshot {
            ptp_input_on: true,
            ptp_report_touch: true,
            ptp_report_button: true,
            ..DeviceSnapshot::default()
        }
    );
}
