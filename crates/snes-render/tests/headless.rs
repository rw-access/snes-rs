use snes_core::MappingMode;
use snes_render::{HeadlessRunner, RomTestDescriptor};

#[test]
fn headless_boundary_accepts_an_image() {
    let runner = HeadlessRunner::new(RomTestDescriptor::default());
    assert!(runner.load_bytes(vec![0]).is_ok());
}

#[test]
fn headless_runner_executes_until_a_bounded_stop() {
    let mut rom = vec![0xea; 0x8000];
    rom[0] = 0xdb;
    rom[0x7ffc] = 0x00;
    rom[0x7ffd] = 0x80;
    let runner = HeadlessRunner::new(RomTestDescriptor {
        max_master_ticks: Some(100),
        ..RomTestDescriptor::default()
    });
    let mut console = runner.load_bytes(rom).unwrap();
    let result = runner.run(&mut console);
    assert!(result.stopped);
    assert!(result.master_ticks > 0);
}

#[test]
fn headless_runner_honors_frame_bounds() {
    let mut rom = vec![0xea; 0x8000];
    rom[0x7ffc] = 0;
    rom[0x7ffd] = 0x80;
    let runner = HeadlessRunner::new(RomTestDescriptor {
        max_frames: Some(1),
        ..RomTestDescriptor::default()
    });
    let mut console = runner.load_bytes(rom).unwrap();
    let result = runner.run(&mut console);
    assert!(result.master_ticks > 0);
    assert!(!result.stopped);
}

#[test]
fn headless_runner_applies_explicit_mapping_to_raw_bytes() {
    let mut rom = vec![0; 0x10000];
    // A headerless HiROM image uses the linear address at $00:FFFC for its
    // reset vector and maps $00:8000 to the byte at ROM offset $8000.
    rom[0xfffc] = 0x00;
    rom[0xfffd] = 0x80;
    rom[0x8000] = 0xdb; // STP
    let runner = HeadlessRunner::new(RomTestDescriptor {
        mapping: Some(MappingMode::HiRom),
        max_master_ticks: Some(100),
        ..RomTestDescriptor::default()
    });

    let mut console = runner.load_bytes(rom).unwrap();
    let result = runner.run(&mut console);

    assert!(result.stopped);
}
