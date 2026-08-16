//! Opt-in manifest-driven SNES corpus acceptance runner.

use std::{env, path::PathBuf, process::ExitCode};

use snes_render::{AcceptanceManifest, AcceptanceRunner, CaseStatus};

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("snes-acceptance: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<u8, String> {
    let mut args = env::args_os().skip(1);
    let mut manifest_path = PathBuf::from("tests/snes-test-roms/cases.toml");
    let mut project_root = PathBuf::from(".");
    let mut selected = Vec::new();

    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--manifest") => {
                manifest_path = args
                    .next()
                    .map(PathBuf::from)
                    .ok_or_else(|| "--manifest requires a path".to_owned())?;
            }
            Some("--project-root") => {
                project_root = args
                    .next()
                    .map(PathBuf::from)
                    .ok_or_else(|| "--project-root requires a path".to_owned())?;
            }
            Some("--case") => {
                selected.push(
                    args.next()
                        .ok_or_else(|| "--case requires an id".to_owned())?,
                );
            }
            Some("--help") | Some("-h") => {
                println!(
                    "usage: snes-acceptance [--manifest PATH] [--project-root PATH] [--case ID]"
                );
                return Ok(0);
            }
            Some(other) => return Err(format!("unknown argument {other}")),
            None => return Err("arguments must be valid UTF-8".to_owned()),
        }
    }

    let manifest =
        AcceptanceManifest::load_path(&manifest_path).map_err(|error| error.to_string())?;
    let runner = AcceptanceRunner::new(manifest, &project_root);
    let reports = if selected.is_empty() {
        runner.run_enabled().map_err(|error| error.to_string())?
    } else {
        selected
            .iter()
            .map(|id| {
                let case = runner
                    .manifest()
                    .case(id.to_str().unwrap_or_default())
                    .ok_or_else(|| format!("unknown case {id:?}"))?;
                runner.run_case(case).map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut failures = 0u8;
    for report in reports {
        let status = match report.status {
            CaseStatus::Passed => "PASS",
            CaseStatus::Diagnostic => "DIAGNOSTIC",
            CaseStatus::Failed => {
                failures = failures.saturating_add(1);
                "FAIL"
            }
        };
        let message = report
            .message
            .as_deref()
            .map(|message| format!(" message={message}"))
            .unwrap_or_default();
        println!(
            "{status} case={} ticks={} stopped={}{}",
            report.case_id, report.run.master_ticks, report.run.stopped, message
        );
        if let Some(path) = report.artifacts.actual {
            println!("  actual={}", path.display());
        }
        if let Some(path) = report.artifacts.diff {
            println!("  diff={}", path.display());
        }
    }
    Ok(if failures == 0 { 0 } else { 1 })
}
