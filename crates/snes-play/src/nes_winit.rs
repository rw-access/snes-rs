use std::collections::VecDeque;
use std::env;
use std::error::Error;
use std::fs;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, StreamConfig};
use snes_core::{
    Console, ControllerPort, RewindBuffer, RomImage, AUDIO_SAMPLE_RATE, MASTER_TICKS_PER_FRAME,
};
use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

const SCALE: usize = 3;
const WINDOW_WIDTH: u32 = 256 * SCALE as u32;
const WINDOW_HEIGHT: u32 = 224 * SCALE as u32;
const FRAME_DURATION: Duration = Duration::from_nanos(1_000_000_000 / 60);
const AUDIO_QUEUE_CAPACITY: usize = AUDIO_SAMPLE_RATE as usize * 2;

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

struct AudioQueue {
    samples: Mutex<VecDeque<f32>>,
    capacity: usize,
}

impl AudioQueue {
    fn new(capacity: usize) -> Self {
        Self {
            samples: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    fn enqueue(&self, stereo: &[f32]) {
        let Ok(mut queue) = self.samples.lock() else {
            return;
        };
        let overflow = queue
            .len()
            .saturating_add(stereo.len())
            .saturating_sub(self.capacity);
        for _ in 0..overflow {
            queue.pop_front();
        }
        queue.extend(stereo.iter().copied().take(self.capacity - queue.len()));
    }

    fn flush(&self) {
        if let Ok(mut queue) = self.samples.lock() {
            queue.clear();
        }
    }

    fn fill(&self, output: &mut [f32], channels: usize) {
        if let Ok(mut queue) = self.samples.lock() {
            for frame in output.chunks_mut(channels) {
                let left = queue.pop_front().unwrap_or(0.0);
                let right = queue.pop_front().unwrap_or(left);
                if let Some(channel) = frame.get_mut(0) {
                    *channel = left;
                }
                for channel in frame.iter_mut().skip(1) {
                    *channel = right;
                }
            }
        } else {
            output.fill(0.0);
        }
    }
}

struct AudioOutput {
    queue: Arc<AudioQueue>,
    _stream: cpal::Stream,
}

impl AudioOutput {
    fn new() -> Result<Self, Box<dyn Error>> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no default audio output device")?;
        let wanted = SampleRate(AUDIO_SAMPLE_RATE);
        let supported = device
            .supported_output_configs()?
            .find(|config| {
                config.channels() >= 1
                    && config.sample_format() == SampleFormat::F32
                    && config.min_sample_rate() <= wanted
                    && wanted <= config.max_sample_rate()
            })
            .ok_or("no supported 32 kHz f32 output configuration")?;
        let config: StreamConfig = supported.with_sample_rate(wanted).config();
        let channels = config.channels as usize;
        let queue = Arc::new(AudioQueue::new(AUDIO_QUEUE_CAPACITY));
        let callback_queue = Arc::clone(&queue);
        let stream = device.build_output_stream::<f32, _, _>(
            &config,
            move |output, _| callback_queue.fill(output, channels),
            |error| eprintln!("audio stream error: {error}"),
            None,
        )?;
        stream.play()?;
        Ok(Self {
            queue,
            _stream: stream,
        })
    }
}

struct RunningWindow {
    window: Rc<Window>,
    surface: Option<Surface<OwnedDisplayHandle, Rc<Window>>>,
}

struct App {
    context: Context<OwnedDisplayHandle>,
    audio: AudioOutput,
    console: Console,
    rewind: RewindBuffer,
    buttons: u16,
    rewinding: bool,
    window: Option<RunningWindow>,
    pixels: Vec<u32>,
    width: usize,
    height: usize,
    frame_number: u64,
    audio_samples_emitted: u64,
    next_frame_at: Instant,
    fps_throttled: bool,
}

impl App {
    fn new(context: Context<OwnedDisplayHandle>, console: Console, audio: AudioOutput) -> Self {
        Self {
            context,
            audio,
            console,
            rewind: RewindBuffer::new(600),
            buttons: 0,
            rewinding: false,
            window: None,
            pixels: Vec::new(),
            width: 256,
            height: 224,
            frame_number: 0,
            audio_samples_emitted: 0,
            next_frame_at: Instant::now(),
            fps_throttled: true,
        }
    }

