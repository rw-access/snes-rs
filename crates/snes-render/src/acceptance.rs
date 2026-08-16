//! Optional, manifest-driven acceptance runs for the pinned ROM corpus.
//!
//! The image decoders are kept small and opt-in at the crate level. The
//! harness can consume lossless PPM/ARGB32 references as well as PNG/JPEG
//! captures while remaining usable in an offline checkout.

use std::{
    collections::BTreeMap,
    fmt, fs,
    io::Cursor,
    path::{Component, Path, PathBuf},
};

use snes_core::{Address24, Console, FrameOutput};

use crate::{
    headless::{HeadlessRunner, ResultObservation, ResultProtocol, ResultStatus},
    visual::{compare_frame_masked, pixel_within_tolerance, PixelMismatch},
};

const MANIFEST_VERSION: u64 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReferenceFormat {
    Ppm,
    Argb32,
    Png,
    Jpeg,
    Unsupported(String),
}

impl ReferenceFormat {
    fn parse(value: &str) -> Self {
        match value {
            "ppm" | "ppm-p6" => Self::Ppm,
            "argb32" | "raw-argb32" => Self::Argb32,
            "png" => Self::Png,
            "jpg" | "jpeg" => Self::Jpeg,
            other => Self::Unsupported(other.to_owned()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptanceCase {
    pub id: String,
    pub source_id: String,
    pub revision: String,
    pub rom_path: PathBuf,
    pub rom_sha256: String,
    pub enabled: bool,
    pub diagnostic_only: bool,
    pub max_frames: Option<u64>,
    pub max_master_ticks: Option<u64>,
    pub deterministic_replay: bool,
    /// Frame zero is the rendered framebuffer immediately after reset. Frame
    /// one is captured after one bounded frame advance, and so on.
    pub capture_frame: Option<u64>,
    pub reference_path: Option<PathBuf>,
    pub reference_sha256: Option<String>,
    pub reference_format: ReferenceFormat,
    pub result_protocol: Option<ResultProtocol>,
    pub width: u16,
    pub height: u16,
    pub channel_tolerance: u8,
    /// A one-byte-per-pixel mask. Nonzero bytes are ignored by comparison.
    pub mask_path: Option<PathBuf>,
    pub artifact_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptanceManifest {
    pub format_version: u64,
    pub corpus_root: PathBuf,
    pub artifact_root: PathBuf,
    pub cases: Vec<AcceptanceCase>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedFrame {
    pub frame_number: u64,
    pub master_ticks: u64,
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptanceRun {
    pub master_ticks: u64,
    pub stopped: bool,
    pub result: Option<ResultObservation>,
    pub captured_frame: Option<CapturedFrame>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaseStatus {
    Passed,
    Diagnostic,
    Failed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtifactPaths {
    pub metadata: Option<PathBuf>,
    pub actual: Option<PathBuf>,
    pub diff: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaseReport {
    pub case_id: String,
    pub status: CaseStatus,
    pub run: AcceptanceRun,
    pub message: Option<String>,
    pub mismatch: Option<PixelMismatch>,
    pub artifacts: ArtifactPaths,
}

#[derive(Debug)]
pub enum AcceptanceError {
    Io(String),
    Parse(String),
    InvalidCase(String),
    MissingFile(PathBuf),
    Checksum {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    UnsupportedReference(String),
    InvalidReference(String),
    Bound(String),
}

impl fmt::Display for AcceptanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(formatter, "I/O error: {message}"),
            Self::Parse(message) => write!(formatter, "manifest parse error: {message}"),
            Self::InvalidCase(message) => write!(formatter, "invalid acceptance case: {message}"),
            Self::MissingFile(path) => write!(formatter, "missing file: {}", path.display()),
            Self::Checksum {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "checksum mismatch for {}: expected {expected}, got {actual}",
                path.display()
            ),
            Self::UnsupportedReference(format) => {
                write!(
                    formatter,
                    "reference format is not supported by this build: {format}"
                )
            }
            Self::InvalidReference(message) => write!(formatter, "invalid reference: {message}"),
            Self::Bound(message) => write!(formatter, "execution bound error: {message}"),
        }
    }
}

impl std::error::Error for AcceptanceError {}

impl AcceptanceManifest {
    pub fn load_path(path: &Path) -> Result<Self, AcceptanceError> {
        let text = fs::read_to_string(path)
            .map_err(|error| AcceptanceError::Io(format!("{}: {error}", path.display())))?;
        Self::parse(&text)
    }

    /// Parses the intentionally small TOML-like descriptor format used by
    /// `tests/snes-test-roms/cases.toml`. It supports quoted strings, booleans,
    /// unsigned integers, root keys, and `[case.<id>]` sections.
    pub fn parse(text: &str) -> Result<Self, AcceptanceError> {
        let mut root = BTreeMap::new();
        let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut current: Option<String> = None;

        for (line_number, raw_line) in text.lines().enumerate() {
            let uncommented = strip_comment(raw_line);
            let line = uncommented.trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') {
                if !line.ends_with(']') || !line.starts_with("[case.") {
                    return Err(AcceptanceError::Parse(format!(
                        "line {}: expected [case.<id>] section",
                        line_number + 1
                    )));
                }
                let id = &line[6..line.len() - 1];
                if !valid_name(id) {
                    return Err(AcceptanceError::Parse(format!(
                        "line {}: invalid case id {id:?}",
                        line_number + 1
                    )));
                }
                if sections.contains_key(id) {
                    return Err(AcceptanceError::Parse(format!(
                        "line {}: duplicate case {id}",
                        line_number + 1
                    )));
                }
                sections.insert(id.to_owned(), BTreeMap::new());
                current = Some(id.to_owned());
                continue;
            }

            let Some((key, value)) = line.split_once('=') else {
                return Err(AcceptanceError::Parse(format!(
                    "line {}: expected key = value",
                    line_number + 1
                )));
            };
            let key = key.trim();
            if !valid_name(key) {
                return Err(AcceptanceError::Parse(format!(
                    "line {}: invalid key {key:?}",
                    line_number + 1
                )));
            }
            let value = value.trim().to_owned();
            let target = match current.as_ref() {
                Some(id) => sections.get_mut(id).expect("current section exists"),
                None => &mut root,
            };
            if target.insert(key.to_owned(), value).is_some() {
                return Err(AcceptanceError::Parse(format!(
                    "line {}: duplicate key {key}",
                    line_number + 1
                )));
            }
        }

        let format_version = parse_u64(root.get("format_version"), "format_version")?;
        if format_version != MANIFEST_VERSION {
            return Err(AcceptanceError::Parse(format!(
                "unsupported format_version {format_version}"
            )));
        }
        let corpus_root = parse_path(root.get("corpus_root"), "corpus_root")?;
        let artifact_root = parse_path(root.get("artifact_root"), "artifact_root")?;
        let mut cases = Vec::with_capacity(sections.len());
        for (id, values) in sections {
            cases.push(parse_case(&id, &values)?);
        }
        if cases.is_empty() {
            return Err(AcceptanceError::Parse(
                "manifest contains no cases".to_owned(),
            ));
        }
        Ok(Self {
            format_version,
            corpus_root,
            artifact_root,
            cases,
        })
    }

    pub fn case(&self, id: &str) -> Option<&AcceptanceCase> {
        self.cases.iter().find(|case| case.id == id)
    }
}

pub struct AcceptanceRunner {
    manifest: AcceptanceManifest,
    corpus_root: PathBuf,
    artifact_root: PathBuf,
}

impl AcceptanceRunner {
    pub fn new(manifest: AcceptanceManifest, project_root: &Path) -> Self {
        Self {
            corpus_root: project_root.join(&manifest.corpus_root),
            artifact_root: project_root.join(&manifest.artifact_root),
            manifest,
        }
    }

    pub fn manifest(&self) -> &AcceptanceManifest {
        &self.manifest
    }

    pub fn run_enabled(&self) -> Result<Vec<CaseReport>, AcceptanceError> {
        let enabled: Vec<AcceptanceCase> = self
            .manifest
            .cases
            .iter()
            .filter(|case| case.enabled)
            .cloned()
            .collect();
        if enabled.is_empty() {
            return Err(AcceptanceError::InvalidCase(
                "no enabled cases in manifest".to_owned(),
            ));
        }
        enabled.iter().map(|case| self.run_case(case)).collect()
    }

    pub fn run_case(&self, case: &AcceptanceCase) -> Result<CaseReport, AcceptanceError> {
        validate_case(case)?;
        let rom_path = safe_join(&self.corpus_root, &case.source_id, &case.rom_path)?;
        let rom_bytes = read_required(&rom_path)?;
        let actual_checksum = sha256_hex(&rom_bytes);
        if actual_checksum != case.rom_sha256 {
            return Err(AcceptanceError::Checksum {
                path: rom_path,
                expected: case.rom_sha256.clone(),
                actual: actual_checksum,
            });
        }

        let runner = HeadlessRunner::default();
        let mut console = runner.load_path(&rom_path).map_err(|error| {
            AcceptanceError::InvalidCase(format!(
                "{} could not load: {error:?}",
                rom_path.display()
            ))
        })?;
        let run = execute_case(&mut console, case)?;
        let replay = if case.deterministic_replay {
            let mut replay_console = runner.load_path(&rom_path).map_err(|error| {
                AcceptanceError::InvalidCase(format!(
                    "{} could not reload for deterministic replay: {error:?}",
                    rom_path.display()
                ))
            })?;
            Some(execute_case(&mut replay_console, case)?)
        } else {
            None
        };
        let mut report = CaseReport {
            case_id: case.id.clone(),
            status: if case.diagnostic_only {
                CaseStatus::Diagnostic
            } else {
                CaseStatus::Passed
            },
            run,
            message: None,
            mismatch: None,
            artifacts: ArtifactPaths::default(),
        };

        if let Some(replay) = replay.as_ref() {
            if replay != &report.run {
                report.status = if case.diagnostic_only {
                    CaseStatus::Diagnostic
                } else {
                    CaseStatus::Failed
                };
                report.message = Some("deterministic replay produced a different run".to_owned());
                if let Some(frame) = report.run.captured_frame.as_ref() {
                    report.artifacts =
                        write_artifacts(&self.artifact_root, case, &report.run, frame, None, None)?;
                }
                return Ok(report);
            }
        }

        if let Some(observation) = report.run.result {
            match observation.status {
                ResultStatus::Passed => {}
                ResultStatus::Running => {
                    report.status = if case.diagnostic_only {
                        CaseStatus::Diagnostic
                    } else {
                        CaseStatus::Failed
                    };
                    report.message = Some(format!(
                        "result protocol still running at ${:06x} (value {})",
                        observation.address.get(),
                        observation.value
                    ));
                    return Ok(report);
                }
                ResultStatus::Failed(value) => {
                    report.status = if case.diagnostic_only {
                        CaseStatus::Diagnostic
                    } else {
                        CaseStatus::Failed
                    };
                    report.message = Some(format!(
                        "result protocol failed at ${:06x} with value {value:#04x}",
                        observation.address.get()
                    ));
                    return Ok(report);
                }
            }
        }

        if let Some(frame) = report.run.captured_frame.as_ref() {
            report.artifacts =
                write_artifacts(&self.artifact_root, case, &report.run, frame, None, None)?;
        }

        let Some(reference_path) = case.reference_path.as_ref() else {
            if case.result_protocol.is_some() {
                return Ok(report);
            }
            if !case.diagnostic_only {
                report.status = CaseStatus::Failed;
                report.message = Some("strict case has no reference frame".to_owned());
            }
            return Ok(report);
        };
        let reference_path = safe_join(&self.corpus_root, &case.source_id, reference_path)?;
        let reference_bytes = match read_required(&reference_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                if case.diagnostic_only {
                    report.message = Some(error.to_string());
                    return Ok(report);
                }
                return Err(error);
            }
        };
        if let Some(expected_checksum) = &case.reference_sha256 {
            let actual = sha256_hex(&reference_bytes);
            if &actual != expected_checksum {
                return Err(AcceptanceError::Checksum {
                    path: reference_path,
                    expected: expected_checksum.clone(),
                    actual,
                });
            }
        }
        let expected = match decode_reference(
            &case.reference_format,
            &reference_bytes,
            case.width,
            case.height,
        ) {
            Ok(reference) => reference,
            Err(error) if case.diagnostic_only => {
                report.message = Some(error.to_string());
                return Ok(report);
            }
            Err(error) => return Err(error),
        };
        if expected.width != case.width || expected.height != case.height {
            report.status = CaseStatus::Failed;
            report.message = Some(format!(
                "reference dimensions are {}x{}, descriptor requires {}x{}",
                expected.width, expected.height, case.width, case.height
            ));
            return Ok(report);
        }
        let mask = load_mask(
            &self.corpus_root,
            &case.source_id,
            case,
            expected.pixels.len(),
        )?;
        let Some(frame) = report.run.captured_frame.as_ref() else {
            report.status = CaseStatus::Failed;
            report.message = Some("strict visual case did not request a captured frame".to_owned());
            return Ok(report);
        };
        if let Err(mismatch) = compare_frame_masked(
            &frame.as_output(),
            expected.width,
            expected.height,
            &expected.pixels,
            &mask,
            case.channel_tolerance,
        ) {
            report.status = CaseStatus::Failed;
            report.message = Some(format!("first mismatch at pixel {}", mismatch.index));
            report.mismatch = Some(mismatch);
            report.artifacts = write_artifacts(
                &self.artifact_root,
                case,
                &report.run,
                frame,
                Some(&expected),
                Some(&mask),
            )?;
        } else {
            report.status = if case.diagnostic_only {
                CaseStatus::Diagnostic
            } else {
                CaseStatus::Passed
            };
            report.artifacts = write_artifacts(
                &self.artifact_root,
                case,
                &report.run,
                frame,
                Some(&expected),
                Some(&mask),
            )?;
        }
        Ok(report)
    }
}

impl CapturedFrame {
    fn as_output(&self) -> FrameOutput<'_> {
        FrameOutput {
            pixels: &self.pixels,
            width: self.width,
            height: self.height,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecodedReference {
    width: u16,
    height: u16,
    pixels: Vec<u32>,
}

fn execute_case(
    console: &mut Console,
    case: &AcceptanceCase,
) -> Result<AcceptanceRun, AcceptanceError> {
    console.reset();
    if let Some(target_frame) = case.capture_frame {
        let max_frames = case.max_frames.unwrap_or(target_frame);
        if target_frame > max_frames {
            return Err(AcceptanceError::Bound(format!(
                "capture_frame {target_frame} exceeds max_frames {max_frames}"
            )));
        }
        for _ in 0..target_frame {
            console.run_for_frames(1);
            if let Some(limit) = case.max_master_ticks {
                if console.master_ticks() > limit {
                    return Err(AcceptanceError::Bound(format!(
                        "frame {} exceeded max_master_ticks {}",
                        target_frame, limit
                    )));
                }
            }
            if console.is_stopped() {
                break;
            }
        }
        let (width, height, pixels) = {
            let frame = console.next_frame();
            (frame.width, frame.height, frame.pixels.to_vec())
        };
        let master_ticks = console.master_ticks();
        let stopped = console.is_stopped();
        let result = case
            .result_protocol
            .as_ref()
            .map(|protocol| protocol.observe(console));
        return Ok(AcceptanceRun {
            master_ticks,
            stopped,
            result,
            captured_frame: Some(CapturedFrame {
                frame_number: target_frame,
                master_ticks,
                width,
                height,
                pixels,
            }),
        });
    }

    match (case.max_master_ticks, case.max_frames) {
        (Some(ticks), _) => console.run_for_master_ticks(ticks),
        (None, Some(frames)) => console.run_for_frames(frames),
        (None, None) => {
            return Err(AcceptanceError::Bound(
                "case must specify max_master_ticks, max_frames, or capture_frame".to_owned(),
            ))
        }
    }
    let result = case
        .result_protocol
        .as_ref()
        .map(|protocol| protocol.observe(console));
    Ok(AcceptanceRun {
        master_ticks: console.master_ticks(),
        stopped: console.is_stopped(),
        result,
        captured_frame: None,
    })
}

fn write_artifacts(
    artifact_root: &Path,
    case: &AcceptanceCase,
    run: &AcceptanceRun,
    frame: &CapturedFrame,
    expected: Option<&DecodedReference>,
    mask: Option<&[bool]>,
) -> Result<ArtifactPaths, AcceptanceError> {
    let directory = artifact_root.join(&case.artifact_name);
    fs::create_dir_all(&directory)
        .map_err(|error| AcceptanceError::Io(format!("{}: {error}", directory.display())))?;
    let actual_path = directory.join("actual.ppm");
    write_ppm(&actual_path, frame.width, frame.height, &frame.pixels)?;
    let metadata_path = directory.join("metadata.txt");
    let mut metadata = String::new();
    metadata.push_str(&format!("case = {}\n", case.id));
    metadata.push_str(&format!("source = {}\n", case.source_id));
    metadata.push_str(&format!("revision = {}\n", case.revision));
    metadata.push_str(&format!("rom = {}\n", case.rom_path.display()));
    metadata.push_str(&format!("rom_sha256 = {}\n", case.rom_sha256));
    metadata.push_str(&format!("frame = {}\n", frame.frame_number));
    metadata.push_str(&format!("master_ticks = {}\n", run.master_ticks));
    metadata.push_str(&format!("stopped = {}\n", run.stopped));
    metadata.push_str(&format!(
        "deterministic_replay = {}\n",
        case.deterministic_replay
    ));
    metadata.push_str(&format!(
        "actual_sha256 = {}\n",
        sha256_hex(&argb_bytes(&frame.pixels))
    ));
    fs::write(&metadata_path, metadata)
        .map_err(|error| AcceptanceError::Io(format!("{}: {error}", metadata_path.display())))?;

    let mut paths = ArtifactPaths {
        metadata: Some(metadata_path),
        actual: Some(actual_path),
        diff: None,
    };
    if let Some(expected) = expected {
        let mask = mask.unwrap_or(&[]);
        let mut diff = vec![0xff00_0000; frame.pixels.len()];
        for (index, actual) in frame.pixels.iter().copied().enumerate() {
            let ignored = !mask.is_empty() && mask[index];
            diff[index] = if ignored
                || pixel_within_tolerance(actual, expected.pixels[index], case.channel_tolerance)
            {
                0xff00_0000
            } else {
                0xffff_0000
            };
        }
        if diff.iter().any(|pixel| *pixel == 0xffff_0000) {
            let diff_path = directory.join("diff.ppm");
            write_ppm(&diff_path, frame.width, frame.height, &diff)?;
            paths.diff = Some(diff_path);
        }
    }
    Ok(paths)
}

fn write_ppm(path: &Path, width: u16, height: u16, pixels: &[u32]) -> Result<(), AcceptanceError> {
    if pixels.len() != usize::from(width) * usize::from(height) {
        return Err(AcceptanceError::InvalidReference(format!(
            "cannot write {}x{} frame with {} pixels",
            width,
            height,
            pixels.len()
        )));
    }
    let mut bytes = format!("P6\n{} {}\n255\n", width, height).into_bytes();
    for pixel in pixels {
        bytes.push((pixel >> 16) as u8);
        bytes.push((pixel >> 8) as u8);
        bytes.push(*pixel as u8);
    }
    fs::write(path, bytes)
        .map_err(|error| AcceptanceError::Io(format!("{}: {error}", path.display())))
}

fn decode_reference(
    format: &ReferenceFormat,
    bytes: &[u8],
    width: u16,
    height: u16,
) -> Result<DecodedReference, AcceptanceError> {
    match format {
        ReferenceFormat::Ppm => decode_ppm(bytes),
        ReferenceFormat::Argb32 => {
            let pixel_count = reference_pixel_count(width, height)?;
            if bytes.len() != pixel_count * 4 {
                return Err(AcceptanceError::InvalidReference(format!(
                    "ARGB32 reference has {} bytes, expected {}",
                    bytes.len(),
                    pixel_count * 4
                )));
            }
            let pixels = bytes
                .chunks_exact(4)
                .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect::<Vec<_>>();
            Ok(DecodedReference {
                width,
                height,
                pixels,
            })
        }
        ReferenceFormat::Png => decode_png(bytes, width, height),
        ReferenceFormat::Jpeg => decode_jpeg(bytes, width, height),
        ReferenceFormat::Unsupported(format) => {
            Err(AcceptanceError::UnsupportedReference(format.clone()))
        }
    }
}

fn decode_png(
    bytes: &[u8],
    expected_width: u16,
    expected_height: u16,
) -> Result<DecodedReference, AcceptanceError> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    // Keep checksum validation enabled (the default), while normalizing the
    // common indexed, low-bit-depth, and 16-bit PNG variants to 8-bit samples.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|error| AcceptanceError::InvalidReference(format!("PNG header: {error:?}")))?;
    let (source_width, source_height) = reader.info().size();
    let (width, height) = validate_decoded_dimensions(
        source_width,
        source_height,
        expected_width,
        expected_height,
        "PNG",
    )?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let output = reader
        .next_frame(&mut buffer)
        .map_err(|error| AcceptanceError::InvalidReference(format!("PNG image: {error:?}")))?;
    let (color_type, bit_depth) = reader.output_color_type();
    if bit_depth != png::BitDepth::Eight {
        return Err(AcceptanceError::InvalidReference(format!(
            "PNG decoder returned unsupported bit depth {bit_depth:?}"
        )));
    }
    let data = &buffer[..output.buffer_size()];
    let pixel_count = reference_pixel_count(width, height)?;
    let pixels = match color_type {
        png::ColorType::Rgb => {
            if data.len() != pixel_count * 3 {
                return Err(invalid_decoded_buffer(
                    "PNG RGB",
                    data.len(),
                    pixel_count * 3,
                ));
            }
            data.chunks_exact(3)
                .map(|chunk| {
                    0xff00_0000
                        | (u32::from(chunk[0]) << 16)
                        | (u32::from(chunk[1]) << 8)
                        | u32::from(chunk[2])
                })
                .collect()
        }
        png::ColorType::Rgba => {
            if data.len() != pixel_count * 4 {
                return Err(invalid_decoded_buffer(
                    "PNG RGBA",
                    data.len(),
                    pixel_count * 4,
                ));
            }
            data.chunks_exact(4)
                .map(|chunk| {
                    (u32::from(chunk[3]) << 24)
                        | (u32::from(chunk[0]) << 16)
                        | (u32::from(chunk[1]) << 8)
                        | u32::from(chunk[2])
                })
                .collect()
        }
        png::ColorType::Grayscale => {
            if data.len() != pixel_count {
                return Err(invalid_decoded_buffer(
                    "PNG grayscale",
                    data.len(),
                    pixel_count,
                ));
            }
            data.iter()
                .copied()
                .map(|gray| {
                    0xff00_0000 | (u32::from(gray) << 16) | (u32::from(gray) << 8) | u32::from(gray)
                })
                .collect()
        }
        png::ColorType::GrayscaleAlpha => {
            if data.len() != pixel_count * 2 {
                return Err(invalid_decoded_buffer(
                    "PNG grayscale-alpha",
                    data.len(),
                    pixel_count * 2,
                ));
            }
            data.chunks_exact(2)
                .map(|chunk| {
                    (u32::from(chunk[1]) << 24)
                        | (u32::from(chunk[0]) << 16)
                        | (u32::from(chunk[0]) << 8)
                        | u32::from(chunk[0])
                })
                .collect()
        }
        png::ColorType::Indexed => {
            return Err(AcceptanceError::InvalidReference(
                "PNG decoder returned an unexpanded indexed image".to_owned(),
            ))
        }
    };
    Ok(DecodedReference {
        width,
        height,
        pixels,
    })
}

fn decode_jpeg(
    bytes: &[u8],
    expected_width: u16,
    expected_height: u16,
) -> Result<DecodedReference, AcceptanceError> {
    let expected_pixels = reference_pixel_count(expected_width, expected_height)?;
    let max_decoded_bytes = expected_pixels.checked_mul(4).ok_or_else(|| {
        AcceptanceError::InvalidReference("JPEG reference is too large".to_owned())
    })?;
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(bytes));
    decoder.set_max_decoding_buffer_size(max_decoded_bytes);
    decoder
        .read_info()
        .map_err(|error| AcceptanceError::InvalidReference(format!("JPEG header: {error:?}")))?;
    let info = decoder.info().ok_or_else(|| {
        AcceptanceError::InvalidReference("JPEG decoder returned no image metadata".to_owned())
    })?;
    let (width, height) = validate_decoded_dimensions(
        u32::from(info.width),
        u32::from(info.height),
        expected_width,
        expected_height,
        "JPEG",
    )?;
    let data = decoder
        .decode()
        .map_err(|error| AcceptanceError::InvalidReference(format!("JPEG image: {error:?}")))?;
    let pixel_count = reference_pixel_count(width, height)?;
    let pixels = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => {
            if data.len() != pixel_count * 3 {
                return Err(invalid_decoded_buffer(
                    "JPEG RGB",
                    data.len(),
                    pixel_count * 3,
                ));
            }
            data.chunks_exact(3)
                .map(|chunk| {
                    0xff00_0000
                        | (u32::from(chunk[0]) << 16)
                        | (u32::from(chunk[1]) << 8)
                        | u32::from(chunk[2])
                })
                .collect()
        }
        jpeg_decoder::PixelFormat::L8 => {
            if data.len() != pixel_count {
                return Err(invalid_decoded_buffer(
                    "JPEG grayscale",
                    data.len(),
                    pixel_count,
                ));
            }
            data.iter()
                .copied()
                .map(|gray| {
                    0xff00_0000 | (u32::from(gray) << 16) | (u32::from(gray) << 8) | u32::from(gray)
                })
                .collect()
        }
        jpeg_decoder::PixelFormat::L16 => {
            if data.len() != pixel_count * 2 {
                return Err(invalid_decoded_buffer(
                    "JPEG 16-bit grayscale",
                    data.len(),
                    pixel_count * 2,
                ));
            }
            data.chunks_exact(2)
                .map(|chunk| {
                    let gray = chunk[0];
                    0xff00_0000 | (u32::from(gray) << 16) | (u32::from(gray) << 8) | u32::from(gray)
                })
                .collect()
        }
        jpeg_decoder::PixelFormat::CMYK32 => {
            return Err(AcceptanceError::InvalidReference(
                "JPEG CMYK references are not supported; convert to RGB or grayscale".to_owned(),
            ))
        }
    };
    Ok(DecodedReference {
        width,
        height,
        pixels,
    })
}

fn validate_decoded_dimensions(
    actual_width: u32,
    actual_height: u32,
    expected_width: u16,
    expected_height: u16,
    format: &str,
) -> Result<(u16, u16), AcceptanceError> {
    let width = u16::try_from(actual_width).map_err(|_| {
        AcceptanceError::InvalidReference(format!(
            "{format} width {actual_width} exceeds the descriptor range"
        ))
    })?;
    let height = u16::try_from(actual_height).map_err(|_| {
        AcceptanceError::InvalidReference(format!(
            "{format} height {actual_height} exceeds the descriptor range"
        ))
    })?;
    if width != expected_width || height != expected_height {
        return Err(AcceptanceError::InvalidReference(format!(
            "{format} dimensions are {width}x{height}, descriptor requires {expected_width}x{expected_height}"
        )));
    }
    Ok((width, height))
}

fn reference_pixel_count(width: u16, height: u16) -> Result<usize, AcceptanceError> {
    usize::from(width)
        .checked_mul(usize::from(height))
        .ok_or_else(|| AcceptanceError::InvalidReference("reference is too large".to_owned()))
}

fn invalid_decoded_buffer(format: &str, actual: usize, expected: usize) -> AcceptanceError {
    AcceptanceError::InvalidReference(format!(
        "{format} decoder returned {actual} bytes, expected {expected}"
    ))
}

fn decode_ppm(bytes: &[u8]) -> Result<DecodedReference, AcceptanceError> {
    if !bytes.starts_with(b"P6") {
        return Err(AcceptanceError::InvalidReference(
            "only binary P6 PPM is supported".to_owned(),
        ));
    }
    let mut position = 2;
    let width = next_token(bytes, &mut position)?
        .parse::<u16>()
        .map_err(|_| AcceptanceError::InvalidReference("invalid PPM width".to_owned()))?;
    let height = next_token(bytes, &mut position)?
        .parse::<u16>()
        .map_err(|_| AcceptanceError::InvalidReference("invalid PPM height".to_owned()))?;
    let max_value = next_token(bytes, &mut position)?
        .parse::<u16>()
        .map_err(|_| AcceptanceError::InvalidReference("invalid PPM max value".to_owned()))?;
    if max_value != 255 {
        return Err(AcceptanceError::InvalidReference(
            "PPM max value must be 255".to_owned(),
        ));
    }
    if position >= bytes.len() || !bytes[position].is_ascii_whitespace() {
        return Err(AcceptanceError::InvalidReference(
            "PPM header has no separator".to_owned(),
        ));
    }
    position += 1;
    let pixel_count = usize::from(width) * usize::from(height);
    let expected_bytes = pixel_count * 3;
    if bytes.len().saturating_sub(position) != expected_bytes {
        return Err(AcceptanceError::InvalidReference(format!(
            "PPM contains {} bytes, expected {expected_bytes}",
            bytes.len().saturating_sub(position)
        )));
    }
    let mut pixels = Vec::with_capacity(pixel_count);
    for chunk in bytes[position..].chunks_exact(3) {
        pixels.push(
            0xff00_0000
                | (u32::from(chunk[0]) << 16)
                | (u32::from(chunk[1]) << 8)
                | u32::from(chunk[2]),
        );
    }
    Ok(DecodedReference {
        width,
        height,
        pixels,
    })
}

fn next_token(bytes: &[u8], position: &mut usize) -> Result<String, AcceptanceError> {
    while *position < bytes.len() {
        if bytes[*position].is_ascii_whitespace() {
            *position += 1;
        } else if bytes[*position] == b'#' {
            while *position < bytes.len() && bytes[*position] != b'\n' {
                *position += 1;
            }
        } else {
            break;
        }
    }
    let start = *position;
    while *position < bytes.len() && !bytes[*position].is_ascii_whitespace() {
        *position += 1;
    }
    if start == *position {
        return Err(AcceptanceError::InvalidReference(
            "truncated PPM header".to_owned(),
        ));
    }
    String::from_utf8(bytes[start..*position].to_vec())
        .map_err(|_| AcceptanceError::InvalidReference("PPM header is not UTF-8".to_owned()))
}

fn load_mask(
    corpus_root: &Path,
    source_id: &str,
    case: &AcceptanceCase,
    pixel_count: usize,
) -> Result<Vec<bool>, AcceptanceError> {
    let Some(mask_path) = case.mask_path.as_ref() else {
        return Ok(Vec::new());
    };
    let path = safe_join(corpus_root, source_id, mask_path)?;
    let bytes = read_required(&path)?;
    if bytes.len() != pixel_count {
        return Err(AcceptanceError::InvalidReference(format!(
            "mask {} has {} bytes, expected {pixel_count}",
            path.display(),
            bytes.len()
        )));
    }
    Ok(bytes.into_iter().map(|byte| byte != 0).collect())
}

fn parse_case(
    id: &str,
    values: &BTreeMap<String, String>,
) -> Result<AcceptanceCase, AcceptanceError> {
    let source_id = parse_string(values.get("source"), "source")?;
    let revision = parse_string(values.get("revision"), "revision")?;
    let rom_path = parse_path(values.get("rom"), "rom")?;
    let rom_sha256 = parse_string(values.get("rom_sha256"), "rom_sha256")?;
    validate_hex(&rom_sha256, 64, "rom_sha256")?;
    let enabled = parse_bool(values.get("enabled"), "enabled")?.unwrap_or(false);
    let diagnostic_only =
        parse_bool(values.get("diagnostic_only"), "diagnostic_only")?.unwrap_or(false);
    let max_frames = parse_optional_u64(values.get("max_frames"), "max_frames")?;
    let max_master_ticks = parse_optional_u64(values.get("max_master_ticks"), "max_master_ticks")?;
    let deterministic_replay =
        parse_bool(values.get("deterministic_replay"), "deterministic_replay")?.unwrap_or(true);
    let capture_frame = parse_optional_u64(values.get("capture_frame"), "capture_frame")?;
    let reference_path = parse_optional_path(values.get("reference"), "reference")?;
    let reference_sha256 =
        parse_optional_string(values.get("reference_sha256"), "reference_sha256")?;
    if let Some(checksum) = &reference_sha256 {
        validate_hex(checksum, 64, "reference_sha256")?;
    }
    let reference_format = match values.get("reference_format") {
        Some(value) => ReferenceFormat::parse(&parse_string(Some(value), "reference_format")?),
        None => reference_path
            .as_ref()
            .and_then(|path| path.extension().and_then(|extension| extension.to_str()))
            .map(|extension| ReferenceFormat::parse(extension))
            .unwrap_or(ReferenceFormat::Unsupported("none".to_owned())),
    };
    let result_protocol = parse_result_protocol(values)?;
    let width = parse_optional_u64(values.get("width"), "width")?.unwrap_or(256);
    let height = parse_optional_u64(values.get("height"), "height")?.unwrap_or(224);
    let channel_tolerance =
        parse_optional_u64(values.get("channel_tolerance"), "channel_tolerance")?.unwrap_or(0);
    let mask_path = parse_optional_path(values.get("mask"), "mask")?;
    let artifact_name =
        parse_optional_string(values.get("artifact"), "artifact")?.unwrap_or_else(|| id.to_owned());
    if width == 0 || width > u64::from(u16::MAX) || height == 0 || height > u64::from(u16::MAX) {
        return Err(AcceptanceError::InvalidCase(format!(
            "{id}: invalid dimensions"
        )));
    }
    if channel_tolerance > u64::from(u8::MAX) {
        return Err(AcceptanceError::InvalidCase(format!(
            "{id}: invalid channel_tolerance"
        )));
    }
    Ok(AcceptanceCase {
        id: id.to_owned(),
        source_id,
        revision,
        rom_path,
        rom_sha256,
        enabled,
        diagnostic_only,
        max_frames,
        max_master_ticks,
        deterministic_replay,
        capture_frame,
        reference_path,
        reference_sha256,
        reference_format,
        result_protocol,
        width: width as u16,
        height: height as u16,
        channel_tolerance: channel_tolerance as u8,
        mask_path,
        artifact_name,
    })
}

fn validate_case(case: &AcceptanceCase) -> Result<(), AcceptanceError> {
    if !valid_name(&case.id) || !valid_name(&case.source_id) || !valid_name(&case.artifact_name) {
        return Err(AcceptanceError::InvalidCase(format!(
            "{} contains an unsafe name",
            case.id
        )));
    }
    if case.max_frames.is_none() && case.max_master_ticks.is_none() && case.capture_frame.is_none()
    {
        return Err(AcceptanceError::Bound(format!(
            "{} has no execution bound",
            case.id
        )));
    }
    if case.reference_path.is_some() && case.capture_frame.is_none() {
        return Err(AcceptanceError::InvalidCase(format!(
            "{} has a reference but no capture_frame",
            case.id
        )));
    }
    if !case.diagnostic_only && case.reference_path.is_none() && case.result_protocol.is_none() {
        return Err(AcceptanceError::InvalidCase(format!(
            "{} has neither a reference frame nor a result protocol",
            case.id
        )));
    }
    validate_hex(&case.rom_sha256, 64, "rom_sha256")
}

fn parse_result_protocol(
    values: &BTreeMap<String, String>,
) -> Result<Option<ResultProtocol>, AcceptanceError> {
    let Some(protocol) = values.get("result_protocol") else {
        for key in ["result_address", "result_running", "result_passed"] {
            if values.contains_key(key) {
                return Err(AcceptanceError::Parse(format!(
                    "{key} requires result_protocol"
                )));
            }
        }
        return Ok(None);
    };
    let protocol = parse_string(Some(protocol), "result_protocol")?;
    match protocol.as_str() {
        "memory-byte" => {
            let address = parse_u64(values.get("result_address"), "result_address")?;
            if address > u64::from(Address24::MAX.get()) {
                return Err(AcceptanceError::InvalidCase(format!(
                    "result_address {address:#x} exceeds the 24-bit bus"
                )));
            }
            let running = parse_u64(values.get("result_running"), "result_running")?;
            let passed = parse_u64(values.get("result_passed"), "result_passed")?;
            if running > u64::from(u8::MAX) || passed > u64::from(u8::MAX) {
                return Err(AcceptanceError::InvalidCase(
                    "memory-byte result values must fit in one byte".to_owned(),
                ));
            }
            Ok(Some(ResultProtocol::memory_byte(
                Address24::new(address as u32),
                running as u8,
                passed as u8,
            )))
        }
        other => Err(AcceptanceError::Parse(format!(
            "unsupported result_protocol {other:?}; supported protocols: memory-byte"
        ))),
    }
}

fn read_required(path: &Path) -> Result<Vec<u8>, AcceptanceError> {
    if !path.is_file() {
        return Err(AcceptanceError::MissingFile(path.to_owned()));
    }
    fs::read(path).map_err(|error| AcceptanceError::Io(format!("{}: {error}", path.display())))
}

fn safe_join(root: &Path, source: &str, relative: &Path) -> Result<PathBuf, AcceptanceError> {
    if !valid_name(source)
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AcceptanceError::InvalidCase(format!(
            "unsafe corpus path {source}/{}",
            relative.display()
        )));
    }
    Ok(root.join(source).join(relative))
}

fn parse_path(value: Option<&String>, key: &str) -> Result<PathBuf, AcceptanceError> {
    let path = parse_string(value, key)?;
    if path.is_empty() || Path::new(&path).is_absolute() {
        return Err(AcceptanceError::Parse(format!(
            "{key} must be a relative path"
        )));
    }
    Ok(PathBuf::from(path))
}

fn parse_optional_path(
    value: Option<&String>,
    key: &str,
) -> Result<Option<PathBuf>, AcceptanceError> {
    match value {
        None => Ok(None),
        Some(value) if parse_string(Some(value), key)?.is_empty() => Ok(None),
        Some(_) => parse_path(value, key).map(Some),
    }
}

fn parse_string(value: Option<&String>, key: &str) -> Result<String, AcceptanceError> {
    let Some(value) = value else {
        return Err(AcceptanceError::Parse(format!("missing {key}")));
    };
    let value = value.trim();
    if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
        return Err(AcceptanceError::Parse(format!(
            "{key} must be a quoted string"
        )));
    }
    let inner = &value[1..value.len() - 1];
    let mut output = String::new();
    let mut escaped = false;
    for character in inner.chars() {
        if escaped {
            match character {
                '"' => output.push('"'),
                '\\' => output.push('\\'),
                'n' => output.push('\n'),
                other => {
                    return Err(AcceptanceError::Parse(format!(
                        "{key} has invalid escape \\{other}"
                    )))
                }
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            output.push(character);
        }
    }
    if escaped {
        return Err(AcceptanceError::Parse(format!(
            "{key} has a trailing escape"
        )));
    }
    Ok(output)
}

fn parse_optional_string(
    value: Option<&String>,
    key: &str,
) -> Result<Option<String>, AcceptanceError> {
    value
        .map(|value| parse_string(Some(value), key))
        .transpose()
}

fn parse_u64(value: Option<&String>, key: &str) -> Result<u64, AcceptanceError> {
    parse_optional_u64(value, key)?.ok_or_else(|| AcceptanceError::Parse(format!("missing {key}")))
}

fn parse_optional_u64(value: Option<&String>, key: &str) -> Result<Option<u64>, AcceptanceError> {
    value
        .map(|value| {
            let value = value.trim();
            let parsed = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
                .map_or_else(
                    || value.parse::<u64>(),
                    |digits| u64::from_str_radix(digits, 16),
                );
            parsed.map_err(|_| AcceptanceError::Parse(format!("{key} must be an unsigned integer")))
        })
        .transpose()
}

fn parse_bool(value: Option<&String>, key: &str) -> Result<Option<bool>, AcceptanceError> {
    value
        .map(|value| match value.trim() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(AcceptanceError::Parse(format!(
                "{key} must be true or false"
            ))),
        })
        .transpose()
}

fn validate_hex(value: &str, length: usize, key: &str) -> Result<(), AcceptanceError> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(AcceptanceError::Parse(format!(
            "{key} must be lowercase hexadecimal ({length} characters)"
        )));
    }
    Ok(())
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' || byte == b'.'
        })
}

