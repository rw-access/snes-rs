//! Replay an lsnes SNES movie into an MP4 and optional inspection artifacts.

use snes_render::{export, LsmvMovie};
use std::{env, fs, path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lsmv_to_mp4: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return if args.is_empty() {
            Err("an LSMV movie path is required".to_owned())
        } else {
            Ok(())
        };
    }
    let movie_path = PathBuf::from(&args[0]);
    let rom_path = args
        .get(1)
        .map(PathBuf::from)
        .ok_or_else(|| "a ROM path is required after the movie path".to_owned())?;
    let mut output = None;
    let mut frame_limit = None;
    let mut scale = 1u32;
    let mut video_only = false;
    let mut sample_dir = None;
    let mut sample_every = 600usize;
    let mut audio_output = None;
    let mut ignore_rom_checksum = false;
    let mut index = 2;
    while index < args.len() {
        let flag = &args[index];
        if flag == "--video-only" {
            video_only = true;
            index += 1;
            continue;
        }
        if flag == "--ignore-rom-checksum" {
            ignore_rom_checksum = true;
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--output" => output = Some(PathBuf::from(value)),
            "--frames" => frame_limit = Some(parse_usize(value, "frames")?),
            "--scale" => scale = parse_u32(value, "scale")?,
            "--sample-dir" => sample_dir = Some(PathBuf::from(value)),
            "--sample-every" => sample_every = parse_usize(value, "sample-every")?,
            "--audio-wav" => audio_output = Some(PathBuf::from(value)),
            other => return Err(format!("unknown option {other}")),
        }
        index += 2;
    }
    let output = output.ok_or_else(|| "--output is required".to_owned())?;
    let movie = LsmvMovie::read(&movie_path).map_err(|error| error.to_string())?;
    let rom = fs::read(&rom_path).map_err(|error| format!("{}: {error}", rom_path.display()))?;
    if !ignore_rom_checksum {
        movie
            .verify_rom_checksum(&rom)
            .map_err(|error| format!("{}: {error}", movie_path.display()))?;
    }
    let report = export::export_movie(
        &movie,
        &rom,
        &export::ExportOptions {
            output: output.clone(),
            frame_limit,
            scale,
            video_only,
            sample_dir,
            sample_every,
            audio_output,
        },
    )
    .map_err(|error| error.to_string())?;
    println!(
        "wrote {} ({} frames, {}x{}, {} audio samples)",
        output.display(),
        report.frames,
        report.width,
        report.height,
        report.audio_samples
    );
    Ok(())
}

fn parse_usize(value: &str, name: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("{name} must be an unsigned integer"))
}

fn parse_u32(value: &str, name: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("{name} must be an unsigned integer"))
}

fn print_help() {
    println!(
        "usage: lsmv_to_mp4 MOVIE.lsmv ROM.sfc --output OUTPUT.mp4 [options]\n\n\
         options:\n\
           --frames N          replay only the first N frames\n\
           --scale N           nearest-neighbor output scale (default 1)\n\
           --video-only        omit the emulator audio stream\n\
           --ignore-rom-checksum bypass the LSMV/ROM identity check\n\
           --sample-dir DIR    write sampled PPM frames for visual inspection\n\
           --sample-every N    sample every Nth frame (default 600)\n\
           --audio-wav PATH    retain the generated stereo PCM WAV\n"
    );
}
