//! The contract between the Bluetooth lower filter (`AmtPtpBthFilter`, below `HidBth`) and the
//! collection filter (`AmtPtpHidFilter`).
//!
//! The Magic Trackpad 2 sends its multitouch report `0x31` and accepts the mode-switch feature
//! `0xF1` without declaring either in its SDP HID descriptor. HIDCLASS drops undeclared reports, so
//! the original design read `HidBth` directly (the detour), competing with HIDCLASS for every packet.
//!
//! The lower filter instead declares both reports, so HIDCLASS itself delivers them:
//!
//! 1. [`augment_sdp_attribute_list`] appends [`VENDOR_TLC`] (a vendor top-level collection with input
//!    `0x31` and feature `0xF1`) to the report descriptor in the SDP attribute list that `HidBth`
//!    reads at start. It becomes the device's last collection (`Col03`); `Col01` and `Col02` keep
//!    their numbers.
//! 2. HIDCLASS walks a read in steps of the declared report length and drops a shorter remainder, so
//!    a variable-length report cannot be declared as it is. [`pad_interrupt_payload`] pads every
//!    `0x31` interrupt payload to [`PADDED_REPORT_LEN`] and stores the original length in the last
//!    byte; [`unpad_report`] reverses it on the reader's side.
//!
//! Everything here is pure; the drivers only move bytes.

/// Report ID of the multitouch input report (Bluetooth).
pub const MULTITOUCH_REPORT_ID: u8 = 0x31;
/// Report ID of the mode-switch feature report (Bluetooth): `F1 02 01`.
pub const MODE_SWITCH_REPORT_ID: u8 = 0xF1;
/// Largest raw `0x31` report accepted, ID included: a 4-byte header and 15 nine-byte fingers (the
/// limit Linux `hid-magicmouse` enforces).
pub const RAW_REPORT_MAX_LEN: usize = 4 + 9 * 15;
/// Length of a padded `0x31` report, ID included: the raw report, zeros, then one length byte.
pub const PADDED_REPORT_LEN: usize = RAW_REPORT_MAX_LEN + 1;
/// HIDP header of an input report on the interrupt channel (`DATA | Input`).
pub const HIDP_DATA_INPUT: u8 = 0xA1;
/// Length of a padded interrupt payload: the HIDP header and the padded report. `HidBth` reads each
/// interrupt transfer into a buffer of the upper read's size plus one, so a HIDCLASS read of
/// [`PADDED_REPORT_LEN`] bytes gives exactly this capacity.
pub const PADDED_PAYLOAD_LEN: usize = 1 + PADDED_REPORT_LEN;

/// Data bytes of the declared `0x31` report (after the ID).
const TLC_INPUT_COUNT: u8 = (PADDED_REPORT_LEN - 1) as u8;
const _: () = assert!(PADDED_REPORT_LEN - 1 <= u8::MAX as usize);

/// The vendor top-level collection appended to the device's report descriptor.
///
/// It resets the globals the earlier collections leave set (physical range, unit, exponent), so
/// the `0x31` and `0xF1` fields are plain bytes.
pub const VENDOR_TLC: [u8; 39] = [
    0x06,
    0x01,
    0xFF, // Usage Page (Vendor 0xFF01)
    0x09,
    0x31, // Usage (0x31)
    0xA1,
    0x01, // Collection (Application)
    0x85,
    MULTITOUCH_REPORT_ID, //   Report ID (0x31)
    0x15,
    0x00, //   Logical Minimum (0)
    0x26,
    0xFF,
    0x00, //   Logical Maximum (255)
    0x35,
    0x00, //   Physical Minimum (0)
    0x45,
    0x00, //   Physical Maximum (0)
    0x65,
    0x00, //   Unit (None)
    0x55,
    0x00, //   Unit Exponent (0)
    0x75,
    0x08, //   Report Size (8)
    0x95,
    TLC_INPUT_COUNT, //   Report Count (139)
    0x09,
    0x01, //   Usage (0x01)
    0x81,
    0x02, //   Input (Data, Var, Abs)
    0x85,
    MODE_SWITCH_REPORT_ID, //   Report ID (0xF1)
    0x95,
    0x02, //   Report Count (2)
    0x09,
    0x02, //   Usage (0x02)
    0xB1,
    0x02, //   Feature (Data, Var, Abs)
    0xC0, // End Collection
];

