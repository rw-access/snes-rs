use snes_render::{HeadlessRunner, RomTestDescriptor};

#[test]
fn headless_boundary_accepts_an_image() {
    let runner = HeadlessRunner::new(RomTestDescriptor::default());
    assert!(runner.load_bytes(vec![0]).is_ok());
}
