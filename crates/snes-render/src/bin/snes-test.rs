//! Command-line entry point for bounded headless ROM execution.

use std::{env, path::PathBuf, process::ExitCode};

use snes_core::{Address24, MappingMode};
use snes_render::{HeadlessRunner, ResultProtocol, ResultStatus, RomTestDescriptor};

#[derive(Debug)]
struct Arguments {
    path: PathBuf,
    max_master_ticks: u64,
    mapping: Option<MappingMode>,
    result_protocol: Option<ResultProtocol>,
}

fn usage() {
    eprintln!(
        "usage: snes-test ROM [MASTER_TICKS] [--mapping lorom|hirom \
         --result-address ADDRESS --result-running BYTE --result-passed BYTE]"
    );
}

fn parse_number(value: &str, name: &str) -> Result<u64, String> {
    let (radix, digits) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map_or((10, value), |digits| (16, digits));
    u64::from_str_radix(digits, radix).map_err(|_| format!("invalid {name}: {value}"))
}

fn parse_mapping(value: &str) -> Result<MappingMode, String> {
    match value.to_ascii_lowercase().as_str() {
        "lorom" => Ok(MappingMode::LoRom),
        "hirom" => Ok(MappingMode::HiRom),
        _ => Err(format!("invalid mapping: {value}")),
    }
}

fn parse_args<I>(arguments: I) -> Result<Option<Arguments>, String>
where
    I: IntoIterator<Item = String>,
{
    let mut values = arguments.into_iter();
    let mut positional = Vec::new();
    let mut mapping = None;
    let mut result_address = None;
    let mut result_running = None;
    let mut result_passed = None;

    while let Some(value) = values.next() {
        if value == "--help" || value == "-h" {
            return Ok(None);
        }
        if let Some(flag) = value.strip_prefix("--") {
            let argument = values
                .next()
                .ok_or_else(|| format!("missing value for --{flag}"))?;
            match flag {
                "mapping" => mapping = Some(parse_mapping(&argument)?),
                "result-address" => result_address = Some(parse_number(&argument, flag)?),
                "result-running" => result_running = Some(parse_number(&argument, flag)?),
                "result-passed" => result_passed = Some(parse_number(&argument, flag)?),
                other => return Err(format!("unknown option: --{other}")),
            }
        } else {
            positional.push(value);
        }
    }

    let path = positional
        .first()
        .map(PathBuf::from)
        .ok_or_else(|| "missing ROM path".to_owned())?;
    if positional.len() > 2 {
        return Err("expected at most one MASTER_TICKS positional argument".to_owned());
    }
    let max_master_ticks = positional
        .get(1)
        .map(|value| parse_number(value, "MASTER_TICKS"))
        .transpose()?
        .unwrap_or(100_000);

    let result_values = [result_address, result_running, result_passed];
    let result_protocol = if result_values.iter().any(Option::is_some) {
        if result_values.iter().any(Option::is_none) {
            return Err(
                "--result-address, --result-running, and --result-passed must be supplied together"
                    .to_owned(),
            );
        }
        let address = result_address.unwrap();
        let running = result_running.unwrap();
        let passed = result_passed.unwrap();
        if address > u64::from(Address24::MAX.get()) {
            return Err(format!("result address exceeds 24 bits: {address:#x}"));
        }
        if running > u64::from(u8::MAX) || passed > u64::from(u8::MAX) {
            return Err("result values must fit in one byte".to_owned());
        }
        Some(ResultProtocol::memory_byte(
            Address24::new(address as u32),
            running as u8,
            passed as u8,
        ))
    } else {
        None
    };

    Ok(Some(Arguments {
        path,
        max_master_ticks,
        mapping,
        result_protocol,
    }))
}

fn main() -> ExitCode {
    let arguments = match parse_args(env::args().skip(1)) {
        Ok(Some(arguments)) => arguments,
        Ok(None) => {
            usage();
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("snes-test: {error}");
            usage();
            return ExitCode::from(2);
        }
    };

    let runner = HeadlessRunner::new(RomTestDescriptor {
        mapping: arguments.mapping,
        max_master_ticks: Some(arguments.max_master_ticks),
        result_protocol: arguments.result_protocol,
        ..RomTestDescriptor::default()
    });
    let mut console = match runner.load_path(&arguments.path) {
        Ok(console) => console,
        Err(error) => {
            eprintln!(
                "snes-test: could not load {}: {error:?}",
                arguments.path.display()
            );
            return ExitCode::from(1);
        }
    };
    let result = runner.run(&mut console);
    println!("ticks={} stopped={}", result.master_ticks, result.stopped);
    match result.result {
        Some(observation) => {
            println!(
                "result= {:?} address=${:06x} value=${:02x}",
                observation.status,
                observation.address.get(),
                observation.value
            );
            if matches!(observation.status, ResultStatus::Passed) {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        None => ExitCode::SUCCESS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_memory_result_protocol() {
        let arguments = parse_args([
            "test.sfc".to_owned(),
            "--result-address".to_owned(),
            "0x7e1234".to_owned(),
            "--result-running".to_owned(),
            "0".to_owned(),
            "--result-passed".to_owned(),
            "1".to_owned(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(arguments.max_master_ticks, 100_000);
        assert_eq!(arguments.mapping, None);
        assert_eq!(
            arguments.result_protocol,
            Some(ResultProtocol::memory_byte(Address24::new(0x7e1234), 0, 1))
        );
    }

    #[test]
    fn requires_complete_result_protocol() {
        let error = parse_args([
            "test.sfc".to_owned(),
            "--result-address".to_owned(),
            "0".to_owned(),
        ])
        .unwrap_err();
        assert!(error.contains("supplied together"));
    }
}
