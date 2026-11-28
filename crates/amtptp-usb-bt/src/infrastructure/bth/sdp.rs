//! Adds the vendor HID collection to the device's SDP attribute list.

use super::{MODE_SWITCH_REPORT_ID, MULTITOUCH_REPORT_ID, PADDED_REPORT_LEN};

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

#[cfg(test)]
mod tests;