    fn set_rewinding(&mut self, rewinding: bool) {
        if self.rewinding == rewinding {
            return;
        }
        self.rewinding = rewinding;
        self.audio.queue.flush();
        if !rewinding {
            self.console
                .set_controller_buttons(ControllerPort::Port1, self.buttons);
        }
    }

    fn advance_frame(&mut self) {
        if self.rewinding {
            if self.rewind.rewind(&mut self.console).unwrap_or(false) {
                self.frame_number = self.frame_number.saturating_sub(1);
                self.audio_samples_emitted = self.frame_number * u64::from(AUDIO_SAMPLE_RATE) / 60;
            }
        } else {
            self.rewind.capture(&self.console);
            self.console
                .set_controller_buttons(ControllerPort::Port1, self.buttons);
            self.console.run_for_master_ticks(MASTER_TICKS_PER_FRAME);
            self.frame_number = self.frame_number.saturating_add(1);
            let target = self.frame_number * u64::from(AUDIO_SAMPLE_RATE) / 60;
            let count = target.saturating_sub(self.audio_samples_emitted) as usize;
            self.audio_samples_emitted = target;
            let samples = self.console.render_audio(count);
            let mut stereo = Vec::with_capacity(samples.len() * 2);
            for sample in samples {
                stereo.push(f32::from(sample.left) / 32_768.0);
                stereo.push(f32::from(sample.right) / 32_768.0);
            }
            self.audio.queue.enqueue(&stereo);
        }

        let frame = self.console.next_frame();
        self.width = frame.width as usize;
        self.height = frame.height as usize;
        self.pixels.clear();
        self.pixels.extend_from_slice(frame.pixels);

        self.next_frame_at = self
            .next_frame_at
            .checked_add(FRAME_DURATION)
            .unwrap_or_else(Instant::now);
        if self.next_frame_at < Instant::now() {
            self.next_frame_at = Instant::now();
        }
    }

    fn render(&mut self) {
        let Some(running) = self.window.as_mut() else {
            return;
        };
        let Some(surface) = running.surface.as_mut() else {
            return;
        };
        let size = running.window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        if let Err(error) = surface.resize(width, height) {
            eprintln!("failed to resize surface: {error}");
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };
        let output_width = buffer.width().get() as usize;
        let output_height = buffer.height().get() as usize;
        buffer.fill(0);
        if self.width == 0 || self.height == 0 {
            return;
        }
        let scale = SCALE
            .min(output_width / self.width)
            .min(output_height / self.height)
            .max(1);
        let offset_x = output_width.saturating_sub(self.width * scale) / 2;
        let offset_y = output_height.saturating_sub(self.height * scale) / 2;
        for y in 0..self.height {
            for x in 0..self.width {
                let color = self.pixels.get(y * self.width + x).copied().unwrap_or(0);
                for dy in 0..scale {
                    let oy = offset_y + y * scale + dy;
                    if oy >= output_height {
                        continue;
                    }
                    for dx in 0..scale {
                        let ox = offset_x + x * scale + dx;
                        if ox < output_width {
                            buffer[oy * output_width + ox] = color;
                        }
                    }
                }
            }
        }
        let _ = buffer.present();
    }

    fn set_button(&mut self, mask: u16, pressed: bool) {
        if pressed {
            self.buttons |= mask;
        } else {
            self.buttons &= !mask;
        }
        if !self.rewinding {
            self.console
                .set_controller_buttons(ControllerPort::Port1, self.buttons);
        }
    }

