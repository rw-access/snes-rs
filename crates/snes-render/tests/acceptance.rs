use std::{env, path::PathBuf};

use snes_core::MappingMode;
use snes_render::{
    AcceptanceManifest, AcceptanceRunner, CaseStatus, HeadlessRunner, RomTestDescriptor,
};

#[test]
fn enabled_snes_acceptance_cases() {
    if env::var("SNES_RUN_ACCEPTANCE").ok().as_deref() != Some("1") {
        eprintln!("skipping SNES acceptance cases; set SNES_RUN_ACCEPTANCE=1 to enable");
        return;
    }

    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest_path = env::var_os("SNES_ACCEPTANCE_MANIFEST")
        .map(PathBuf::from)
        .unwrap_or_else(|| project_root.join("tests/snes-test-roms/cases.toml"));
    let manifest = AcceptanceManifest::load_path(&manifest_path)
        .unwrap_or_else(|error| panic!("{}: {error}", manifest_path.display()));
    let runner = AcceptanceRunner::new(manifest, &project_root);
    let reports = runner
        .run_enabled()
        .unwrap_or_else(|error| panic!("SNES acceptance run failed: {error}"));

    for report in reports {
        assert_eq!(
            report.status,
            CaseStatus::Passed,
            "SNES acceptance case {} failed: {:?}",
            report.case_id,
            report.message
        );
    }
}

#[test]
fn materialized_mode5_corpus_rom_reports_native_hires_dimensions() {
    let Some(path) = env::var_os("SNES_HIRES_CORPUS_ROM") else {
        eprintln!(
            "skipping native hires corpus check; set SNES_HIRES_CORPUS_ROM to a PeterLemon Mode 5 ROM"
        );
        return;
    };

    let runner = HeadlessRunner::new(RomTestDescriptor {
        mapping: Some(MappingMode::LoRom),
        max_frames: Some(5),
        ..RomTestDescriptor::default()
    });
    let path = PathBuf::from(path);
    let path = if path.is_absolute() {
        path
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path)
    };
    let mut console = runner
        .load_path(&path)
        .unwrap_or_else(|error| panic!("could not load hires corpus ROM: {error:?}"));
    runner.run(&mut console);
    let frame = console.next_frame();

    assert_eq!((frame.width, frame.height), (512, 224));
    assert_eq!(frame.pixels.len(), 512 * 224);
}
