extern crate std;
use std::vec;
use std::vec::Vec;

use super::super::PADDED_PAYLOAD_LEN;
use super::*;

/// The Magic Trackpad 2's SDP report descriptor (135 bytes): input 0x02 and 0x90, feature 0x55.
const MT2_DESCRIPTOR: [u8; 135] = [
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x85, 0x02, 0x05, 0x09, 0x19, 0x01, 0x29, 0x02, 0x15, 0x00,
    0x25, 0x01, 0x95, 0x02, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x06, 0x81, 0x03, 0x05, 0x01,
    0x09, 0x01, 0xa1, 0x00, 0x16, 0x81, 0xff, 0x26, 0x7f, 0x00, 0x36, 0xc3, 0xfe, 0x46, 0x3d, 0x01,
    0x65, 0x13, 0x55, 0x0d, 0x09, 0x30, 0x09, 0x31, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06, 0x75, 0x08,
    0x95, 0x04, 0x81, 0x01, 0xc0, 0x06, 0x02, 0xff, 0x09, 0x55, 0x85, 0x55, 0x15, 0x00, 0x26, 0xff,
    0x00, 0x75, 0x08, 0x95, 0x40, 0xb1, 0xa2, 0xc0, 0x06, 0x00, 0xff, 0x09, 0x14, 0xa1, 0x01, 0x85,
    0x90, 0x05, 0x84, 0x75, 0x01, 0x95, 0x03, 0x15, 0x00, 0x25, 0x01, 0x09, 0x61, 0x05, 0x85, 0x09,
    0x44, 0x09, 0x46, 0x81, 0x02, 0x95, 0x05, 0x81, 0x01, 0x75, 0x08, 0x95, 0x01, 0x15, 0x00, 0x26,
    0xff, 0x00, 0x09, 0x65, 0x81, 0x02, 0xc0,
];

fn seq(body: &[u8]) -> Vec<u8> {
    var(SDP_SEQUENCE, body)
}

/// A variable element with the narrowest length field.
fn var(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    if body.len() < 0x100 {
        v.push((kind << 3) | 5);
        v.push(body.len() as u8);
    } else {
        v.push((kind << 3) | 6);
        v.extend_from_slice(&(body.len() as u16).to_be_bytes());
    }
    v.extend_from_slice(body);
    v
}

fn attr_id(id: u16) -> Vec<u8> {
    let mut v = vec![0x09];
    v.extend_from_slice(&id.to_be_bytes());
    v
}

/// An attribute list like the one `HidBth` reads: a few attributes around 0x0206.
fn attribute_list(descriptor: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(attr_id(0x0000));
    body.extend([0x0A, 0x00, 0x01, 0x00, 0x00]); // uint32
    body.extend(attr_id(0x0205));
    body.extend([0x28, 0x01]); // bool
    body.extend(attr_id(0x0206));
    let mut class_descriptor = vec![0x08, CLASS_DESCRIPTOR_REPORT];
    class_descriptor.extend(var(SDP_TEXT, descriptor));
    body.extend(seq(&seq(&class_descriptor)));
    body.extend(attr_id(0x0207));
    body.extend(seq(&seq(&[0x09, 0x04, 0x09, 0x09, 0x01, 0x00])));
    seq(&body)
}

fn augment(input: &[u8]) -> Result<Vec<u8>, SdpAugmentError> {
    let mut out = vec![0_u8; input.len() + 64];
    let n = augment_sdp_attribute_list(input, &mut out)?;
    out.truncate(n);
    Ok(out)
}

fn descriptor_of(list: &[u8]) -> Vec<u8> {
    let path = find_report_descriptor(list).unwrap();
    let text = *path.elements().last().unwrap();
    list[text.body_start()..text.end()].to_vec()
}

#[test]
fn appends_the_tlc_and_keeps_every_other_attribute() {
    let input = attribute_list(&MT2_DESCRIPTOR);
    let out = augment(&input).unwrap();
    let mut expected_descriptor = MT2_DESCRIPTOR.to_vec();
    expected_descriptor.extend(VENDOR_TLC);
    assert_eq!(out, attribute_list(&expected_descriptor));
    assert_eq!(out.len(), input.len() + VENDOR_TLC.len());
    assert_eq!(descriptor_of(&out), expected_descriptor);
    assert!(descriptor_declares_report(
        &expected_descriptor,
        MULTITOUCH_REPORT_ID
    ));
    assert!(descriptor_declares_report(
        &expected_descriptor,
        MODE_SWITCH_REPORT_ID
    ));
    assert!(!descriptor_declares_report(
        &MT2_DESCRIPTOR,
        MULTITOUCH_REPORT_ID
    ));
    assert!(descriptor_declares_report(&MT2_DESCRIPTOR, 0x90));
}

