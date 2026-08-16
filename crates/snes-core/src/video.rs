//! Frontend-neutral video output types.

/// A frame produced by the core and consumed by a renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameOutput<'a> {
    pub pixels: &'a [u32],
    pub width: u16,
    pub height: u16,
}