/// SDP attribute `HIDDescriptorList`.
const ATTR_HID_DESCRIPTOR_LIST: u16 = 0x0206;
/// Class descriptor type of a report descriptor in `HIDDescriptorList`.
const CLASS_DESCRIPTOR_REPORT: u8 = 0x22;

/// SDP data element types (Bluetooth Core, Vol 3 Part B, 3.2).
const SDP_NIL: u8 = 0;
const SDP_UINT: u8 = 1;
const SDP_INT: u8 = 2;
const SDP_UUID: u8 = 3;
const SDP_TEXT: u8 = 4;
const SDP_BOOL: u8 = 5;
const SDP_SEQUENCE: u8 = 6;
const SDP_ALTERNATIVE: u8 = 7;
const SDP_URL: u8 = 8;

/// Why [`augment_sdp_attribute_list`] left the attribute list alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdpAugmentError {
    /// The stream is not a well-formed SDP attribute list.
    Malformed,
    /// No `HIDDescriptorList` with a report descriptor.
    NoReportDescriptor,
    /// The report descriptor already declares `0x31` or `0xF1` (nothing to add), or does not end
    /// with an End Collection item.
    Unexpected,
    /// The output buffer is too small.
    OutputTooSmall,
}

/// A parsed SDP data element: where its header and body are.
#[derive(Clone, Copy, Debug)]
struct Element {
    /// Offset of the header byte.
    start: usize,
    /// Element type (bits 7..3 of the header byte).
    kind: u8,
    /// Size index (bits 2..0 of the header byte).
    size_index: u8,
    /// Header length: the type byte plus any length field.
    header_len: usize,
    /// Body length.
    body_len: usize,
}

impl Element {
    const fn body_start(&self) -> usize {
        self.start + self.header_len
    }
    const fn end(&self) -> usize {
        self.start + self.header_len + self.body_len
    }
    const fn is_variable(&self) -> bool {
        self.size_index >= 5
    }
}

/// Parses the element at `start`, which must end within `buf`.
fn element(buf: &[u8], start: usize) -> Result<Element, SdpAugmentError> {
    let &header = buf.get(start).ok_or(SdpAugmentError::Malformed)?;
    let kind = header >> 3;
    let size_index = header & 7;
    let (header_len, body_len) = match (kind, size_index) {
        (SDP_NIL, 0) => (1, 0),
        (SDP_UINT | SDP_INT | SDP_UUID, 0..=4) | (SDP_BOOL, 0) => (1, 1_usize << size_index),
        (SDP_TEXT | SDP_SEQUENCE | SDP_ALTERNATIVE | SDP_URL, 5..=7) => {
            let width = 1_usize << (size_index - 5);
            let field = buf
                .get(start + 1..start + 1 + width)
                .ok_or(SdpAugmentError::Malformed)?;
            let len = field
                .iter()
                .fold(0_usize, |acc, &b| (acc << 8) | usize::from(b));
            (1 + width, len)
        }
        _ => return Err(SdpAugmentError::Malformed),
    };
    let e = Element {
        start,
        kind,
        size_index,
        header_len,
        body_len,
    };
    if e.end() > buf.len() {
        return Err(SdpAugmentError::Malformed);
    }
    Ok(e)
}

/// The children of a sequence element, one at a time.
struct Children<'a> {
    buf: &'a [u8],
    next: usize,
    end: usize,
}

