//! LSMV replay export through an external FFmpeg process.

use crate::lsmv::{FrameRate, LsmvMovie};
use snes_core::{controller::ControllerPort, AudioSample, Console, RomImage};
use std::{
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
};

/// The native output rate used by the bounded SNES mixer.
pub const AUDIO_SAMPLE_RATE: u32 = 32_000;

#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub output: PathBuf,
    pub frame_limit: Option<usize>,
    pub scale: u32,
    pub video_only: bool,
    pub sample_dir: Option<PathBuf>,
    pub sample_every: usize,
    pub audio_output: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExportReport {
    pub frames: usize,
    pub width: u16,
    pub height: u16,
    pub audio_samples: usize,
}

#[derive(Debug)]
pub struct ExportError {
    message: String,
}

impl ExportError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn io(context: &str, error: impl fmt::Display) -> Self {
        Self::new(format!("{context}: {error}"))
    }
}

impl fmt::Display for ExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(formatter)
    }
}

impl Error for ExportError {}

pub fn export_movie(
    movie: &LsmvMovie,
    rom: &[u8],
    options: &ExportOptions,
) -> Result<ExportReport, ExportError> {
    if movie.frames.is_empty() {
        return Err(ExportError::new("LSMV movie contains no frames"));
    }
    if options.scale == 0 || options.scale > 32 {
        return Err(ExportError::new("scale must be between 1 and 32"));
    }
    if options.sample_dir.is_some() && options.sample_every == 0 {
        return Err(ExportError::new("sample-every must be greater than zero"));
    }
    if let Some(parent) = options.output.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(ExportError::new(format!(
                "output directory does not exist: {}",
                parent.display()
            )));
        }
    }
    if let Some(path) = &options.audio_output {
        ensure_parent(path)?;
    }
    if let Some(path) = &options.sample_dir {
        fs::create_dir_all(path)
            .map_err(|error| ExportError::io("create sample directory", error))?;
    }

    let image = RomImage::load(rom.to_vec())
        .map_err(|error| ExportError::new(format!("cannot load replay ROM: {error:?}")))?;
    let mut console = Console::new(image)
        .map_err(|error| ExportError::new(format!("cannot create replay console: {error:?}")))?;
    console.reset();

    let frames = options
        .frame_limit
        .unwrap_or(movie.frames.len())
        .min(movie.frames.len());
    if frames == 0 {
        return Err(ExportError::new("frame limit produced an empty replay"));
    }

    let video_path = if options.video_only {
        options.output.clone()
    } else {
        adjacent_temp_path(&options.output, "video.mp4")
    };
    let audio_path = if options.video_only {
        None
    } else if let Some(path) = &options.audio_output {
        Some(path.clone())
    } else {
        Some(adjacent_temp_path(&options.output, "audio.wav"))
    };

    let result = encode_video_and_audio(
        movie,
        &mut console,
        options,
        frames,
        &video_path,
        audio_path.as_deref(),
    );
    if let Err(error) = result {
        let _ = fs::remove_file(&video_path);
        if options.audio_output.is_none() {
            if let Some(path) = &audio_path {
                let _ = fs::remove_file(path);
            }
        }
        return Err(error);
    }

    if let Some(audio_path) = audio_path {
        mux_audio(&video_path, &audio_path, &options.output)?;
        fs::remove_file(&video_path)
            .map_err(|error| ExportError::io("remove temporary video", error))?;
        if options.audio_output.is_none() {
            fs::remove_file(&audio_path)
                .map_err(|error| ExportError::io("remove temporary audio", error))?;
        }
    }

    let (width, height) = {
        let output = console.next_frame();
        (output.width, output.height)
    };
    let audio_samples = if options.video_only {
        0
    } else {
        target_audio_samples(frames, movie.frame_rate)
    };
    Ok(ExportReport {
        frames,
        width,
        height,
        audio_samples,
    })
}