    fn handle_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        code: KeyCode,
        state: ElementState,
        repeat: bool,
    ) {
        let pressed = state == ElementState::Pressed;
        if pressed && code == KeyCode::Escape {
            event_loop.exit();
            return;
        }
        if code == KeyCode::KeyR {
            self.set_rewinding(pressed);
            return;
        }
        if pressed && !repeat && code == KeyCode::KeyF {
            self.fps_throttled = !self.fps_throttled;
            return;
        }
        let mask = match code {
            KeyCode::KeyW | KeyCode::ArrowUp => Some(UP),
            KeyCode::KeyS | KeyCode::ArrowDown => Some(DOWN),
            KeyCode::KeyA | KeyCode::ArrowLeft => Some(LEFT),
            KeyCode::KeyD | KeyCode::ArrowRight => Some(RIGHT),
            KeyCode::KeyJ | KeyCode::KeyZ => Some(B),
            KeyCode::KeyK | KeyCode::KeyX => Some(A),
            KeyCode::Comma | KeyCode::ShiftLeft => Some(SELECT),
            KeyCode::Period | KeyCode::Enter => Some(START),
            KeyCode::KeyU => Some(Y),
            KeyCode::KeyI => Some(X),
            KeyCode::KeyO => Some(L),
            KeyCode::KeyP => Some(R),
            _ => None,
        };
        if let Some(mask) = mask {
            self.set_button(mask, pressed);
        }
    }
}

impl ApplicationHandler for App {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::Init) {
            self.next_frame_at = Instant::now();
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let attributes = Window::default_attributes()
                .with_title("snes-rs")
                .with_inner_size(winit::dpi::PhysicalSize::new(WINDOW_WIDTH, WINDOW_HEIGHT))
                .with_min_inner_size(winit::dpi::PhysicalSize::new(WINDOW_WIDTH, WINDOW_HEIGHT))
                .with_resizable(false);
            let window = Rc::new(
                event_loop
                    .create_window(attributes)
                    .expect("failed to create window"),
            );
            self.window = Some(RunningWindow {
                window,
                surface: None,
            });
        }
        let window = Rc::clone(&self.window.as_ref().expect("window was created").window);
        let surface = Surface::new(&self.context, window).expect("failed to create surface");
        self.window.as_mut().expect("window was created").surface = Some(surface);
        self.window
            .as_ref()
            .expect("window was created")
            .window
            .request_redraw();
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_mut() {
            window.surface = None;
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if !matches!(self.window.as_ref(), Some(window) if window.window.id() == window_id) {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                if !self.fps_throttled || Instant::now() >= self.next_frame_at {
                    self.advance_frame();
                }
                self.render();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    self.handle_key(event_loop, code, event.state, event.repeat);
                }
            }
            WindowEvent::Focused(false) => {
                self.buttons = 0;
                self.set_rewinding(false);
                self.console
                    .set_controller_buttons(ControllerPort::Port1, 0);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.fps_throttled {
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame_at));
            if Instant::now() >= self.next_frame_at {
                if let Some(window) = &self.window {
                    window.window.request_redraw();
                }
            }
        } else {
            event_loop.set_control_flow(ControlFlow::Poll);
            if let Some(window) = &self.window {
                window.window.request_redraw();
            }
        }
    }
}

fn load_console(path: &PathBuf) -> Result<Console, Box<dyn Error>> {
    let image = RomImage::load(fs::read(path)?)?;
    let mut console = Console::new(image)?;
    console.reset();
    Ok(console)
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os();
    let program = args.next().unwrap_or_else(|| "snes-winit".into());
    let path = args
        .next()
        .ok_or_else(|| format!("usage: {} ROM", PathBuf::from(program).display()))?;
    if args.next().is_some() {
        return Err("usage: snes-winit ROM".into());
    }
    let event_loop = EventLoop::new()?;
    let context = Context::new(event_loop.owned_display_handle())?;
    let audio = AudioOutput::new()?;
    let mut app = App::new(context, load_console(&PathBuf::from(path))?, audio);
    event_loop.run_app(&mut app)?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("snes-winit: {error}");
        std::process::exit(1);
    }
}