impl<'a> Children<'a> {
    const fn of(buf: &'a [u8], parent: Element) -> Result<Self, SdpAugmentError> {
        if parent.kind != SDP_SEQUENCE && parent.kind != SDP_ALTERNATIVE {
            return Err(SdpAugmentError::Malformed);
        }
        Ok(Self {
            buf,
            next: parent.body_start(),
            end: parent.end(),
        })
    }
}

impl Iterator for Children<'_> {
    type Item = Result<Element, SdpAugmentError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let e = element(self.buf, self.next);
        match e {
            Ok(e) if e.end() <= self.end => {
                self.next = e.end();
                Some(Ok(e))
            }
            Ok(_) | Err(_) => {
                self.next = self.end;
                Some(Err(SdpAugmentError::Malformed))
            }
        }
    }
}

/// The unsigned value of a `uint` element.
fn uint_value(buf: &[u8], e: Element) -> Option<u64> {
    if e.kind != SDP_UINT || e.body_len > 8 {
        return None;
    }
    let body = buf.get(e.body_start()..e.end())?;
    Some(body.iter().fold(0_u64, |acc, &b| (acc << 8) | u64::from(b)))
}

/// Whether a HID report descriptor declares report ID `id` (a `Report ID` global item).
#[must_use]
pub fn descriptor_declares_report(descriptor: &[u8], id: u8) -> bool {
    let mut i = 0;
    while let Some(&prefix) = descriptor.get(i) {
        if prefix == 0xFE {
            // Long item: bDataSize, bLongItemTag, data.
            let Some(&size) = descriptor.get(i + 1) else {
                return false;
            };
            i += 3 + usize::from(size);
            continue;
        }
        let size = match prefix & 3 {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 4,
        };
        if prefix & 0xFC == 0x84 && size >= 1 && descriptor.get(i + 1) == Some(&id) {
            return true;
        }
        i += 1 + size;
    }
    false
}

/// Most elements between the stream and the report descriptor text: the attribute lists, one
/// attribute list, the `HIDDescriptorList` value, a class-descriptor sequence and the text.
const MAX_DEPTH: usize = 5;

/// The elements from the outermost sequence down to the report descriptor text element,
/// outermost first: `chain[..depth]`.
struct Path {
    chain: [Element; MAX_DEPTH],
    depth: usize,
}

impl Path {
    fn elements(&self) -> &[Element] {
        self.chain.get(..self.depth).unwrap_or(&[])
    }
}

/// Searches one attribute list (a sequence of attribute ID / value pairs) for the report
/// descriptor; returns the `HIDDescriptorList` value, the class-descriptor sequence and the text.
fn find_in_attribute_list(
    buf: &[u8],
    list: Element,
) -> Result<Option<[Element; 3]>, SdpAugmentError> {
    let mut children = Children::of(buf, list)?;
    while let Some(id) = children.next() {
        let id = id?;
        let value = children.next().ok_or(SdpAugmentError::Malformed)??;
        if uint_value(buf, id) != Some(u64::from(ATTR_HID_DESCRIPTOR_LIST)) {
            continue;
        }
        for class_descriptor in Children::of(buf, value)? {
            let class_descriptor = class_descriptor?;
            let mut fields = Children::of(buf, class_descriptor)?;
            let (Some(Ok(kind)), Some(Ok(text))) = (fields.next(), fields.next()) else {
                continue;
            };
            if uint_value(buf, kind) == Some(u64::from(CLASS_DESCRIPTOR_REPORT))
                && text.kind == SDP_TEXT
                && text.is_variable()
            {
                return Ok(Some([value, class_descriptor, text]));
            }
        }
        return Ok(None);
    }
    Ok(None)
}

