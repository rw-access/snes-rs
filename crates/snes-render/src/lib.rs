//! Rendering and headless test-runner boundaries.

pub mod acceptance;
pub mod conformance;
pub mod headless;
pub mod visual;

pub use acceptance::{
    sha256_hex, AcceptanceCase, AcceptanceError, AcceptanceManifest, AcceptanceRun,
    AcceptanceRunner, ArtifactPaths, CapturedFrame, CaseReport, CaseStatus, ReferenceFormat,
};
pub use conformance::{
    run_vector, BusCycle, ConformanceError, CpuVector, VectorFile, VectorMismatch, VectorReport,
    VectorState, VectorStatus,
};
pub use headless::{HeadlessRunner, RomRunResult, RomTestDescriptor};
pub use visual::{compare_frame, compare_frame_masked, pixel_within_tolerance, PixelMismatch};