fn encode_video_and_audio(
    movie: &LsmvMovie,
    console: &mut Console,
    options: &ExportOptions,
    frames: usize,
    video_path: &Path,
    audio_path: Option<&Path>,
) -> Result<(), ExportError> {
    let mut child: Option<Child> = None;
    let mut video_input: Option<BufWriter<ChildStdin>> = None;
    let mut dimensions = None;
    let mut audio = audio_path.map(WavWriter::create).transpose()?;
    let mut audio_written = 0usize;

    for (index, input) in movie.frames.iter().take(frames).enumerate() {
        console.set_controller_buttons(ControllerPort::Port1, input.port1);
        console.set_controller_buttons(ControllerPort::Port2, input.port2);
        console.run_for_frames(1);

        let (width, height, pixels) = {
            let frame = console.next_frame();
            (frame.width, frame.height, frame.pixels.to_vec())
        };
        let (expected_width, expected_height) = dimensions.get_or_insert((width, height));
        if *expected_width != width || *expected_height != height {
            return Err(ExportError::new(format!(
                "replay changed video dimensions from {expected_width}x{expected_height} to {width}x{height} at frame {}",
                index + 1
            )));
        }

        if child.is_none() {
            let (child_process, stdin) =
                spawn_video_encoder(video_path, width, height, options.scale, movie.frame_rate)?;
            child = Some(child_process);
            video_input = Some(BufWriter::with_capacity(1024 * 1024, stdin));
        }

        let video = video_input
            .as_mut()
            .expect("video input initialized before writing");
        write_rgb24(
            video,
            &pixels,
            width as usize,
            height as usize,
            options.scale,
        )
        .map_err(|error| ExportError::io("write FFmpeg video frame", error))?;

        if let Some(directory) = &options.sample_dir {
            if (index + 1) % options.sample_every == 0 || index + 1 == frames {
                let path = directory.join(format!("frame-{:06}.ppm", index + 1));
                write_ppm(&path, width, height, &pixels)?;
            }
        }

        if let Some(audio) = audio.as_mut() {
            let target = target_audio_samples(index + 1, movie.frame_rate);
            let sample_count = target.saturating_sub(audio_written);
            let samples = console.render_audio(sample_count);
            audio.write_samples(&samples)?;
            audio_written += samples.len();
        }
    }

    if let Some(mut input) = video_input {
        input
            .flush()
            .map_err(|error| ExportError::io("flush FFmpeg video", error))?;
    }
    if let Some(audio) = audio {
        audio.finish()?;
    }
    let Some(child) = child else {
        return Err(ExportError::new("FFmpeg encoder received no frames"));
    };
    wait_for_ffmpeg(child)
}

fn spawn_video_encoder(
    output: &Path,
    width: u16,
    height: u16,
    scale: u32,
    frame_rate: FrameRate,
) -> Result<(Child, ChildStdin), ExportError> {
    let width = u32::from(width)
        .checked_mul(scale)
        .ok_or_else(|| ExportError::new("scaled video width is too large"))?;
    let height = u32::from(height)
        .checked_mul(scale)
        .ok_or_else(|| ExportError::new("scaled video height is too large"))?;
    let mut command = Command::new("ffmpeg");
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-f")
        .arg("rawvideo")
        .arg("-pix_fmt")
        .arg("rgb24")
        .arg("-s:v")
        .arg(format!("{width}x{height}"))
        .arg("-framerate")
        .arg(frame_rate.as_ffmpeg_value())
        .arg("-i")
        .arg("pipe:0")
        .arg("-an")
        .arg("-c:v")
        .arg("libx264")
        .arg("-preset")
        .arg("ultrafast")
        .arg("-crf")
        .arg("18")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-movflags")
        .arg("+faststart")
        .arg(output)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| ExportError::io("start FFmpeg", error))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| ExportError::new("FFmpeg video input pipe was not available"))?;
    Ok((child, stdin))
}

fn wait_for_ffmpeg(child: Child) -> Result<(), ExportError> {
    let output = child
        .wait_with_output()
        .map_err(|error| ExportError::io("wait for FFmpeg", error))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(ExportError::new(if detail.is_empty() {
        format!("FFmpeg failed ({})", output.status)
    } else {
        format!("FFmpeg failed: {detail}")
    }))
}

fn mux_audio(video: &Path, audio: &Path, output: &Path) -> Result<(), ExportError> {
    let result = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(video)
        .arg("-i")
        .arg(audio)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("1:a:0")
        .arg("-c:v")
        .arg("copy")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("192k")
        .arg("-af")
        .arg("apad")
        .arg("-shortest")
        .arg("-movflags")
        .arg("+faststart")
        .arg(output)
        .output()
        .map_err(|error| ExportError::io("start FFmpeg audio mux", error))?;
    if result.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&result.stderr).trim().to_owned();
    Err(ExportError::new(if detail.is_empty() {
        format!("FFmpeg audio mux failed ({})", result.status)
    } else {
        format!("FFmpeg audio mux failed: {detail}")
    }))
}

fn target_audio_samples(frames: usize, frame_rate: FrameRate) -> usize {
    ((frames as u128 * u128::from(AUDIO_SAMPLE_RATE) * u128::from(frame_rate.denominator))
        / u128::from(frame_rate.numerator)) as usize
}

