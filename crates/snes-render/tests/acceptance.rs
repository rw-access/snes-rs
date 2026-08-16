use std::{env, path::PathBuf};

use snes_render::{AcceptanceManifest, AcceptanceRunner, CaseStatus};

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
