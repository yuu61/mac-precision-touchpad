/// Replace just the coordinate fields in a C-oracle report. The C decoder inverted
/// Y before discarding the X bits and did not clamp to the descriptor maxima.
pub fn correct_coordinates(report: &mut [u8], records: &[u8]) {
    let signed13 = |bits: u32| (bits & 0xfff) as i32 - (bits & 0x1000) as i32;
    let count = usize::from(report[48]);
    let (fingers, _) = records.as_chunks::<9>();
    assert!(count <= fingers.len());
    for (contact, finger) in report[1..46]
        .as_chunks_mut::<9>()
        .0
        .iter_mut()
        .take(count)
        .zip(fingers)
    {
        let coordinates = u32::from_le_bytes(finger[..4].try_into().unwrap());
        let x = (3678 + signed13(coordinates)).clamp(0, 7612) as u16;
        let y = (2479 - signed13(coordinates >> 13)).clamp(0, 5065) as u16;
        contact[5..7].copy_from_slice(&x.to_le_bytes());
        contact[7..9].copy_from_slice(&y.to_le_bytes());
    }
}
