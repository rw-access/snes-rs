//! A small, layout-independent C ABI for the SNES emulator.
//!
//! Handles are opaque heap allocations. Exported operations catch Rust
//! panics before crossing the C boundary and report failures through
//! [`SnesStatus`] and [`snes_last_error`]. RAM, video, and audio pointers are
//! borrowed views owned by the emulator handle; callers must not retain them
//! after the next operation on that handle.

use std::{
    cell::RefCell,
    ffi::CString,
    os::raw::c_char,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr, slice,
};

use snes_core::{
    Address24, AudioSample, Console, ConsoleState, ControllerPort, RewindBuffer, RomImage,
    AUDIO_SAMPLE_RATE, MASTER_TICKS_PER_FRAME,
};

/// Size of the SNES WRAM exposed by the stable RAM view.
pub const SNES_RAM_BYTES: usize = 128 * 1024;

/// Maximum native visible framebuffer size: 512 pixels by 224 lines, RGBA.
/// The view is always backed by this many bytes. Use the per-handle current
/// length and dimensions when the active PPU mode is not 256 pixels wide.
pub const SNES_FRAMEBUFFER_RGBA_BYTES: usize = 512 * 224 * 4;

/// C ABI result code. Zero means success; all nonzero values are failures.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnesStatus {
    Ok = 0,
    InvalidArgument = 1,
    InvalidRom = 2,
    UnsupportedMapper = 3,
    AllocationFailure = 4,
    Panic = 5,
    InvalidSnapshot = 6,
}

/// Opaque emulator handle. Do not construct or inspect this type from C.
pub struct SnesHandle {
    console: Console,
    rewind: RewindBuffer,
    ram_view: [u8; SNES_RAM_BYTES],
    framebuffer: Vec<u8>,
    framebuffer_len: usize,
    framebuffer_width: u16,
    framebuffer_height: u16,
    audio_view: Vec<f32>,
    controller_bits: u16,
    frame_number: u64,
    audio_samples_emitted: u64,
    video_enabled: bool,
}

/// Opaque point-in-time emulator snapshot handle.
pub struct SnesSnapshot {
    state: ConsoleState,
    controller_bits: u16,
    frame_number: u64,
    audio_samples_emitted: u64,
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_error(message: impl Into<String>) {
    let message = message.into().replace('\0', "");
    LAST_ERROR.with(|last_error| {
        *last_error.borrow_mut() = CString::new(message).ok();
    });
}

fn clear_error() {
    LAST_ERROR.with(|last_error| {
        *last_error.borrow_mut() = None;
    });
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "Rust panic across snes-ffi boundary".to_owned()
    }
}

fn ffi_status<F>(operation: F) -> SnesStatus
where
    F: FnOnce() -> SnesStatus,
{
    clear_error();
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(status) => status,
        Err(payload) => {
            set_error(format!("panic: {}", panic_message(payload)));
            SnesStatus::Panic
        }
    }
}

fn ffi_ptr<T, F>(operation: F) -> *const T
where
    F: FnOnce() -> *const T,
{
    clear_error();
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(pointer) => pointer,
        Err(payload) => {
            set_error(format!("panic: {}", panic_message(payload)));
            ptr::null()
        }
    }
}

fn invalid_argument(message: &'static str) -> SnesStatus {
    set_error(message);
    SnesStatus::InvalidArgument
}

fn invalid_snapshot(error: impl std::fmt::Debug) -> SnesStatus {
    set_error(format!("could not restore SNES snapshot: {error:?}"));
    SnesStatus::InvalidSnapshot
}

unsafe fn mutable_handle<'a>(handle: *mut SnesHandle) -> Result<&'a mut SnesHandle, SnesStatus> {
    handle
        .as_mut()
        .ok_or_else(|| invalid_argument("emulator handle is null"))
}

unsafe fn handle_ref<'a>(handle: *const SnesHandle) -> Result<&'a SnesHandle, SnesStatus> {
    handle
        .as_ref()
        .ok_or_else(|| invalid_argument("emulator handle is null"))
}