#[test]
fn widens_a_length_field_that_overflows() {
    // A descriptor of 230 bytes: 230 + 39 > 255 widens the text and both inner sequences.
    let mut descriptor = vec![0x05, 0x01];
    descriptor.resize(229, 0x00); // filler items (0x00: a zero-size item)
    descriptor.push(0xC0);
    let input = attribute_list(&descriptor);
    let out = augment(&input).unwrap();
    let mut expected_descriptor = descriptor.clone();
    expected_descriptor.extend(VENDOR_TLC);
    assert_eq!(out, attribute_list(&expected_descriptor));
    assert_eq!(descriptor_of(&out), expected_descriptor);
}

#[test]
fn handles_the_attribute_lists_of_a_search_response() {
    // A service search attribute response: a sequence of attribute lists, the HID one second.
    let mut other = attr_id(0x0000);
    other.extend([0x0A, 0x00, 0x01, 0x00, 0x01]);
    let mut lists = seq(&other);
    lists.extend(attribute_list(&MT2_DESCRIPTOR));
    let input = seq(&lists);
    let out = augment(&input).unwrap();

    let mut expected_descriptor = MT2_DESCRIPTOR.to_vec();
    expected_descriptor.extend(VENDOR_TLC);
    let mut expected_lists = seq(&other);
    expected_lists.extend(attribute_list(&expected_descriptor));
    assert_eq!(out, seq(&expected_lists));
    assert_eq!(descriptor_of(&out), expected_descriptor);

    // No HID record in the lists.
    assert_eq!(
        augment(&seq(&seq(&other))),
        Err(SdpAugmentError::NoReportDescriptor)
    );
    // An empty stream sequence.
    assert_eq!(augment(&seq(&[])), Err(SdpAugmentError::NoReportDescriptor));
}

#[test]
fn keeps_an_existing_wide_length_field() {
    // The attribute list's own length field is 2 bytes even though the body is short.
    let mut input = attribute_list(&MT2_DESCRIPTOR);
    let body = input.split_off(2);
    let mut wide = vec![(SDP_SEQUENCE << 3) | 6];
    wide.extend((body.len() as u16).to_be_bytes());
    wide.extend(&body);
    let out = augment(&wide).unwrap();
    assert_eq!(out[0], (SDP_SEQUENCE << 3) | 6);
    assert_eq!(
        u16::from_be_bytes([out[1], out[2]]) as usize,
        body.len() + VENDOR_TLC.len()
    );
}

#[test]
fn refuses_what_it_cannot_or_need_not_change() {
    let mut declared = MT2_DESCRIPTOR.to_vec();
    declared.extend(VENDOR_TLC);
    assert_eq!(
        augment(&attribute_list(&declared)),
        Err(SdpAugmentError::Unexpected)
    );

    let mut no_end = MT2_DESCRIPTOR.to_vec();
    no_end.pop();
    assert_eq!(
        augment(&attribute_list(&no_end)),
        Err(SdpAugmentError::Unexpected)
    );

    let mut body = attr_id(0x0000);
    body.extend([0x0A, 0, 1, 0, 0]);
    assert_eq!(
        augment(&seq(&body)),
        Err(SdpAugmentError::NoReportDescriptor)
    );

    let input = attribute_list(&MT2_DESCRIPTOR);
    for cut in [1, 2, 10, input.len() - 1] {
        assert_eq!(
            augment(&input[..cut]),
            Err(SdpAugmentError::Malformed),
            "cut {cut}"
        );
    }
    let mut trailing = input.clone();
    trailing.push(0);
    assert_eq!(augment(&trailing), Err(SdpAugmentError::Malformed));

    let mut small = vec![0_u8; input.len() + VENDOR_TLC.len() - 1];
    assert_eq!(
        augment_sdp_attribute_list(&input, &mut small),
        Err(SdpAugmentError::OutputTooSmall)
    );
}

#[test]
fn the_tlc_is_a_well_formed_collection() {
    assert_eq!(VENDOR_TLC.len(), 39);
    assert_eq!(VENDOR_TLC[VENDOR_TLC.len() - 1], 0xC0);
    assert_eq!(usize::from(TLC_INPUT_COUNT) + 1, PADDED_REPORT_LEN);
    assert_eq!(PADDED_REPORT_LEN, 140);
    assert_eq!(PADDED_PAYLOAD_LEN, 141);
}
