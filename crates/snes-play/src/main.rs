//! Dependency-light command-line frontend for bounded SNES runs.

use std::{env, fs, path::PathBuf, process::ExitCode};

use snes_core::{Console, FrameOutput, RomImage, MASTER_TICKS_PER_FRAME};

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("snes-play: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return if args.is_empty() {
            Err("a ROM path is required".to_owned())
        } else {
            Ok(())
        };
    }

    let rom_path = PathBuf::from(&args[0]);
    let mut frames = None;
    let mut ticks = None;
    let mut ppm = None;
    let mut index = 1;
    while index < args.len() {
        let flag = &args[index];
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--frames" => {
                if ticks.is_some() {
                    return Err("--frames and --ticks are mutually exclusive".to_owned());
                }
                frames = Some(parse_u64(value, "frames")?);
            }
            "--ticks" => {
                if frames.is_some() {
                    return Err("--frames and --ticks are mutually exclusive".to_owned());
                }
                ticks = Some(parse_u64(value, "ticks")?);
            }
            "--ppm" => ppm = Some(PathBuf::from(value)),
            other => return Err(format!("unknown option {other}")),
        }
        index += 2;
    }

    let bytes = fs::read(&rom_path).map_err(|error| format!("{}: {error}", rom_path.display()))?;
    let rom =
        RomImage::load(bytes).map_err(|error| format!("{}: {error:?}", rom_path.display()))?;
    let mut console =
        Console::new(rom).map_err(|error| format!("unable to create console: {error:?}"))?;
    console.reset();
    if let Some(frames) = frames {
        console.run_for_frames(frames);
    } else {
        console.run_for_master_ticks(ticks.unwrap_or(MASTER_TICKS_PER_FRAME));
    }

    let (width, height) = {
        let frame = console.next_frame();
        if let Some(path) = ppm {
            write_ppm(&path, &frame)?;
            println!("frame={}", path.display());
        }
        (frame.width, frame.height)
    };
    println!(
        "ticks={} stopped={} dimensions={}x{}",
        console.master_ticks(),
        console.is_stopped(),
        width,
        height
    );
    Ok(())
}

fn parse_u64(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("{name} must be an unsigned integer"))
}

fn write_ppm(path: &PathBuf, frame: &FrameOutput<'_>) -> Result<(), String> {
    let mut bytes = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
    for pixel in frame.pixels {
        bytes.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8]);
    }
    fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))
}

fn print_help() {
    println!(
        "usage: snes-play ROM [--frames N | --ticks N] [--ppm PATH]\n\n\
         Runs a ROM for a bounded interval (one frame by default) and optionally\n\
         writes the current native framebuffer as a binary PPM image; ordinary\n\
         modes are 256x224, while Modes 5/6 may use native 512x224 dimensions."
    );
}
