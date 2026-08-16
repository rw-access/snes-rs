//! Rendering and headless test-runner boundaries.

pub mod headless;
pub mod visual;

pub use headless::{HeadlessRunner, RomRunResult, RomTestDescriptor};
pub use visual::{compare_frame, PixelMismatch};