fn strip_comment(line: &str) -> String {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == '#' && !quoted {
            return line[..index].to_owned();
        }
    }
    line.to_owned()
}

fn argb_bytes(pixels: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(pixels.len() * 4);
    for pixel in pixels {
        bytes.extend_from_slice(&pixel.to_le_bytes());
    }
    bytes
}

/// Returns the lowercase SHA-256 digest without requiring a crypto crate.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut state = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_length = (bytes.len() as u64).wrapping_mul(8);
    let mut message = bytes.to_vec();
    message.push(0x80);
    while (message.len() + 8) % 64 != 0 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    for block in message.chunks_exact(64) {
        let mut schedule = [0u32; 64];
        for (index, word) in schedule[..16].iter_mut().enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                block[offset],
                block[offset + 1],
                block[offset + 2],
                block[offset + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h) = (
            state[0], state[1], state[2], state[3], state[4], state[5], state[6], state[7],
        );
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(schedule[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_case(rom: &[u8], reference: &str) -> AcceptanceCase {
        AcceptanceCase {
            id: "synthetic".to_owned(),
            source_id: "fixture".to_owned(),
            revision: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            rom_path: PathBuf::from("test.sfc"),
            rom_sha256: sha256_hex(rom),
            enabled: true,
            diagnostic_only: false,
            max_frames: Some(0),
            max_master_ticks: None,
            deterministic_replay: true,
            capture_frame: Some(0),
            reference_path: Some(PathBuf::from(reference)),
            reference_sha256: None,
            reference_format: ReferenceFormat::Ppm,
            result_protocol: None,
            width: 256,
            height: 224,
            channel_tolerance: 0,
            mask_path: None,
            artifact_name: "synthetic".to_owned(),
        }
    }

    #[test]
    fn sha256_matches_standard_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn manifest_parser_keeps_explicit_case_metadata() {
        let manifest = AcceptanceManifest::parse(
            r#"
format_version = 1
corpus_root = "materialized"
artifact_root = "target/snes-acceptance"

[case.cpuadc]
source = "peterlemon-snes"
revision = "0123456789abcdef0123456789abcdef01234567"
rom = "CPU/CPUADC.sfc"
rom_sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
enabled = false
diagnostic_only = true
max_frames = 1
capture_frame = 0
reference = "CPU/CPUADC.png"
reference_format = "png"
width = 256
height = 224
channel_tolerance = 0
mask = ""
artifact = "cpuadc"
"#,
        )
        .unwrap();
        let case = manifest.case("cpuadc").unwrap();
        assert_eq!(case.source_id, "peterlemon-snes");
        assert_eq!(case.reference_format, ReferenceFormat::Png);
        assert!(case.diagnostic_only);
        assert!(case.mask_path.is_none());
    }

    #[test]
    fn manifest_parser_accepts_memory_result_protocol() {
        let manifest = AcceptanceManifest::parse(
            r#"
format_version = 1
corpus_root = "materialized"
artifact_root = "target/snes-acceptance"

[case.result]
source = "higan-snes-test-roms"
revision = "0123456789abcdef0123456789abcdef01234567"
rom = "absindx/test.sfc"
rom_sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
enabled = false
diagnostic_only = true
max_master_ticks = 1000
result_protocol = "memory-byte"
result_address = 0x000000
result_running = 0
result_passed = 1
artifact = "result"
"#,
        )
        .unwrap();
        assert_eq!(
            manifest.case("result").unwrap().result_protocol,
            Some(ResultProtocol::memory_byte(Address24::new(0), 0, 1))
        );
    }

    #[test]
    fn argb32_reference_uses_declared_non_square_dimensions() {
        let bytes = vec![0, 0, 0xff, 0xff, 0xff, 0, 0, 0xff];
        let decoded = decode_reference(&ReferenceFormat::Argb32, &bytes, 2, 1).unwrap();
        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 1);
        assert_eq!(decoded.pixels.len(), 2);
    }

    fn encoded_rgb_png() -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0xff, 0, 0, 0, 0xff, 0]).unwrap();
        }
        bytes
    }

    #[test]
    fn png_reference_decodes_pixels_and_checks_dimensions() {
        let bytes = encoded_rgb_png();
        let decoded = decode_reference(&ReferenceFormat::Png, &bytes, 2, 1).unwrap();
        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 1);
        assert_eq!(decoded.pixels, vec![0xffff_0000, 0xff00_ff00]);

        let error = decode_reference(&ReferenceFormat::Png, &bytes, 1, 2).unwrap_err();
        assert!(error.to_string().contains("dimensions"));
    }

    #[test]
    fn png_reference_rejects_corrupt_chunk_checksum() {
        let mut bytes = encoded_rgb_png();
        let mut position = 8;
        let mut corrupted = false;
        while position + 12 <= bytes.len() {
            let length = u32::from_be_bytes([
                bytes[position],
                bytes[position + 1],
                bytes[position + 2],
                bytes[position + 3],
            ]) as usize;
            let chunk_type = &bytes[position + 4..position + 8];
            let crc = position + 8 + length;
            if chunk_type == b"IDAT" {
                bytes[crc] ^= 1;
                corrupted = true;
                break;
            }
            position = crc + 4;
        }
        assert!(corrupted);
        assert!(decode_reference(&ReferenceFormat::Png, &bytes, 2, 1).is_err());
    }

    #[test]
    fn jpeg_reference_decodes_rgb_and_checks_dimensions() {
        // A valid 2x1 baseline RGB JPEG generated by ffmpeg. JPEG is lossy,
        // so the assertion checks the decoded color family rather than exact RGB.
        let bytes = [
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01, 0x02, 0x00,
            0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xff, 0xfe, 0x00, 0x10, 0x4c, 0x61, 0x76, 0x63,
            0x36, 0x32, 0x2e, 0x32, 0x38, 0x2e, 0x31, 0x30, 0x32, 0x00, 0xff, 0xdb, 0x00, 0x43,
            0x00, 0x08, 0x04, 0x04, 0x04, 0x04, 0x04, 0x05, 0x05, 0x05, 0x05, 0x05, 0x05, 0x06,
            0x06, 0x06, 0x06, 0x06, 0x06, 0x06, 0x06, 0x06, 0x06, 0x06, 0x06, 0x06, 0x07, 0x07,
            0x07, 0x08, 0x08, 0x08, 0x07, 0x07, 0x07, 0x06, 0x06, 0x07, 0x07, 0x08, 0x08, 0x08,
            0x08, 0x09, 0x09, 0x09, 0x08, 0x08, 0x08, 0x08, 0x09, 0x09, 0x0a, 0x0a, 0x0a, 0x0c,
            0x0c, 0x0b, 0x0b, 0x0e, 0x0e, 0x0e, 0x11, 0x11, 0x14, 0xff, 0xc4, 0x00, 0x4c, 0x00,
            0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x06, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x07, 0x10, 0x01, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x11,
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x01, 0x00, 0x02, 0x03, 0x01,
            0x22, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00, 0xff, 0xda, 0x00, 0x0c, 0x03, 0x01,
            0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3f, 0x00, 0x8b, 0x00, 0x4d, 0x7f, 0x7f, 0xff,
            0xd9,
        ];
        let decoded = decode_reference(&ReferenceFormat::Jpeg, &bytes, 2, 1).unwrap();
        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 1);
        assert_eq!(decoded.pixels.len(), 2);
        for pixel in decoded.pixels {
            assert_eq!(pixel >> 24, 0xff);
            assert!(pixel >> 16 & 0xff > 100);
            assert!(pixel >> 8 & 0xff < 100);
            assert!(pixel & 0xff < 100);
        }

        let error = decode_reference(&ReferenceFormat::Jpeg, &bytes, 1, 2).unwrap_err();
        assert!(error.to_string().contains("dimensions"));
    }

    #[test]
    fn strict_case_captures_compares_and_writes_artifacts() {
        let root = std::env::temp_dir().join(format!("snes-acceptance-{}", std::process::id()));
        let corpus = root.join("materialized/fixture");
        let reference = corpus.join("blank.ppm");
        fs::create_dir_all(&corpus).unwrap();
        let mut rom = vec![0xdb; 0x8000];
        rom[0x7fc0..0x7fc0 + 21].copy_from_slice(b"SYNTHETIC ACCEPTANCE!");
        rom[0x7fd5] = 0x20;
        rom[0x7fd6] = 0;
        rom[0x7fd7] = 7;
        rom[0x7fd8] = 0;
        rom[0x7fd9] = 0;
        rom[0x7ffc] = 0;
        rom[0x7ffd] = 0x80;
        fs::write(corpus.join("test.sfc"), &rom).unwrap();
        let pixels = vec![0u32; 256 * 224];
        write_ppm(&reference, 256, 224, &pixels).unwrap();
        let manifest = AcceptanceManifest {
            format_version: MANIFEST_VERSION,
            corpus_root: PathBuf::from("materialized"),
            artifact_root: PathBuf::from("artifacts"),
            cases: vec![synthetic_case(&rom, "blank.ppm")],
        };
        let report = AcceptanceRunner::new(manifest, &root)
            .run_case(&synthetic_case(&rom, "blank.ppm"))
            .unwrap();
        assert_eq!(report.status, CaseStatus::Passed);
        assert!(report.artifacts.actual.as_ref().unwrap().is_file());
        assert!(report.artifacts.metadata.as_ref().unwrap().is_file());
        assert!(report.artifacts.diff.is_none());
        let _ = fs::remove_dir_all(root);
    }
}
