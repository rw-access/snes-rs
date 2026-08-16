use std::env;
use std::fs;
use std::time::{Duration, Instant};

use sdl2::audio::{AudioQueue, AudioSpecDesired};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::PixelFormatEnum;
use snes_core::{
    Console, ControllerPort, RewindBuffer, RomImage, AUDIO_SAMPLE_RATE, MASTER_TICKS_PER_FRAME,
};

const SCALE: u32 = 3;
const B: u16 = 1 << 0;
const Y: u16 = 1 << 1;
const SELECT: u16 = 1 << 2;
const START: u16 = 1 << 3;
const UP: u16 = 1 << 4;
const DOWN: u16 = 1 << 5;
const LEFT: u16 = 1 << 6;
const RIGHT: u16 = 1 << 7;
const A: u16 = 1 << 8;
const X: u16 = 1 << 9;
const L: u16 = 1 << 10;
const R: u16 = 1 << 11;

fn button(key: Keycode) -> Option<u16> {
    Some(match key {
        Keycode::W | Keycode::Up => UP,
        Keycode::S | Keycode::Down => DOWN,
        Keycode::A | Keycode::Left => LEFT,
        Keycode::D | Keycode::Right => RIGHT,
        Keycode::J | Keycode::Z => B,
        Keycode::K | Keycode::X => A,
        Keycode::U => Y,
        Keycode::I => X,
        Keycode::O => L,
        Keycode::P => R,
        Keycode::Comma | Keycode::LShift => SELECT,
        Keycode::Period | Keycode::Return => START,
        _ => return None,
    })
}

fn audio_block(console: &mut Console, frame: u64, emitted: &mut u64) -> Vec<f32> {
    let target = frame * u64::from(AUDIO_SAMPLE_RATE) / 60;
    let count = target.saturating_sub(*emitted) as usize;
    *emitted = target;
    let samples = console.render_audio(count);
    let mut output = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        output.push(f32::from(sample.left) / 32_768.0);
        output.push(f32::from(sample.right) / 32_768.0);
    }
    output
}

fn main() -> Result<(), String> {
    let mut args = env::args_os();
    let _program = args.next();
    let path = args.next().ok_or("usage: snes-sdl2 ROM")?;
    if args.next().is_some() {
        return Err("usage: snes-sdl2 ROM".into());
    }
    let path = std::path::PathBuf::from(path);
    let image =
        RomImage::load(fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?)
            .map_err(|error| format!("{}: {error:?}", path.display()))?;
    let mut console =
        Console::new(image).map_err(|error| format!("could not create console: {error:?}"))?;
    console.reset();
    let sdl = sdl2::init().map_err(|error| error.to_string())?;
    let video = sdl.video().map_err(|error| error.to_string())?;
    let audio = sdl.audio().map_err(|error| error.to_string())?;
    let window = video
        .window("snes-rs", 256 * SCALE, 224 * SCALE)
        .position_centered()
        .resizable()
        .build()
        .map_err(|error| error.to_string())?;
    let mut canvas = window
        .into_canvas()
        .accelerated()
        .present_vsync()
        .build()
        .map_err(|error| error.to_string())?;
    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture_streaming(PixelFormatEnum::RGB24, 256, 224)
        .map_err(|error| error.to_string())?;
    let desired = AudioSpecDesired {
        freq: Some(AUDIO_SAMPLE_RATE as i32),
        channels: Some(2),
        samples: Some(1024),
    };
    let audio_queue: AudioQueue<f32> = audio
        .open_queue(None, &desired)
        .map_err(|error| error.to_string())?;
    audio_queue.resume();
    let mut events = sdl.event_pump().map_err(|error| error.to_string())?;
    let mut rewind = RewindBuffer::new(600);
    let mut buttons = 0u16;
    let mut rewinding = false;
    let mut frame_number = 0u64;
    let mut audio_emitted = 0u64;
    let mut last_frame = Instant::now();
    'running: loop {
        for event in events.poll_iter() {
            match event {
                Event::Quit { .. } => break 'running,
                Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    repeat: false,
                    ..
                } => break 'running,
                Event::KeyDown {
                    keycode: Some(Keycode::R),
                    repeat: false,
                    ..
                } => {
                    rewinding = true;
                    audio_queue.clear();
                }
                Event::KeyUp {
                    keycode: Some(Keycode::R),
                    ..
                } => rewinding = false,
                Event::KeyDown {
                    keycode: Some(key),
                    repeat: false,
                    ..
                } => {
                    if let Some(mask) = button(key) {
                        buttons |= mask;
                    }
                }
                Event::KeyUp {
                    keycode: Some(key), ..
                } => {
                    if let Some(mask) = button(key) {
                        buttons &= !mask;
                    }
                }
                Event::Window {
                    win_event: sdl2::event::WindowEvent::FocusLost,
                    ..
                } => {
                    buttons = 0;
                    rewinding = false;
                }
                _ => {}
            }
        }
        if last_frame.elapsed() >= Duration::from_nanos(1_000_000_000 / 60) {
            last_frame = Instant::now();
            if rewinding {
                if rewind.rewind(&mut console).unwrap_or(false) {
                    frame_number = frame_number.saturating_sub(1);
                    audio_emitted = frame_number * u64::from(AUDIO_SAMPLE_RATE) / 60;
                }
            } else {
                rewind.capture(&console);
                console.set_controller_buttons(ControllerPort::Port1, buttons);
                console.run_for_master_ticks(MASTER_TICKS_PER_FRAME);
                frame_number = frame_number.saturating_add(1);
                audio_queue
                    .queue_audio(&audio_block(&mut console, frame_number, &mut audio_emitted))
                    .map_err(|error| error.to_string())?;
            }

            let frame = console.next_frame();
            let width = frame.width as u32;
            let height = frame.height as u32;
            if texture.query().width != width || texture.query().height != height {
                texture = texture_creator
                    .create_texture_streaming(PixelFormatEnum::RGB24, width, height)
                    .map_err(|error| error.to_string())?;
            }
            let mut rgb = vec![0u8; (width * height * 3) as usize];
            for (index, pixel) in frame.pixels.iter().copied().enumerate() {
                rgb[index * 3] = (pixel >> 16) as u8;
                rgb[index * 3 + 1] = (pixel >> 8) as u8;
                rgb[index * 3 + 2] = pixel as u8;
            }
            texture
                .update(None, &rgb, width as usize * 3)
                .map_err(|error| error.to_string())?;
            canvas.clear();
            canvas
                .copy(&texture, None, None)
                .map_err(|error| error.to_string())?;
            canvas.present();
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    Ok(())
}