/// Finds the report descriptor in either form of the stream: one attribute list (a cached record),
/// or the `AttributeLists` of a service search attribute response (a sequence of attribute lists,
/// one per matching record).
fn find_report_descriptor(buf: &[u8]) -> Result<Path, SdpAugmentError> {
    let top = element(buf, 0)?;
    if top.kind != SDP_SEQUENCE || top.end() != buf.len() {
        return Err(SdpAugmentError::Malformed);
    }
    let first = Children::of(buf, top)?.next().transpose()?;
    let nested = first.is_some_and(|e| e.kind == SDP_SEQUENCE);
    if !nested {
        return match find_in_attribute_list(buf, top)? {
            Some([value, class_descriptor, text]) => Ok(Path {
                chain: [top, value, class_descriptor, text, text],
                depth: 4,
            }),
            None => Err(SdpAugmentError::NoReportDescriptor),
        };
    }
    for list in Children::of(buf, top)? {
        let list = list?;
        if list.kind != SDP_SEQUENCE {
            return Err(SdpAugmentError::Malformed);
        }
        if let Some([value, class_descriptor, text]) = find_in_attribute_list(buf, list)? {
            return Ok(Path {
                chain: [top, list, value, class_descriptor, text],
                depth: 5,
            });
        }
    }
    Err(SdpAugmentError::NoReportDescriptor)
}

/// Length-field width for a variable element of `body_len`, at least the original one.
fn length_width(original_size_index: u8, body_len: usize) -> Option<(u8, usize)> {
    (original_size_index..=7).find_map(|index| {
        let width = 1_usize << (index - 5);
        (body_len < (1_usize << (8 * width))).then_some((index, width))
    })
}

/// Writes a variable element header for `body_len` at `out[at..]`; returns the header length.
fn write_header(
    out: &mut [u8],
    at: usize,
    e: Element,
    body_len: usize,
) -> Result<usize, SdpAugmentError> {
    let (index, width) = length_width(e.size_index, body_len).ok_or(SdpAugmentError::Malformed)?;
    let header = out
        .get_mut(at..at + 1 + width)
        .ok_or(SdpAugmentError::OutputTooSmall)?;
    let (kind, field) = header.split_at_mut(1);
    if let Some(k) = kind.first_mut() {
        *k = (e.kind << 3) | index;
    }
    for (i, b) in field.iter_mut().enumerate() {
        let shift = 8 * (width - 1 - i);
        *b = u8::try_from((body_len >> shift) & 0xFF)
            .ok()
            .ok_or(SdpAugmentError::Malformed)?;
    }
    Ok(1 + width)
}

fn copy_into(out: &mut [u8], at: usize, bytes: &[u8]) -> Result<usize, SdpAugmentError> {
    out.get_mut(at..at + bytes.len())
        .ok_or(SdpAugmentError::OutputTooSmall)?
        .copy_from_slice(bytes);
    Ok(bytes.len())
}