/// Return the last error for the calling thread, or null if the last call
/// succeeded. The pointer remains valid until the next snes-ffi call on the
/// same thread.
#[no_mangle]
pub extern "C" fn snes_last_error() -> *const c_char {
    match catch_unwind(AssertUnwindSafe(|| {
        LAST_ERROR.with(|last_error| {
            last_error
                .borrow()
                .as_ref()
                .map_or(ptr::null(), |message| message.as_ptr())
        })
    })) {
        Ok(pointer) => pointer,
        Err(payload) => {
            set_error(format!("panic: {}", panic_message(payload)));
            ptr::null()
        }
    }
}

/// Return the fixed SNES WRAM view size in bytes.
#[no_mangle]
pub extern "C" fn snes_ram_view_len() -> usize {
    SNES_RAM_BYTES
}

/// Return the maximum RGBA framebuffer view size in bytes.
///
/// The active image may be smaller. Query `snes_framebuffer_view_len_current`
/// and the width/height functions for the valid current image dimensions.
#[no_mangle]
pub extern "C" fn snes_framebuffer_view_len() -> usize {
    SNES_FRAMEBUFFER_RGBA_BYTES
}

/// Return the active framebuffer byte length for a handle.
#[no_mangle]
pub unsafe extern "C" fn snes_framebuffer_view_len_current(handle: *const SnesHandle) -> usize {
    match handle_ref(handle) {
        Ok(handle) => handle.framebuffer_len,
        Err(_) => 0,
    }
}

/// Return the active native framebuffer width in pixels.
#[no_mangle]
pub unsafe extern "C" fn snes_framebuffer_width(handle: *const SnesHandle) -> u16 {
    match handle_ref(handle) {
        Ok(handle) => handle.framebuffer_width,
        Err(_) => 0,
    }
}

/// Return the active native framebuffer height in pixels.
#[no_mangle]
pub unsafe extern "C" fn snes_framebuffer_height(handle: *const SnesHandle) -> u16 {
    match handle_ref(handle) {
        Ok(handle) => handle.framebuffer_height,
        Err(_) => 0,
    }
}

/// Return the number of interleaved stereo f32 values produced by the most
/// recent frame. The value is twice the number of audio samples.
#[no_mangle]
pub unsafe extern "C" fn snes_audio_view_len(handle: *const SnesHandle) -> usize {
    match handle_ref(handle) {
        Ok(handle) => handle.audio_view.len(),
        Err(_) => 0,
    }
}

/// Return the fixed S-DSP output rate used by snes-core.
#[no_mangle]
pub extern "C" fn snes_audio_sample_rate() -> u32 {
    AUDIO_SAMPLE_RATE
}

/// Return the most recent frame's borrowed interleaved stereo f32 audio.
#[no_mangle]
pub unsafe extern "C" fn snes_audio_view(handle: *const SnesHandle) -> *const f32 {
    match handle_ref(handle) {
        Ok(handle) => handle.audio_view.as_ptr(),
        Err(_) => ptr::null(),
    }
}

/// Create an emulator from an in-memory SNES ROM image.
///
/// The handle starts with video output disabled. Enable it with
/// [`snes_set_video_output`] when framebuffer updates are needed.
#[no_mangle]
pub unsafe extern "C" fn snes_create(
    rom_bytes: *const u8,
    rom_len: usize,
    out_handle: *mut *mut SnesHandle,
) -> SnesStatus {
    ffi_status(|| {
        if out_handle.is_null() {
            return invalid_argument("output emulator handle is null");
        }
        *out_handle = ptr::null_mut();
        if rom_bytes.is_null() && rom_len != 0 {
            return invalid_argument("ROM pointer is null");
        }

        let rom = if rom_len == 0 {
            &[]
        } else {
            // SAFETY: The caller promises a readable `rom_len`-byte region.
            slice::from_raw_parts(rom_bytes, rom_len)
        };
        let image = match RomImage::load(rom.to_vec()) {
            Ok(image) => image,
            Err(error) => {
                set_error(format!("invalid or unsupported SNES ROM: {error:?}"));
                return match error {
                    snes_core::LoadError::UnsupportedCoprocessor => SnesStatus::UnsupportedMapper,
                    snes_core::LoadError::EmptyImage | snes_core::LoadError::UnsupportedFormat => {
                        SnesStatus::InvalidRom
                    }
                };
            }
        };
        let mut console = match Console::new(image) {
            Ok(console) => console,
            Err(error) => {
                set_error(format!("could not create SNES console: {error:?}"));
                return SnesStatus::InvalidRom;
            }
        };
        console.reset();

        let mut handle = Box::new(SnesHandle {
            console,
            rewind: RewindBuffer::new(600),
            ram_view: [0; SNES_RAM_BYTES],
            framebuffer: vec![0; SNES_FRAMEBUFFER_RGBA_BYTES],
            framebuffer_len: 256 * 224 * 4,
            framebuffer_width: 256,
            framebuffer_height: 224,
            audio_view: Vec::new(),
            controller_bits: 0,
            frame_number: 0,
            audio_samples_emitted: 0,
            video_enabled: false,
        });
        handle.refresh_ram_view();
        *out_handle = Box::into_raw(handle);
        SnesStatus::Ok
    })
}

