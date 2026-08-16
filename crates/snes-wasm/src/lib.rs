use js_sys::{Float32Array, Uint8Array};
use snes_core::ppu::{SCREEN_HEIGHT, SCREEN_WIDTH};
use snes_core::{
    Console, ConsoleState, ControllerPort, RewindBuffer, RomImage, AUDIO_SAMPLE_RATE,
    MASTER_TICKS_PER_FRAME,
};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen]
#[derive(Clone, Copy)]
pub struct FrameMetadata {
    frame_number: u64,
    width: u32,
    height: u32,
    audio_sample_rate: u32,
    audio_samples: usize,
    audio_discontinuity: bool,
}

#[wasm_bindgen]
impl FrameMetadata {
    #[wasm_bindgen(getter)]
    pub fn frame_number(&self) -> u64 {
        self.frame_number
    }
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.width
    }
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.height
    }
    #[wasm_bindgen(getter)]
    pub fn audio_sample_rate(&self) -> u32 {
        self.audio_sample_rate
    }
    #[wasm_bindgen(getter)]
    pub fn audio_samples(&self) -> usize {
        self.audio_samples
    }
    #[wasm_bindgen(getter)]
    pub fn audio_discontinuity(&self) -> bool {
        self.audio_discontinuity
    }
}

#[wasm_bindgen]
pub struct Snapshot {
    state: ConsoleState,
    controller_bits: u16,
    frame_number: u64,
    audio_samples_emitted: u64,
}

#[wasm_bindgen]
pub struct SnesWeb {
    console: Console,
    rewind: RewindBuffer,
    rgba: Vec<u8>,
    rgba_js: Uint8Array,
    audio: Vec<f32>,
    audio_js: Float32Array,
    controller_bits: u16,
    frame_number: u64,
    audio_samples_emitted: u64,
    last_frame: FrameMetadata,
}

#[wasm_bindgen]
impl SnesWeb {
    #[wasm_bindgen(constructor)]
    pub fn new(rom: &[u8]) -> Result<SnesWeb, JsValue> {
        let image = RomImage::load(rom.to_vec())
            .map_err(|error| JsValue::from_str(&format!("invalid SNES ROM: {error:?}")))?;
        let mut console = Console::new(image)
            .map_err(|error| JsValue::from_str(&format!("could not create console: {error:?}")))?;
        console.reset();
        Ok(Self {
            console,
            rewind: RewindBuffer::new(600),
            rgba: Vec::new(),
            rgba_js: Uint8Array::new_with_length(0),
            audio: Vec::new(),
            audio_js: Float32Array::new_with_length(0),
            controller_bits: 0,
            frame_number: 0,
            audio_samples_emitted: 0,
            last_frame: FrameMetadata {
                frame_number: 0,
                width: SCREEN_WIDTH as u32,
                height: SCREEN_HEIGHT as u32,
                audio_sample_rate: AUDIO_SAMPLE_RATE,
                audio_samples: 0,
                audio_discontinuity: false,
            },
        })
    }

    pub fn load_rom(rom: &[u8]) -> Result<SnesWeb, JsValue> {
        Self::new(rom)
    }

    /// Advance one NTSC frame and refresh reusable JS-owned buffers.
    pub fn step_frame(&mut self) -> FrameMetadata {
        self.rewind.capture(&self.console);
        self.console.run_for_master_ticks(MASTER_TICKS_PER_FRAME);
        self.frame_number = self.frame_number.saturating_add(1);
        self.refresh(false)
    }

    /// Restore the previous captured frame without advancing emulation.
    pub fn rewind(&mut self) -> bool {
        let Ok(restored) = self.rewind.rewind(&mut self.console) else {
            return false;
        };
        if restored {
            self.frame_number = self.frame_number.saturating_sub(1);
            self.audio_samples_emitted = audio_target_for_frame(self.frame_number);
            self.refresh(true);
        }
        restored
    }

    pub fn frame_metadata(&self) -> FrameMetadata {
        self.last_frame
    }
    pub fn rgba_buffer(&self) -> Uint8Array {
        self.rgba_js.clone()
    }
    pub fn audio_buffer(&self) -> Float32Array {
        self.audio_js.clone()
    }

    pub fn set_controller(&mut self, buttons: u16) {
        self.controller_bits = buttons;
        self.console
            .set_controller_buttons(ControllerPort::Port1, buttons);
    }

    pub fn controller_bits(&self) -> u16 {
        self.controller_bits
    }

    pub fn save_snapshot(&self) -> Snapshot {
        Snapshot {
            state: self.console.snapshot(),
            controller_bits: self.controller_bits,
            frame_number: self.frame_number,
            audio_samples_emitted: self.audio_samples_emitted,
        }
    }

    pub fn restore_snapshot(&mut self, snapshot: &Snapshot) -> Result<(), JsValue> {
        self.console
            .restore(&snapshot.state)
            .map_err(|error| JsValue::from_str(&format!("could not restore state: {error:?}")))?;
        self.controller_bits = snapshot.controller_bits;
        self.frame_number = snapshot.frame_number;
        self.audio_samples_emitted = snapshot.audio_samples_emitted;
        self.rewind.clear();
        self.console
            .set_controller_buttons(ControllerPort::Port1, self.controller_bits);
        self.refresh(true);
        Ok(())
    }

    pub fn width(&self) -> u32 {
        self.last_frame.width
    }
    pub fn height(&self) -> u32 {
        self.last_frame.height
    }
    pub fn audio_sample_rate(&self) -> u32 {
        AUDIO_SAMPLE_RATE
    }

    fn refresh(&mut self, discontinuity: bool) -> FrameMetadata {
        let (frame_width, frame_height) = {
            let frame = self.console.next_frame();
            self.rgba.resize(frame.pixels.len() * 4, 0);
            for (index, pixel) in frame.pixels.iter().copied().enumerate() {
                let offset = index * 4;
                self.rgba[offset] = (pixel >> 16) as u8;
                self.rgba[offset + 1] = (pixel >> 8) as u8;
                self.rgba[offset + 2] = pixel as u8;
                self.rgba[offset + 3] = 255;
            }
            (frame.width, frame.height)
        };
        self.rgba_js = Uint8Array::new_with_length(self.rgba.len() as u32);
        self.rgba_js.copy_from(&self.rgba);

        let target = audio_target_for_frame(self.frame_number);
        let count = usize::try_from(target.saturating_sub(self.audio_samples_emitted))
            .unwrap_or(usize::MAX);
        self.audio_samples_emitted = target;
        let samples = self.console.render_audio(count);
        self.audio.clear();
        self.audio.reserve(samples.len() * 2);
        for sample in &samples {
            self.audio.push(f32::from(sample.left) / 32_768.0);
            self.audio.push(f32::from(sample.right) / 32_768.0);
        }
        self.audio_js = Float32Array::new_with_length(self.audio.len() as u32);
        self.audio_js.copy_from(&self.audio);
        self.last_frame = FrameMetadata {
            frame_number: self.frame_number,
            width: frame_width as u32,
            height: frame_height as u32,
            audio_sample_rate: AUDIO_SAMPLE_RATE,
            audio_samples: samples.len(),
            audio_discontinuity: discontinuity,
        };
        self.last_frame
    }
}

fn audio_target_for_frame(frame_number: u64) -> u64 {
    frame_number.saturating_mul(u64::from(AUDIO_SAMPLE_RATE)) / 60
}
