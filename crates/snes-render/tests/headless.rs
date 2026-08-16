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