/// Destroy an emulator handle. Null is accepted as a no-op.
#[no_mangle]
pub unsafe extern "C" fn snes_destroy(handle: *mut SnesHandle) {
    let _ = ffi_status(|| {
        if !handle.is_null() {
            // SAFETY: The pointer must have come from `snes_create` and must
            // not have been destroyed already.
            drop(Box::from_raw(handle));
        }
        SnesStatus::Ok
    });
}

impl SnesHandle {
    fn refresh_ram_view(&mut self) {
        for (index, byte) in self.ram_view.iter_mut().enumerate() {
            *byte = self
                .console
                .read_memory(Address24::new(0x7e_0000 + index as u32));
        }
    }

    fn refresh_framebuffer(&mut self) {
        let frame = self.console.next_frame();
        let width = usize::from(frame.width);
        let height = usize::from(frame.height);
        let byte_len = width * height * 4;
        debug_assert!(byte_len <= self.framebuffer.len());
        for (index, pixel) in frame.pixels.iter().copied().enumerate() {
            let offset = index * 4;
            self.framebuffer[offset..offset + 4].copy_from_slice(&pixel.to_be_bytes());
        }
        self.framebuffer_len = byte_len;
        self.framebuffer_width = frame.width;
        self.framebuffer_height = frame.height;
    }

    fn render_audio_for_frame(&mut self) {
        let target = self
            .frame_number
            .saturating_mul(u64::from(AUDIO_SAMPLE_RATE))
            / 60;
        let count = usize::try_from(target.saturating_sub(self.audio_samples_emitted))
            .unwrap_or(usize::MAX);
        self.audio_samples_emitted = target;
        let samples = self.console.render_audio(count);
        self.audio_view.clear();
        self.audio_view.reserve(samples.len().saturating_mul(2));
        for AudioSample { left, right } in samples {
            self.audio_view.push(f32::from(left) / 32_768.0);
            self.audio_view.push(f32::from(right) / 32_768.0);
        }
    }

    fn advance_one_frame(&mut self, controller_bits: u16) {
        self.rewind.capture(&self.console);
        self.controller_bits = controller_bits;
        self.console
            .set_controller_buttons(ControllerPort::Port1, controller_bits);
        self.console.run_for_master_ticks(MASTER_TICKS_PER_FRAME);
        self.frame_number = self.frame_number.saturating_add(1);
        self.render_audio_for_frame();
        if self.video_enabled {
            self.refresh_framebuffer();
        }
        self.refresh_ram_view();
    }

    fn restore_metadata(&mut self, snapshot: &SnesSnapshot) {
        self.controller_bits = snapshot.controller_bits;
        self.frame_number = snapshot.frame_number;
        self.audio_samples_emitted = snapshot.audio_samples_emitted;
        self.audio_view.clear();
        self.rewind.clear();
        if self.video_enabled {
            self.refresh_framebuffer();
        }
        self.refresh_ram_view();
    }
}

