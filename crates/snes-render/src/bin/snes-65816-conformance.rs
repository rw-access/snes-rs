//! Opt-in SingleStepTests/65816 JSON-vector runner.

use std::{env, fs, path::PathBuf, process::ExitCode};

use snes_render::{run_vector, CpuVector, VectorFile, VectorStatus};

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("snes-65816-conformance: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<u8, String> {
    let mut args = env::args_os().skip(1);
    let mut paths = Vec::new();
    let mut filter = None;
    let mut limit = None;
    let mut fail_fast = false;

    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--filter") => {
                filter = Some(
                    args.next()
                        .ok_or_else(|| "--filter requires a substring".to_owned())?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            Some("--limit") => {
                let value = args
                    .next()
                    .ok_or_else(|| "--limit requires a positive integer".to_owned())?;
                limit = Some(
                    value
                        .to_string_lossy()
                        .parse::<usize>()
                        .map_err(|_| "--limit requires a positive integer".to_owned())?,
                );
            }
            Some("--fail-fast") => fail_fast = true,
            Some("--help") | Some("-h") => {
                println!(
                    "usage: snes-65816-conformance PATH... [--filter TEXT] [--limit N] [--fail-fast]"
                );
                println!(
                    "PATH is a SingleStepTests/65816 JSON file or directory of .json files; this is opt-in."
                );
                return Ok(0);
            }
            Some(other) if other.starts_with('-') => {
                return Err(format!("unknown option {other}"));
            }
            Some(_) => paths.push(PathBuf::from(argument)),
            None => return Err("arguments must be valid UTF-8".to_owned()),
        }
    }
    if paths.is_empty() {
        return Err("a JSON vector file or directory is required (try --help)".to_owned());
    }

    let files = collect_files(&paths)?;
    let mut total = 0usize;
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut unsupported = 0usize;
    for path in files {
        let file =
            VectorFile::load_path(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        for vector in file.vectors {
            if filter
                .as_deref()
                .is_some_and(|needle| !vector.name.contains(needle))
            {
                continue;
            }
            if limit.is_some_and(|value| total >= value) {
                break;
            }
            let report = run_vector(&vector);
            total += 1;
            match report.status {
                VectorStatus::Passed => {
                    passed += 1;
                    println!("PASS {} cycles={}", vector.name, report.measured_cycles);
                }
                VectorStatus::Failed => {
                    failed += 1;
                    println!("FAIL {}", vector.name);
                    for mismatch in report.mismatches.iter().take(12) {
                        println!("  {mismatch}");
                    }
                    if report.mismatches.len() > 12 {
                        println!("  ... {} more mismatch(es)", report.mismatches.len() - 12);
                    }
                    if fail_fast {
                        break;
                    }
                }
                VectorStatus::Unsupported => {
                    unsupported += 1;
                    println!("UNSUPPORTED {}", vector.name);
                    for mismatch in report.mismatches.iter().take(4) {
                        println!("  {mismatch}");
                    }
                    if fail_fast {
                        break;
                    }
                }
            }
        }
        if fail_fast && (failed != 0 || unsupported != 0) {
            break;
        }
        if limit.is_some_and(|value| total >= value) {
            break;
        }
    }
    if total == 0 {
        return Err("no vectors matched the requested filter/limit".to_owned());
    }
    println!("summary vectors={total} passed={passed} failed={failed} unsupported={unsupported}");
    Ok(if failed == 0 && unsupported == 0 {
        0
    } else {
        1
    })
}

fn collect_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for path in paths {
        let metadata =
            fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
        if metadata.is_file() {
            files.push(path.clone());
            continue;
        }
        if !metadata.is_dir() {
            return Err(format!(
                "{} is neither a file nor a directory",
                path.display()
            ));
        }
        for entry in fs::read_dir(path).map_err(|error| format!("{}: {error}", path.display()))? {
            let entry = entry.map_err(|error| format!("{}: {error}", path.display()))?;
            let child = entry.path();
            if child
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                files.push(child);
            }
        }
    }
    files.sort();
    Ok(files)
}

#[allow(dead_code)]
fn _keep_vector_type_visible(_: &CpuVector) {}