fn write_rgb24<W: Write>(
    writer: &mut W,
    pixels: &[u32],
    width: usize,
    height: usize,
    scale: u32,
) -> io::Result<()> {
    if pixels.len() != width * height {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame pixel count does not match dimensions",
        ));
    }
    let scale = scale as usize;
    for row in pixels.chunks_exact(width) {
        let mut expanded = Vec::with_capacity(width * scale * 3);
        for &pixel in row {
            let rgb = [(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8];
            for _ in 0..scale {
                expanded.extend_from_slice(&rgb);
            }
        }
        for _ in 0..scale {
            writer.write_all(&expanded)?;
        }
    }
    Ok(())
}

fn write_ppm(path: &Path, width: u16, height: u16, pixels: &[u32]) -> Result<(), ExportError> {
    let mut file = BufWriter::new(
        File::create(path).map_err(|error| ExportError::io("create PPM sample", error))?,
    );
    writeln!(file, "P6\n{} {}\n255", width, height)
        .map_err(|error| ExportError::io("write PPM header", error))?;
    for &pixel in pixels {
        file.write_all(&[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8])
            .map_err(|error| ExportError::io("write PPM sample", error))?;
    }
    file.flush()
        .map_err(|error| ExportError::io("flush PPM sample", error))?;
    Ok(())
}

struct WavWriter {
    file: BufWriter<File>,
    data_bytes: u32,
}

impl WavWriter {
    fn create(path: &Path) -> Result<Self, ExportError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .map_err(|error| ExportError::io("create WAV", error))?;
        let mut file = BufWriter::with_capacity(64 * 1024, file);
        file.write_all(&[0; 44])
            .map_err(|error| ExportError::io("reserve WAV header", error))?;
        Ok(Self {
            file,
            data_bytes: 0,
        })
    }

    fn write_samples(&mut self, samples: &[AudioSample]) -> Result<(), ExportError> {
        let byte_count = samples
            .len()
            .checked_mul(4)
            .ok_or_else(|| ExportError::new("WAV sample batch is too large"))?;
        let next_size = self
            .data_bytes
            .checked_add(byte_count as u32)
            .ok_or_else(|| ExportError::new("WAV audio exceeds the 4 GiB limit"))?;
        for sample in samples {
            self.file
                .write_all(&sample.left.to_le_bytes())
                .and_then(|_| self.file.write_all(&sample.right.to_le_bytes()))
                .map_err(|error| ExportError::io("write WAV samples", error))?;
        }
        self.data_bytes = next_size;
        Ok(())
    }

    fn finish(mut self) -> Result<(), ExportError> {
        self.file
            .flush()
            .map_err(|error| ExportError::io("flush WAV", error))?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|error| ExportError::io("seek WAV header", error))?;
        let riff_size = 36u32
            .checked_add(self.data_bytes)
            .ok_or_else(|| ExportError::new("WAV RIFF is too large"))?;
        let mut header = Vec::with_capacity(44);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&riff_size.to_le_bytes());
        header.extend_from_slice(b"WAVEfmt ");
        header.extend_from_slice(&16u32.to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&2u16.to_le_bytes());
        header.extend_from_slice(&AUDIO_SAMPLE_RATE.to_le_bytes());
        header.extend_from_slice(&(AUDIO_SAMPLE_RATE * 4).to_le_bytes());
        header.extend_from_slice(&4u16.to_le_bytes());
        header.extend_from_slice(&16u16.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&self.data_bytes.to_le_bytes());
        self.file
            .write_all(&header)
            .and_then(|_| self.file.flush())
            .map_err(|error| ExportError::io("finish WAV header", error))
    }
}

fn ensure_parent(path: &Path) -> Result<(), ExportError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(ExportError::new(format!(
                "output directory does not exist: {}",
                parent.display()
            )));
        }
    }
    Ok(())
}

fn adjacent_temp_path(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("snes-replay");
    let temp_name = format!(".{name}.{}.{}", std::process::id(), suffix);
    path.with_file_name(temp_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_sample_accumulator_matches_ntsc_duration() {
        let samples = target_audio_samples(60, FrameRate::NTSC);
        assert_eq!(samples, 31_947);
    }

    #[test]
    fn rgb_writer_scales_argb_pixels_nearest_neighbor() {
        let mut output = Vec::new();
        write_rgb24(&mut output, &[0xff12_3456], 1, 1, 2).unwrap();
        assert_eq!(
            output,
            vec![0x12, 0x34, 0x56, 0x12, 0x34, 0x56, 0x12, 0x34, 0x56, 0x12, 0x34, 0x56]
        );
    }
}