/// Advance complete emulator frames, applying one 16-bit controller report to
/// each frame.
#[no_mangle]
pub unsafe extern "C" fn snes_advance_frames(
    handle: *mut SnesHandle,
    controller_bits: u16,
    frames: u32,
) -> SnesStatus {
    ffi_status(|| {
        let handle = match mutable_handle(handle) {
            Ok(handle) => handle,
            Err(status) => return status,
        };
        for _ in 0..frames {
            handle.advance_one_frame(controller_bits);
        }
        SnesStatus::Ok
    })
}

/// Rewind one captured frame. `out_rewound` is false when no older checkpoint
/// exists; that is a normal condition rather than an ABI error.
#[no_mangle]
pub unsafe extern "C" fn snes_rewind(
    handle: *mut SnesHandle,
    out_rewound: *mut bool,
) -> SnesStatus {
    ffi_status(|| {
        if out_rewound.is_null() {
            return invalid_argument("output rewind result is null");
        }
        *out_rewound = false;
        let handle = match mutable_handle(handle) {
            Ok(handle) => handle,
            Err(status) => return status,
        };
        let rewound = match handle.rewind.rewind(&mut handle.console) {
            Ok(rewound) => rewound,
            Err(error) => return invalid_snapshot(error),
        };
        *out_rewound = rewound;
        if rewound {
            handle.frame_number = handle.frame_number.saturating_sub(1);
            handle.audio_samples_emitted = handle
                .frame_number
                .saturating_mul(u64::from(AUDIO_SAMPLE_RATE))
                / 60;
            handle.audio_view.clear();
            if handle.video_enabled {
                handle.refresh_framebuffer();
            }
            handle.refresh_ram_view();
        }
        SnesStatus::Ok
    })
}

/// Return the stable, read-only 128 KiB WRAM view.
#[no_mangle]
pub extern "C" fn snes_ram_view(handle: *const SnesHandle) -> *const u8 {
    ffi_ptr(|| match unsafe { handle_ref(handle) } {
        Ok(handle) => handle.ram_view.as_ptr(),
        Err(_) => ptr::null(),
    })
}

/// Return the stable, read-only RGBA framebuffer backing view.
#[no_mangle]
pub extern "C" fn snes_framebuffer_view(handle: *const SnesHandle) -> *const u8 {
    ffi_ptr(|| match unsafe { handle_ref(handle) } {
        Ok(handle) => handle.framebuffer.as_ptr(),
        Err(_) => ptr::null(),
    })
}

/// Enable or disable final video rendering. Pass zero to disable and any
/// nonzero value to enable. Disabling video preserves emulation and audio
/// timelines while avoiding framebuffer rendering work.
#[no_mangle]
pub unsafe extern "C" fn snes_set_video_output(handle: *mut SnesHandle, enabled: u8) -> SnesStatus {
    ffi_status(|| {
        let handle = match mutable_handle(handle) {
            Ok(handle) => handle,
            Err(status) => return status,
        };
        handle.video_enabled = enabled != 0;
        if handle.video_enabled {
            handle.refresh_framebuffer();
        }
        SnesStatus::Ok
    })
}

/// Clone deterministic emulator state into a new opaque snapshot handle.
#[no_mangle]
pub unsafe extern "C" fn snes_snapshot(
    handle: *const SnesHandle,
    out_snapshot: *mut *mut SnesSnapshot,
) -> SnesStatus {
    ffi_status(|| {
        if out_snapshot.is_null() {
            return invalid_argument("output snapshot handle is null");
        }
        *out_snapshot = ptr::null_mut();
        let handle = match handle_ref(handle) {
            Ok(handle) => handle,
            Err(status) => return status,
        };
        let snapshot = Box::new(SnesSnapshot {
            state: handle.console.snapshot(),
            controller_bits: handle.controller_bits,
            frame_number: handle.frame_number,
            audio_samples_emitted: handle.audio_samples_emitted,
        });
        *out_snapshot = Box::into_raw(snapshot);
        SnesStatus::Ok
    })
}

