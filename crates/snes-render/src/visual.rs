//! Deterministic headless frame comparison helpers.

use snes_core::FrameOutput;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelMismatch {
    pub index: usize,
    pub expected: u32,
    pub actual: u32,
}

/// Compares a frame while ignoring pixels whose corresponding mask entry is
/// true. The mask is row-major and must contain one entry per expected pixel.
/// An empty mask is the explicit "compare every pixel" form.
pub fn compare_frame_masked(
    frame: &FrameOutput<'_>,
    expected_width: u16,
    expected_height: u16,
    expected: &[u32],
    mask: &[bool],
    channel_tolerance: u8,
) -> Result<(), PixelMismatch> {
    if frame.width != expected_width
        || frame.height != expected_height
        || frame.pixels.len() != expected.len()
        || (!mask.is_empty() && mask.len() != expected.len())
    {
        return Err(PixelMismatch {
            index: usize::MAX,
            expected: expected.len() as u32,
            actual: frame.pixels.len() as u32,
        });
    }
    for (index, (&actual, &wanted)) in frame.pixels.iter().zip(expected).enumerate() {
        if !mask.is_empty() && mask[index] {
            continue;
        }
        if !within_tolerance(actual, wanted, channel_tolerance) {
            return Err(PixelMismatch {
                index,
                expected: wanted,
                actual,
            });
        }
    }
    Ok(())
}

/// Tests a single packed `0xAARRGGBB` pixel using the same policy as the
/// frame comparator. This is useful when producing a complete diff artifact
/// after the comparator has reported a mismatch.
pub fn pixel_within_tolerance(actual: u32, expected: u32, tolerance: u8) -> bool {
    within_tolerance(actual, expected, tolerance)
}

pub fn compare_frame(
    frame: &FrameOutput<'_>,
    expected_width: u16,
    expected_height: u16,
    expected: &[u32],
    channel_tolerance: u8,
) -> Result<(), PixelMismatch> {
    compare_frame_masked(
        frame,
        expected_width,
        expected_height,
        expected,
        &[],
        channel_tolerance,
    )
}

fn within_tolerance(actual: u32, expected: u32, tolerance: u8) -> bool {
    [24, 16, 8, 0].into_iter().all(|shift| {
        let actual = ((actual >> shift) & 0xff) as i16;
        let expected = ((expected >> shift) & 0xff) as i16;
        (actual - expected).unsigned_abs() <= u16::from(tolerance)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparator_reports_tolerated_and_non_tolerated_pixels() {
        let pixels = [0xff10_2030];
        let frame = FrameOutput {
            pixels: &pixels,
            width: 1,
            height: 1,
        };
        assert!(compare_frame(&frame, 1, 1, &[0xff11_2030], 1).is_ok());
        assert!(compare_frame(&frame, 1, 1, &[0xff12_2030], 1).is_err());
    }

    #[test]
    fn comparator_applies_an_explicit_mask() {
        let pixels = [0xff00_0000, 0xff10_2030];
        let frame = FrameOutput {
            pixels: &pixels,
            width: 2,
            height: 1,
        };
        assert!(
            compare_frame_masked(&frame, 2, 1, &[0xffff_ffff, 0xff10_2030], &[true, false], 0,)
                .is_ok()
        );
        assert!(compare_frame_masked(
            &frame,
            2,
            1,
            &[0xffff_ffff, 0xff10_2030],
            &[false, false],
            0,
        )
        .is_err());
    }
}