/// Appends [`VENDOR_TLC`] to the report descriptor of an SDP stream and writes the result to `out`.
///
/// The stream is either one attribute list (a cached record) or the `AttributeLists` of
/// `IOCTL_BTH_SDP_SERVICE_ATTRIBUTE_SEARCH` (a sequence of attribute lists).
///
/// Every element from the list down to the descriptor gets its new length; a length field only
/// widens when the new length needs it. Returns the length written.
///
/// # Errors
/// [`SdpAugmentError`]: the caller then passes the original attribute list on unchanged.
pub fn augment_sdp_attribute_list(input: &[u8], out: &mut [u8]) -> Result<usize, SdpAugmentError> {
    let path = find_report_descriptor(input)?;
    let chain = path.elements();
    let &text = chain.last().ok_or(SdpAugmentError::Malformed)?;
    let descriptor = input
        .get(text.body_start()..text.end())
        .ok_or(SdpAugmentError::Malformed)?;
    if descriptor.last() != Some(&0xC0)
        || descriptor_declares_report(descriptor, MULTITOUCH_REPORT_ID)
        || descriptor_declares_report(descriptor, MODE_SWITCH_REPORT_ID)
    {
        return Err(SdpAugmentError::Unexpected);
    }

    // New body lengths, innermost first; each parent's body grows by its child's growth.
    let mut body = [0_usize; MAX_DEPTH];
    let mut child_growth = VENDOR_TLC.len();
    for (depth, e) in chain.iter().enumerate().rev() {
        let new_body = e.body_len + child_growth;
        let (_, width) = length_width(e.size_index, new_body).ok_or(SdpAugmentError::Malformed)?;
        let slot = body.get_mut(depth).ok_or(SdpAugmentError::Malformed)?;
        *slot = new_body;
        child_growth = (1 + width + new_body) - (e.header_len + e.body_len);
    }

    // Emit: each chain element's header, the bytes before its chain child, the child (recursively,
    // here iteratively), then the bytes after it.
    let mut at = 0;
    for (depth, e) in chain.iter().enumerate() {
        let new_body = *body.get(depth).ok_or(SdpAugmentError::Malformed)?;
        at += write_header(out, at, *e, new_body)?;
        let before_end = match chain.get(depth + 1) {
            Some(child) => child.start,
            None => e.end(),
        };
        let before = input
            .get(e.body_start()..before_end)
            .ok_or(SdpAugmentError::Malformed)?;
        at += copy_into(out, at, before)?;
    }
    at += copy_into(out, at, &VENDOR_TLC)?;
    for (depth, e) in chain.iter().enumerate().rev() {
        let after_start = match chain.get(depth + 1) {
            Some(child) => child.end(),
            None => e.end(),
        };
        let after = input
            .get(after_start..e.end())
            .ok_or(SdpAugmentError::Malformed)?;
        at += copy_into(out, at, after)?;
    }
    Ok(at)
}

/// Report ID of the mouse-mode report (Bluetooth). The trackpad sends it until it is switched to
/// multitouch mode, for example after its power switch restarted it.
pub const MOUSE_REPORT_ID: u8 = 0x02;

/// What a padded report read from the vendor collection holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaddedReport<'a> {
    /// A raw `0x31` multitouch report.
    Multitouch(&'a [u8]),
    /// The trackpad sent a report of this ID instead of a multitouch report: it is not in
    /// multitouch mode (the reader sends the mode switch again).
    ModeLost(u8),
}

/// Pads an interrupt payload in place, so HIDCLASS delivers it on the vendor collection.
///
/// `payload` is the whole transfer buffer and `len` the bytes received: the HIDP header
/// [`HIDP_DATA_INPUT`], then the raw report.
///
/// - A `0x31` report of up to [`RAW_REPORT_MAX_LEN`] bytes keeps its bytes, gets zeros up to
///   [`PADDED_REPORT_LEN`] report bytes, and its length in the last byte.
/// - A mouse-mode report ([`MOUSE_REPORT_ID`]) becomes a mode-lost marker: report ID `0x31`, the
///   original ID in the next byte, zeros, and length 0 in the last byte.
///
/// Returns the new payload length, [`PADDED_PAYLOAD_LEN`]; `None` (buffer untouched) for any other
/// payload, a `0x31` report that is too long, or a buffer that is not exactly
/// [`PADDED_PAYLOAD_LEN`] long.
#[must_use]
pub fn pad_interrupt_payload(payload: &mut [u8], len: usize) -> Option<usize> {
    if payload.len() != PADDED_PAYLOAD_LEN || len > payload.len() || len < 2 {
        return None;
    }
    if payload.first() != Some(&HIDP_DATA_INPUT) {
        return None;
    }
    let raw_len = len - 1;
    let (trailer, body) = payload.split_last_mut()?;
    match body.get(1).copied()? {
        MULTITOUCH_REPORT_ID if raw_len <= RAW_REPORT_MAX_LEN => {
            body.get_mut(len..)?.fill(0);
            *trailer = u8::try_from(raw_len).ok()?;
        }
        MOUSE_REPORT_ID => {
            let marker = body.get_mut(1..)?;
            marker.fill(0);
            let [id, original, ..] = marker else {
                return None;
            };
            *id = MULTITOUCH_REPORT_ID;
            *original = MOUSE_REPORT_ID;
            *trailer = 0;
        }
        _ => return None,
    }
    Some(PADDED_PAYLOAD_LEN)
}