/// Restore an emulator to a compatible snapshot and refresh its borrowed
/// views. The rewind history is cleared because it belongs to the old branch.
#[no_mangle]
pub unsafe extern "C" fn snes_restore(
    handle: *mut SnesHandle,
    snapshot: *const SnesSnapshot,
) -> SnesStatus {
    ffi_status(|| {
        let handle = match mutable_handle(handle) {
            Ok(handle) => handle,
            Err(status) => return status,
        };
        let snapshot = match snapshot.as_ref() {
            Some(snapshot) => snapshot,
            None => return invalid_argument("snapshot handle is null"),
        };
        if let Err(error) = handle.console.restore(&snapshot.state) {
            return invalid_snapshot(error);
        }
        handle.restore_metadata(snapshot);
        SnesStatus::Ok
    })
}

/// Destroy a snapshot handle. Null is accepted as a no-op.
#[no_mangle]
pub unsafe extern "C" fn snes_snapshot_destroy(snapshot: *mut SnesSnapshot) {
    let _ = ffi_status(|| {
        if !snapshot.is_null() {
            // SAFETY: The pointer must have come from `snes_snapshot` and
            // must not have been destroyed already.
            drop(Box::from_raw(snapshot));
        }
        SnesStatus::Ok
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_rom() -> Vec<u8> {
        let mut rom = vec![0xea; 0x8000];
        let header = 0x7fc0;
        rom[header..header + 21].fill(b' ');
        rom[header + 0x15] = 0;
        rom[header + 0x16] = 0;
        rom[header + 0x17] = 5;
        rom[header + 0x18] = 0;
        rom[0x7ffc] = 0;
        rom[0x7ffd] = 0x80;
        rom
    }

    unsafe fn create_test_handle(rom: &[u8]) -> *mut SnesHandle {
        let mut handle = ptr::null_mut();
        assert_eq!(
            snes_create(rom.as_ptr(), rom.len(), &mut handle),
            SnesStatus::Ok
        );
        assert!(!handle.is_null());
        handle
    }

    #[test]
    fn invalid_arguments_report_thread_local_errors() {
        unsafe {
            assert_eq!(
                snes_advance_frames(ptr::null_mut(), 0, 1),
                SnesStatus::InvalidArgument
            );
            let error = snes_last_error();
            assert!(!error.is_null());
            assert_eq!(
                std::ffi::CStr::from_ptr(error).to_str().unwrap(),
                "emulator handle is null"
            );
            assert!(snes_ram_view(ptr::null()).is_null());
            assert!(!snes_last_error().is_null());
        }
    }

    #[test]
    fn snapshot_restore_replays_identical_machine_state() {
        let rom = test_rom();
        unsafe {
            let handle = create_test_handle(&rom);
            let mut snapshot = ptr::null_mut();
            assert_eq!(snes_snapshot(handle, &mut snapshot), SnesStatus::Ok);
            assert_eq!(snes_advance_frames(handle, 0x0a55, 2), SnesStatus::Ok);
            let first = (*handle).console.snapshot();
            assert_eq!(snes_restore(handle, snapshot), SnesStatus::Ok);
            assert_eq!(snes_advance_frames(handle, 0x0a55, 2), SnesStatus::Ok);
            assert_eq!((*handle).console.snapshot(), first);
            snes_snapshot_destroy(snapshot);
            snes_destroy(handle);
        }
    }

    #[test]
    fn rewind_restores_previous_captured_state_and_exhausts_cleanly() {
        let rom = test_rom();
        unsafe {
            let handle = create_test_handle(&rom);
            assert_eq!(snes_advance_frames(handle, 1, 1), SnesStatus::Ok);
            let first = (*handle).console.snapshot();
            assert_eq!(snes_advance_frames(handle, 2, 1), SnesStatus::Ok);
            let mut rewound = false;
            assert_eq!(snes_rewind(handle, &mut rewound), SnesStatus::Ok);
            assert!(rewound);
            assert_eq!((*handle).console.snapshot(), first);
            assert_eq!(snes_rewind(handle, &mut rewound), SnesStatus::Ok);
            assert!(rewound);
            assert_eq!(snes_rewind(handle, &mut rewound), SnesStatus::Ok);
            assert!(!rewound);
            snes_destroy(handle);
        }
    }
}
