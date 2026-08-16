//! Command-line entry point for bounded headless ROM execution.

use std::{env, path::PathBuf};

use snes_render::{HeadlessRunner, RomTestDescriptor};

fn main() {
    let mut args = env::args_os().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!("usage: snes-test ROM [MASTER_TICKS]");
        return;
    };
    let max_master_ticks = args
        .next()
        .and_then(|value| value.to_str().and_then(|value| value.parse().ok()))
        .or(Some(100_000));
    let runner = HeadlessRunner::new(RomTestDescriptor {
        max_master_ticks,
        ..RomTestDescriptor::default()
    });
    let mut console = match runner.load_path(&path) {
        Ok(console) => console,
        Err(error) => {
            eprintln!("snes-test: could not load {}: {error:?}", path.display());
            return;
        }
    };
    let result = runner.run(&mut console);
    println!("ticks={} stopped={}", result.master_ticks, result.stopped);
}