/// Decodes a padded report read from the vendor collection; `None` if `report` is not a
/// well-formed padded report.
#[must_use]
pub fn unpad_report(report: &[u8]) -> Option<PaddedReport<'_>> {
    if report.len() != PADDED_REPORT_LEN || report.first() != Some(&MULTITOUCH_REPORT_ID) {
        return None;
    }
    match usize::from(*report.last()?) {
        0 => match report.get(1).copied()? {
            MULTITOUCH_REPORT_ID => None,
            id => Some(PaddedReport::ModeLost(id)),
        },
        raw_len if raw_len <= RAW_REPORT_MAX_LEN => {
            report.get(..raw_len).map(PaddedReport::Multitouch)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    use super::*;

    /// The Magic Trackpad 2's SDP report descriptor (135 bytes): input 0x02 and 0x90, feature 0x55.
    const MT2_DESCRIPTOR: [u8; 135] = [
        0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x85, 0x02, 0x05, 0x09, 0x19, 0x01, 0x29, 0x02, 0x15,
        0x00, 0x25, 0x01, 0x95, 0x02, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x06, 0x81, 0x03,
        0x05, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x16, 0x81, 0xff, 0x26, 0x7f, 0x00, 0x36, 0xc3, 0xfe,
        0x46, 0x3d, 0x01, 0x65, 0x13, 0x55, 0x0d, 0x09, 0x30, 0x09, 0x31, 0x75, 0x08, 0x95, 0x02,
        0x81, 0x06, 0x75, 0x08, 0x95, 0x04, 0x81, 0x01, 0xc0, 0x06, 0x02, 0xff, 0x09, 0x55, 0x85,
        0x55, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40, 0xb1, 0xa2, 0xc0, 0x06, 0x00,
        0xff, 0x09, 0x14, 0xa1, 0x01, 0x85, 0x90, 0x05, 0x84, 0x75, 0x01, 0x95, 0x03, 0x15, 0x00,
        0x25, 0x01, 0x09, 0x61, 0x05, 0x85, 0x09, 0x44, 0x09, 0x46, 0x81, 0x02, 0x95, 0x05, 0x81,
        0x01, 0x75, 0x08, 0x95, 0x01, 0x15, 0x00, 0x26, 0xff, 0x00, 0x09, 0x65, 0x81, 0x02, 0xc0,
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

    fn payload_with(raw: &[u8]) -> ([u8; PADDED_PAYLOAD_LEN], usize) {
        let mut p = [0xEE_u8; PADDED_PAYLOAD_LEN];
        p[0] = HIDP_DATA_INPUT;
        p[1..=raw.len()].copy_from_slice(raw);
        (p, 1 + raw.len())
    }

    fn raw_report(fingers: usize) -> Vec<u8> {
        let mut r = vec![MULTITOUCH_REPORT_ID, 0x60, 0xE0, 0x5A];
        for f in 0..fingers {
            r.extend((0..9).map(|i| (f * 9 + i) as u8 ^ 0x5C));
        }
        r
    }

    #[test]
    fn pads_and_unpads_every_finger_count() {
        for fingers in 0..=15 {
            let raw = raw_report(fingers);
            let (mut payload, len) = payload_with(&raw);
            assert_eq!(
                pad_interrupt_payload(&mut payload, len),
                Some(PADDED_PAYLOAD_LEN)
            );
            let report = &payload[1..];
            assert_eq!(report.len(), PADDED_REPORT_LEN);
            assert!(
                report[raw.len()..PADDED_REPORT_LEN - 1]
                    .iter()
                    .all(|&b| b == 0)
            );
            assert_eq!(
                unpad_report(report),
                Some(PaddedReport::Multitouch(raw.as_slice())),
                "{fingers} fingers"
            );
        }
    }

    #[test]
    fn a_mouse_report_becomes_a_mode_lost_marker() {
        let (mut payload, len) = payload_with(&[MOUSE_REPORT_ID, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(
            pad_interrupt_payload(&mut payload, len),
            Some(PADDED_PAYLOAD_LEN)
        );
        let report = &payload[1..];
        assert!(report.len() > 2);
        assert_eq!(report[0], MULTITOUCH_REPORT_ID);
        assert_eq!(report[1], MOUSE_REPORT_ID);
        assert!(report[2..].iter().all(|&b| b == 0));
        assert_eq!(
            unpad_report(report),
            Some(PaddedReport::ModeLost(MOUSE_REPORT_ID))
        );
    }

    #[test]
    fn leaves_other_payloads_alone() {
        // Battery report 0x90 (Col02 delivers it), an undeclared report.
        for raw in [&[0x90_u8, 0x03, 0x5A][..], &[0x13, 0x00]] {
            let (mut payload, len) = payload_with(raw);
            let before = payload;
            assert_eq!(pad_interrupt_payload(&mut payload, len), None);
            assert_eq!(payload, before);
        }
        // Too long (16 fingers), a continuation fragment, an empty transfer, a wrong capacity.
        let (mut payload, _) = payload_with(&raw_report(15));
        assert_eq!(
            pad_interrupt_payload(&mut payload, PADDED_PAYLOAD_LEN),
            None
        );
        let (mut payload, len) = payload_with(&raw_report(1));
        payload[0] = 0x5C;
        assert_eq!(pad_interrupt_payload(&mut payload, len), None);
        let (mut payload, _) = payload_with(&raw_report(1));
        assert_eq!(pad_interrupt_payload(&mut payload, 0), None);
        assert_eq!(pad_interrupt_payload(&mut payload, 1), None);
        let mut short = [HIDP_DATA_INPUT, MULTITOUCH_REPORT_ID, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(pad_interrupt_payload(&mut short, 5), None);
        let mut long = [0_u8; 1025];
        long[..2].copy_from_slice(&[HIDP_DATA_INPUT, MULTITOUCH_REPORT_ID]);
        assert_eq!(pad_interrupt_payload(&mut long, 14), None);
    }

    #[test]
    fn rejects_malformed_padded_reports() {
        let (mut payload, len) = payload_with(&raw_report(2));
        pad_interrupt_payload(&mut payload, len).unwrap();
        let mut report = payload[1..].to_vec();
        assert!(unpad_report(&report[..PADDED_REPORT_LEN - 1]).is_none());
        report[PADDED_REPORT_LEN - 1] = (RAW_REPORT_MAX_LEN + 1) as u8;
        assert!(unpad_report(&report).is_none());
        // Length 0 with 0x31 in the ID slot is no marker.
        report[PADDED_REPORT_LEN - 1] = 0;
        report[1] = MULTITOUCH_REPORT_ID;
        assert!(unpad_report(&report).is_none());
        report[PADDED_REPORT_LEN - 1] = 4;
        report[0] = 0x90;
        assert!(unpad_report(&report).is_none());
    }

    #[test]
    fn the_tlc_is_a_well_formed_collection() {
        assert_eq!(VENDOR_TLC.len(), 39);
        assert_eq!(VENDOR_TLC[VENDOR_TLC.len() - 1], 0xC0);
        assert_eq!(usize::from(TLC_INPUT_COUNT) + 1, PADDED_REPORT_LEN);
        assert_eq!(PADDED_REPORT_LEN, 140);
        assert_eq!(PADDED_PAYLOAD_LEN, 141);
    }
}
