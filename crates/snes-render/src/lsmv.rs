//! Reader for the text-archive form of lsnes `.lsmv` movie files.
//!
//! The supported subset is intentionally the one useful for ordinary SNES
//! replay: NTSC/PAL standard gamepads, one input sample per video frame, and
//! a clean-reset movie. Savestate/SRAM playback is not attempted here;
//! the ROM and emulator state must come from the caller, like the FM2 path in
//! `nes-rs`.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs,
    io::{Cursor, Read},
    path::Path,
};

use zip::ZipArchive;

const BUTTONS: [char; 12] = ['B', 'Y', 's', 'S', 'u', 'd', 'l', 'r', 'A', 'X', 'L', 'R'];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameRate {
    pub numerator: u64,
    pub denominator: u64,
}

impl FrameRate {
    pub const NTSC: Self = Self {
        numerator: 10_738_636,
        denominator: 178_683,
    };

    pub const PAL: Self = Self {
        numerator: 322_445,
        denominator: 6_448,
    };

    pub fn as_ffmpeg_value(self) -> String {
        format!("{}/{}", self.numerator, self.denominator)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LsmvFrame {
    pub port1: u16,
    pub port2: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LsmvMovie {
    pub game_type: String,
    pub game_name: Option<String>,
    pub rom_sha256: Option<String>,
    pub frame_rate: FrameRate,
    pub frames: Vec<LsmvFrame>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LsmvError {
    message: String,
}

impl LsmvError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for LsmvError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(formatter)
    }
}

impl Error for LsmvError {}

impl LsmvMovie {
    pub fn read(path: &Path) -> Result<Self, LsmvError> {
        let bytes = fs::read(path).map_err(|error| {
            LsmvError::new(format!("failed to read {}: {error}", path.display()))
        })?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, LsmvError> {
        parse_archive(bytes, 0)
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Verifies the ROM identity recorded by lsnes when the movie contains
    /// one. Movies without a ROM hash are accepted for compatibility with
    /// older recordings.
    pub fn verify_rom_checksum(&self, rom: &[u8]) -> Result<(), LsmvError> {
        let Some(expected) = self.rom_sha256.as_deref() else {
            return Ok(());
        };
        let actual = crate::sha256_hex(rom);
        if expected.eq_ignore_ascii_case(&actual) {
            Ok(())
        } else {
            Err(LsmvError::new(format!(
                "ROM SHA-256 mismatch: movie has {expected}, supplied ROM has {actual}"
            )))
        }
    }
}

fn parse_archive(bytes: &[u8], depth: usize) -> Result<LsmvMovie, LsmvError> {
    if depth > 2 {
        return Err(LsmvError::new("nested LSMV wrapper is too deep"));
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| LsmvError::new(format!("invalid LSMV ZIP archive: {error}")))?;
    let mut members = BTreeMap::<String, Vec<u8>>::new();
    for index in 0..archive.len() {
        let mut member = archive
            .by_index(index)
            .map_err(|error| LsmvError::new(format!("cannot read LSMV archive member: {error}")))?;
        if member.is_dir() {
            continue;
        }
        let name = member.name().to_owned();
        let mut data = Vec::new();
        member
            .read_to_end(&mut data)
            .map_err(|error| LsmvError::new(format!("cannot read LSMV member {name}: {error}")))?;
        members.insert(name, data);
    }

    if let Some(input) = members.get("input") {
        return parse_input_members(&members, input);
    }

    let nested = members
        .iter()
        .find(|(name, _)| name.to_ascii_lowercase().ends_with(".lsmv"))
        .map(|(_, data)| data.as_slice())
        .ok_or_else(|| LsmvError::new("LSMV archive has no input member"))?;
    parse_archive(nested, depth + 1)
}

fn parse_input_members(
    members: &BTreeMap<String, Vec<u8>>,
    input: &[u8],
) -> Result<LsmvMovie, LsmvError> {
    let input = std::str::from_utf8(input)
        .map_err(|_| LsmvError::new("LSMV input member is not UTF-8 text"))?;
    let game_type = member_text(members, "gametype")?.unwrap_or_else(|| "snes_ntsc".to_owned());
    if members.contains_key("savestate")
        || members.contains_key("savestate.anchor")
        || members.keys().any(|name| name.starts_with("moviesram."))
    {
        return Err(LsmvError::new(
            "savestate- or SRAM-anchored LSMV movies are unsupported; replay requires a clean ROM reset",
        ));
    }
    let frame_rate = match game_type.trim() {
        "snes_ntsc" => FrameRate::NTSC,
        "snes_pal" => FrameRate::PAL,
        other => {
            return Err(LsmvError::new(format!(
                "unsupported LSMV game type {other}"
            )))
        }
    };
    let mut frames = Vec::new();
    for (line_number, raw_line) in input.lines().enumerate() {
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with('F') {
            return Err(LsmvError::new(format!(
                "LSMV subframe at input line {} is unsupported; replay requires one sample per frame",
                line_number + 1
            )));
        }
        if !line.starts_with("F.") {
            return Err(LsmvError::new(format!(
                "LSMV reset at input line {} is unsupported",
                line_number + 1
            )));
        }
        let mut fields = line.split('|');
        let _poll_fields = fields.next();
        let port1 = parse_controller(fields.next(), line_number)?;
        let port2 = match fields.next() {
            Some(buttons) if !buttons.is_empty() => parse_controller(Some(buttons), line_number)?,
            _ => 0,
        };
        if fields.next().is_some() {
            return Err(LsmvError::new(format!(
                "LSMV input line {} contains more than two controller ports",
                line_number + 1
            )));
        }
        frames.push(LsmvFrame { port1, port2 });
    }
    if frames.is_empty() {
        return Err(LsmvError::new("LSMV movie contains no input frames"));
    }
    Ok(LsmvMovie {
        game_type,
        game_name: member_text(members, "gamename")?,
        rom_sha256: member_text(members, "rom.sha256")?,
        frame_rate,
        frames,
    })
}

fn member_text(
    members: &BTreeMap<String, Vec<u8>>,
    name: &str,
) -> Result<Option<String>, LsmvError> {
    let Some(bytes) = members.get(name) else {
        return Ok(None);
    };
    let text = std::str::from_utf8(bytes)
        .map_err(|_| LsmvError::new(format!("LSMV member {name} is not UTF-8 text")))?;
    Ok(Some(text.trim().to_owned()))
}

fn parse_controller(field: Option<&str>, line_number: usize) -> Result<u16, LsmvError> {
    let field = field.ok_or_else(|| {
        LsmvError::new(format!(
            "LSMV input line {} has no controller field",
            line_number + 1
        ))
    })?;
    let field = field.trim();
    let mut buttons = 0;
    let chars: Vec<char> = field.chars().collect();
    if chars.len() != BUTTONS.len() {
        return Err(LsmvError::new(format!(
            "LSMV input line {} has {} controller characters; expected {}",
            line_number + 1,
            chars.len(),
            BUTTONS.len()
        )));
    }
    for (index, (actual, expected)) in chars.iter().zip(BUTTONS).enumerate() {
        if *actual != '.' && *actual != expected {
            return Err(LsmvError::new(format!(
                "LSMV input line {} has unexpected button marker {:?} at column {}",
                line_number + 1,
                actual,
                index
            )));
        }
        if *actual != '.' {
            buttons |= 1 << index;
        }
    }
    Ok(buttons)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_frame_and_snes_button_order() {
        let movie = parse_input_members(
            &BTreeMap::from([
                ("gametype".to_owned(), b"snes_ntsc\n".to_vec()),
                ("gamename".to_owned(), b"Fixture Game\n".to_vec()),
            ]),
            b"F. 0 0|BYsSudlrAXLR\nF. 0 0|............\n",
        )
        .unwrap();
        assert_eq!(movie.frame_count(), 2);
        assert_eq!(movie.frames[0].port1, 0x0fff);
        assert_eq!(movie.frames[1], LsmvFrame::default());
        assert_eq!(movie.game_name.as_deref(), Some("Fixture Game"));
    }

    #[test]
    fn rejects_subframes_and_resets_in_frame_only_replay() {
        let members = BTreeMap::from([("gametype".to_owned(), b"snes_ntsc".to_vec())]);
        assert!(parse_input_members(&members, b"F. 0 0|............\n .|............\n").is_err());
        assert!(parse_input_members(&members, b"FR 0 0|............\n").is_err());
    }
}
