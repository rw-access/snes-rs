//! Optional SingleStepTests/65816 JSON-vector runner.
//!
//! The vector corpus is deliberately not a normal Rust test dependency.  This
//! module has a small JSON parser so `cargo test --workspace` stays offline and
//! does not need to materialize the external corpus.  The runner uses the
//! public `Cpu65816`/`CpuBus` boundary and a sparse 24-bit bus.

use std::{collections::BTreeMap, fmt, fs, path::Path};

use snes_core::{Address24, Cpu65816, CpuBus, CpuState};

const SCRATCH_SEARCH_START: u32 = 0x008000;
const SCRATCH_SEARCH_END: u32 = 0x00ff00;
const SCRATCH_PREFERRED: u32 = 0x009000;
const BANK_SIZE: u32 = 0x10000;
const BOOTSTRAP_SLOT_SIZE: u32 = 0x00000100;
const STACK_RESERVATION_BYTES: u16 = 0x0100;
const MAX_BOOTSTRAP_STEPS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VectorFile {
    pub vectors: Vec<CpuVector>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuVector {
    pub name: String,
    pub initial: VectorState,
    pub final_state: VectorState,
    pub cycles: Vec<BusCycle>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VectorState {
    pub pc: u16,
    pub s: u16,
    pub p: u8,
    pub a: u16,
    pub x: u16,
    pub y: u16,
    pub dbr: u8,
    pub d: u16,
    pub pbr: u8,
    pub e: bool,
    pub ram: BTreeMap<u32, u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BusCycle {
    pub address: u32,
    pub value: Option<u8>,
    pub tags: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VectorReport {
    pub name: String,
    pub status: VectorStatus,
    pub mismatches: Vec<VectorMismatch>,
    pub bootstrap_steps: usize,
    pub measured_cycles: u32,
    pub expected_bus_cycles: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorStatus {
    Passed,
    Failed,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VectorMismatch {
    Bootstrap(String),
    Cpu {
        field: &'static str,
        expected: String,
        actual: String,
    },
    Memory {
        address: u32,
        expected: Option<u8>,
        actual: u8,
    },
    UnexpectedWrite {
        address: u32,
        value: u8,
    },
}

impl fmt::Display for VectorMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bootstrap(message) => write!(formatter, "bootstrap: {message}"),
            Self::Cpu {
                field,
                expected,
                actual,
            } => write!(formatter, "cpu.{field}: expected {expected}, got {actual}"),
            Self::Memory {
                address,
                expected,
                actual,
            } => match expected {
                Some(expected) => write!(
                    formatter,
                    "memory ${address:06x}: expected ${expected:02x}, got ${actual:02x}"
                ),
                None => write!(
                    formatter,
                    "memory ${address:06x}: expected no write, got ${actual:02x}"
                ),
            },
            Self::UnexpectedWrite { address, value } => write!(
                formatter,
                "memory ${address:06x}: unexpected write of ${value:02x}"
            ),
        }
    }
}

#[derive(Debug)]
pub enum ConformanceError {
    Io(String),
    Parse(String),
    InvalidVector(String),
}

impl fmt::Display for ConformanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(formatter, "I/O error: {message}"),
            Self::Parse(message) => write!(formatter, "JSON parse error: {message}"),
            Self::InvalidVector(message) => write!(formatter, "invalid vector: {message}"),
        }
    }
}

impl std::error::Error for ConformanceError {}

impl VectorFile {
    pub fn parse(text: &str) -> Result<Self, ConformanceError> {
        let value = JsonParser::new(text).parse()?;
        let entries = value.into_array("top-level vector array")?;
        let vectors = entries
            .into_iter()
            .map(CpuVector::from_json)
            .collect::<Result<Vec<_>, _>>()?;
        if vectors.is_empty() {
            return Err(ConformanceError::InvalidVector(
                "vector file is empty".to_owned(),
            ));
        }
        Ok(Self { vectors })
    }

    pub fn load_path(path: &Path) -> Result<Self, ConformanceError> {
        let text = fs::read_to_string(path)
            .map_err(|error| ConformanceError::Io(format!("{}: {error}", path.display())))?;
        Self::parse(&text)
    }
}

impl CpuVector {
    fn from_json(value: JsonValue) -> Result<Self, ConformanceError> {
        let mut object = value.into_object("vector")?;
        let name = take_string(&mut object, "name")?;
        let initial = VectorState::from_json(take(&mut object, "initial")?, "initial")?;
        let final_state = VectorState::from_json(take(&mut object, "final")?, "final")?;
        let cycles = take(&mut object, "cycles")?
            .into_array("cycles")?
            .into_iter()
            .map(BusCycle::from_json)
            .collect::<Result<Vec<_>, _>>()?;
        reject_extra(object, "vector")?;
        Ok(Self {
            name,
            initial,
            final_state,
            cycles,
        })
    }
}

impl VectorState {
    fn from_json(value: JsonValue, label: &str) -> Result<Self, ConformanceError> {
        let mut object = value.into_object(label)?;
        let state = Self {
            pc: number_u16(&mut object, "pc", label)?,
            s: number_u16(&mut object, "s", label)?,
            p: number_u8(&mut object, "p", label)?,
            a: number_u16(&mut object, "a", label)?,
            x: number_u16(&mut object, "x", label)?,
            y: number_u16(&mut object, "y", label)?,
            dbr: number_u8(&mut object, "dbr", label)?,
            d: number_u16(&mut object, "d", label)?,
            pbr: number_u8(&mut object, "pbr", label)?,
            e: number_bool(&mut object, "e", label)?,
            ram: parse_ram(take(&mut object, "ram")?, label)?,
        };
        reject_extra(object, label)?;
        Ok(state)
    }
}

impl BusCycle {
    fn from_json(value: JsonValue) -> Result<Self, ConformanceError> {
        let values = value.into_array("bus cycle")?;
        if values.len() != 3 {
            return Err(ConformanceError::InvalidVector(
                "bus cycle must contain [address, value, tags]".to_owned(),
            ));
        }
        let mut values = values.into_iter();
        let address = values
            .next()
            .expect("length checked")
            .into_number("cycle address")?;
        let value = values
            .next()
            .expect("length checked")
            .into_optional_byte("cycle value")?;
        let tags = values
            .next()
            .expect("length checked")
            .into_string("cycle tags")?;
        Ok(Self {
            address: bounded(address, 0x00ff_ffff, "cycle address")? as u32,
            value,
            tags,
        })
    }
}

fn parse_ram(value: JsonValue, label: &str) -> Result<BTreeMap<u32, u8>, ConformanceError> {
    let mut ram = BTreeMap::new();
    for cell in value.into_array("ram")? {
        let values = cell.into_array("RAM cell")?;
        if values.len() != 2 {
            return Err(ConformanceError::InvalidVector(format!(
                "{label}.ram cell must contain [address, value]"
            )));
        }
        let mut values = values.into_iter();
        let address = bounded(
            values
                .next()
                .expect("length checked")
                .into_number("RAM address")?,
            0x00ff_ffff,
            "RAM address",
        )? as u32;
        let byte = bounded(
            values
                .next()
                .expect("length checked")
                .into_number("RAM value")?,
            0xff,
            "RAM value",
        )? as u8;
        if ram.insert(address, byte).is_some() {
            return Err(ConformanceError::InvalidVector(format!(
                "{label}.ram contains duplicate address ${address:06x}"
            )));
        }
    }
    Ok(ram)
}

/// Runs one vector against the real 65816 semantic engine.
pub fn run_vector(vector: &CpuVector) -> VectorReport {
    let mut bus = SparseBus::new(vector.initial.ram.clone());
    let Some(scratch) = choose_scratch(&vector.initial, &vector.final_state) else {
        return VectorReport {
            name: vector.name.clone(),
            status: VectorStatus::Unsupported,
            mismatches: vec![VectorMismatch::Bootstrap(
                "no collision-free bank-00 bootstrap slot".to_owned(),
            )],
            bootstrap_steps: 0,
            measured_cycles: 0,
            expected_bus_cycles: vector.cycles.len(),
        };
    };
    let bootstrap = build_bootstrap(scratch, &vector.initial);
    bus.install_bootstrap(scratch, &bootstrap);
    bus.memory.insert(0x00fffc, scratch as u8);
    bus.memory.insert(0x00fffd, (scratch >> 8) as u8);

    let mut cpu = Cpu65816::default();
    cpu.reset_with_bus(&mut bus);
    let mut bootstrap_steps = 0;
    let mut bootstrap_error = None;
    while bootstrap_steps < MAX_BOOTSTRAP_STEPS {
        if cpu.state().program_bank == vector.initial.pbr && cpu.state().pc == vector.initial.pc {
            break;
        }
        cpu.step_with_bus(&mut bus, None);
        bootstrap_steps += 1;
    }
    if cpu.state().program_bank != vector.initial.pbr || cpu.state().pc != vector.initial.pc {
        bootstrap_error = Some(format!(
            "did not reach target ${:02x}:{:04x}; reached ${:02x}:{:04x} after {bootstrap_steps} steps",
            vector.initial.pbr,
            vector.initial.pc,
            cpu.state().program_bank,
            cpu.state().pc
        ));
    }

    if bootstrap_error.is_none() {
        bus.begin_measurement();
        let step = cpu.step_with_bus(&mut bus, None);
        let mismatches = compare_result(&cpu.state(), &vector.final_state, &bus);
        return VectorReport {
            name: vector.name.clone(),
            status: if mismatches.is_empty() {
                VectorStatus::Passed
            } else {
                VectorStatus::Failed
            },
            mismatches,
            bootstrap_steps,
            measured_cycles: step.cycles,
            expected_bus_cycles: vector.cycles.len(),
        };
    }

    VectorReport {
        name: vector.name.clone(),
        status: VectorStatus::Unsupported,
        mismatches: vec![VectorMismatch::Bootstrap(
            bootstrap_error.expect("checked above"),
        )],
        bootstrap_steps,
        measured_cycles: 0,
        expected_bus_cycles: vector.cycles.len(),
    }
}

fn compare_result(
    state: &CpuState,
    expected: &VectorState,
    bus: &SparseBus,
) -> Vec<VectorMismatch> {
    let mut mismatches = Vec::new();
    compare_cpu(&mut mismatches, state, expected);
    for (&address, &expected_value) in &expected.ram {
        let actual = bus.memory.get(&address).copied().unwrap_or(0);
        if actual != expected_value {
            mismatches.push(VectorMismatch::Memory {
                address,
                expected: Some(expected_value),
                actual,
            });
        }
    }
    for (&address, &value) in &bus.writes {
        if !expected.ram.contains_key(&address) {
            mismatches.push(VectorMismatch::UnexpectedWrite { address, value });
        }
    }
    mismatches
}

fn compare_cpu(mismatches: &mut Vec<VectorMismatch>, actual: &CpuState, expected: &VectorState) {
    macro_rules! field {
        ($name:literal, $actual:expr, $expected:expr) => {
            if $actual != $expected {
                mismatches.push(VectorMismatch::Cpu {
                    field: $name,
                    expected: format!("{:?}", $expected),
                    actual: format!("{:?}", $actual),
                });
            }
        };
    }
    field!("pc", actual.pc, expected.pc);
    field!("sp", actual.sp, expected.s);
    field!("status", actual.status, expected.p);
    field!("a", actual.a, expected.a);
    field!("x", actual.x, expected.x);
    field!("y", actual.y, expected.y);
    field!("data_bank", actual.data_bank, expected.dbr);
    field!("direct_page", actual.direct_page, expected.d);
    field!("program_bank", actual.program_bank, expected.pbr);
    field!("emulation_mode", actual.emulation_mode, expected.e);
    field!("waiting", actual.waiting, false);
    field!("stopped", actual.stopped, false);
}

fn choose_scratch(initial: &VectorState, final_state: &VectorState) -> Option<u32> {
    let mut occupied = initial
        .ram
        .keys()
        .chain(final_state.ram.keys())
        .copied()
        .collect::<std::collections::BTreeSet<_>>();

    // During setup the bootstrap enters native mode before loading the
    // requested stack pointer. Its PHA instructions therefore write downward
    // from the supplied initial native stack pointer. Reserve the complete
    // possible write range, including 16-bit wrapping at bank 00's boundary.
    for offset in 0..=STACK_RESERVATION_BYTES {
        occupied.insert(u32::from(initial.s.wrapping_sub(offset)));
    }

    // If the requested state is in emulation mode, setup also writes the stack
    // page after XCE. This is a fixed bank-00 range independent of S.
    occupied.extend(0x000100..0x000200);

    // Keep the bootstrap in the documented bank-00 high-memory pages. $9000
    // is preferred because vectors with stacks near $8000 commonly overlap
    // the first candidate page with their setup pushes.
    let candidates = std::iter::once(SCRATCH_PREFERRED).chain(
        (SCRATCH_SEARCH_START..=SCRATCH_SEARCH_END)
            .step_by(BOOTSTRAP_SLOT_SIZE as usize)
            .filter(|&candidate| candidate != SCRATCH_PREFERRED),
    );
    for candidate in candidates {
        let end = candidate + BOOTSTRAP_SLOT_SIZE;
        if (candidate..end).any(|address| occupied.contains(&address)) {
            continue;
        }
        if initial.pbr == 0 && (candidate..end).contains(&u32::from(initial.pc)) {
            continue;
        }
        return Some(candidate);
    }
    None
}

fn build_bootstrap(scratch: u32, initial: &VectorState) -> Vec<u8> {
    debug_assert_eq!(scratch & 0xff0000, 0);
    debug_assert!(scratch
        .checked_add(BOOTSTRAP_SLOT_SIZE)
        .is_some_and(|end| end <= BANK_SIZE));
    let mut code = Vec::new();
    macro_rules! emit {
        ($bytes:expr) => {
            code.extend_from_slice($bytes)
        };
    }
    let emit_imm16 = |code: &mut Vec<u8>, opcode: u8, value: u16| {
        code.extend_from_slice(&[opcode, value as u8, (value >> 8) as u8]);
    };

    // Enter native mode so every register can be initialized, regardless of
    // the reset status.  The final status and emulation bit are restored below.
    emit!(&[0x18, 0xfb, 0xc2, 0x30]); // CLC, XCE, REP #$30
    emit_imm16(&mut code, 0xa2, initial.s);
    emit!(&[0x9a]); // TXS
    emit!(&[0xe2, 0x20, 0xa9, initial.dbr, 0x48, 0xab, 0xc2, 0x20]);
    emit_imm16(&mut code, 0xa9, initial.d);
    emit!(&[0x5b]); // TCD
    emit_imm16(&mut code, 0xa9, initial.a);
    emit!(&[0xc2, 0x10]); // REP #$10; X/Y are loaded as full words.
    emit_imm16(&mut code, 0xa2, initial.x);
    emit_imm16(&mut code, 0xa0, initial.y);

    if initial.e {
        emit!(&[0x38, 0xfb]); // SEC, XCE: return to emulation mode.
    }

    // Put the requested P on the stack, load A without disturbing that stack
    // byte, then PLP.  This is why the final status is exact even though the
    // immediate loads used during setup alter N/Z.
    emit!(&[0xe2, 0x20, 0xa9, initial.p, 0x48]); // SEP #$20; LDA #P; PHA
    if initial.p & 0x20 == 0 {
        emit!(&[0xc2, 0x20]); // REP #$20
        emit_imm16(&mut code, 0xa9, initial.a);
    } else {
        code.extend_from_slice(&[0xa9, initial.a as u8]);
    }
    emit!(&[0x28]); // PLP
    code.extend_from_slice(&[0x5c, initial.pc as u8, (initial.pc >> 8) as u8, initial.pbr]);
    debug_assert!(code.len() as u32 <= BOOTSTRAP_SLOT_SIZE);
    code
}

struct SparseBus {
    memory: BTreeMap<u32, u8>,
    original: BTreeMap<u32, u8>,
    writes: BTreeMap<u32, u8>,
    measuring: bool,
}

impl SparseBus {
    fn new(initial: BTreeMap<u32, u8>) -> Self {
        Self {
            memory: initial.clone(),
            original: initial,
            writes: BTreeMap::new(),
            measuring: false,
        }
    }

    fn install_bootstrap(&mut self, address: u32, bytes: &[u8]) {
        for (offset, &byte) in bytes.iter().enumerate() {
            self.memory.insert(address + offset as u32, byte);
        }
    }

    fn begin_measurement(&mut self) {
        self.memory = self.original.clone();
        self.writes.clear();
        self.measuring = true;
    }
}

impl CpuBus for SparseBus {
    fn read(&mut self, address: Address24) -> u8 {
        self.memory.get(&address.get()).copied().unwrap_or(0)
    }

    fn write(&mut self, address: Address24, value: u8) {
        self.memory.insert(address.get(), value);
        if self.measuring {
            self.writes.insert(address.get(), value);
        }
    }
}

// --- Small dependency-free JSON parser ----------------------------------

#[derive(Clone, Debug, PartialEq)]
enum JsonValue {
    Null,
    Bool(bool),
    Number(u64),
    String(String),
    Array(Vec<JsonValue>),
    Object(BTreeMap<String, JsonValue>),
}

impl JsonValue {
    fn into_array(self, label: &str) -> Result<Vec<JsonValue>, ConformanceError> {
        match self {
            Self::Array(value) => Ok(value),
            _ => Err(ConformanceError::Parse(format!(
                "expected array for {label}"
            ))),
        }
    }

    fn into_object(self, label: &str) -> Result<BTreeMap<String, JsonValue>, ConformanceError> {
        match self {
            Self::Object(value) => Ok(value),
            _ => Err(ConformanceError::Parse(format!(
                "expected object for {label}"
            ))),
        }
    }

    fn into_number(self, label: &str) -> Result<u64, ConformanceError> {
        match self {
            Self::Number(value) => Ok(value),
            _ => Err(ConformanceError::Parse(format!(
                "expected integer for {label}"
            ))),
        }
    }

    fn into_string(self, label: &str) -> Result<String, ConformanceError> {
        match self {
            Self::String(value) => Ok(value),
            _ => Err(ConformanceError::Parse(format!(
                "expected string for {label}"
            ))),
        }
    }

    fn into_optional_byte(self, label: &str) -> Result<Option<u8>, ConformanceError> {
        match self {
            Self::Null => Ok(None),
            Self::Number(value) => bounded(value, 0xff, label).map(|value| Some(value as u8)),
            _ => Err(ConformanceError::Parse(format!(
                "expected integer or null for {label}"
            ))),
        }
    }
}

struct JsonParser<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> JsonParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            position: 0,
        }
    }

    fn parse(mut self) -> Result<JsonValue, ConformanceError> {
        let value = self.value()?;
        self.whitespace();
        if self.position != self.input.len() {
            return Err(self.error("trailing data"));
        }
        Ok(value)
    }

    fn value(&mut self) -> Result<JsonValue, ConformanceError> {
        self.whitespace();
        match self.peek() {
            Some(b'n') => {
                self.literal(b"null")?;
                Ok(JsonValue::Null)
            }
            Some(b't') => {
                self.literal(b"true")?;
                Ok(JsonValue::Bool(true))
            }
            Some(b'f') => {
                self.literal(b"false")?;
                Ok(JsonValue::Bool(false))
            }
            Some(b'"') => Ok(JsonValue::String(self.string()?)),
            Some(b'[') => self.array(),
            Some(b'{') => self.object(),
            Some(byte) if byte.is_ascii_digit() => Ok(JsonValue::Number(self.number()?)),
            _ => Err(self.error("expected JSON value")),
        }
    }

    fn array(&mut self) -> Result<JsonValue, ConformanceError> {
        self.expect(b'[')?;
        let mut values = Vec::new();
        self.whitespace();
        if self.consume(b']') {
            return Ok(JsonValue::Array(values));
        }
        loop {
            values.push(self.value()?);
            self.whitespace();
            if self.consume(b']') {
                return Ok(JsonValue::Array(values));
            }
            self.expect(b',')?;
        }
    }

    fn object(&mut self) -> Result<JsonValue, ConformanceError> {
        self.expect(b'{')?;
        let mut values = BTreeMap::new();
        self.whitespace();
        if self.consume(b'}') {
            return Ok(JsonValue::Object(values));
        }
        loop {
            self.whitespace();
            let key = self.string()?;
            self.whitespace();
            self.expect(b':')?;
            let value = self.value()?;
            if values.insert(key.clone(), value).is_some() {
                return Err(self.error(&format!("duplicate object key {key:?}")));
            }
            self.whitespace();
            if self.consume(b'}') {
                return Ok(JsonValue::Object(values));
            }
            self.expect(b',')?;
        }
    }

    fn string(&mut self) -> Result<String, ConformanceError> {
        self.expect(b'"')?;
        let mut output = String::new();
        loop {
            let byte = self
                .take()
                .ok_or_else(|| self.error("unterminated string"))?;
            match byte {
                b'"' => return Ok(output),
                b'\\' => {
                    let escaped = self.take().ok_or_else(|| self.error("bad escape"))?;
                    match escaped {
                        b'"' => output.push('"'),
                        b'\\' => output.push('\\'),
                        b'/' => output.push('/'),
                        b'b' => output.push('\x08'),
                        b'f' => output.push('\x0c'),
                        b'n' => output.push('\n'),
                        b'r' => output.push('\r'),
                        b't' => output.push('\t'),
                        b'u' => {
                            let code = self.hex16()?;
                            let character = char::from_u32(u32::from(code))
                                .ok_or_else(|| self.error("invalid Unicode escape"))?;
                            output.push(character);
                        }
                        _ => return Err(self.error("invalid string escape")),
                    }
                }
                byte if byte >= 0x20 => output.push(byte as char),
                _ => return Err(self.error("control byte in string")),
            }
        }
    }

    fn number(&mut self) -> Result<u64, ConformanceError> {
        let start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position += 1;
        }
        std::str::from_utf8(&self.input[start..self.position])
            .expect("digits are ASCII")
            .parse()
            .map_err(|_| self.error("integer out of range"))
    }

    fn hex16(&mut self) -> Result<u16, ConformanceError> {
        let mut value = 0u16;
        for _ in 0..4 {
            let byte = self
                .take()
                .ok_or_else(|| self.error("short Unicode escape"))?;
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(self.error("bad Unicode escape")),
            };
            value = (value << 4) | u16::from(digit);
        }
        Ok(value)
    }

    fn literal(&mut self, expected: &[u8]) -> Result<(), ConformanceError> {
        if self
            .input
            .get(self.position..self.position + expected.len())
            != Some(expected)
        {
            return Err(self.error("invalid literal"));
        }
        self.position += expected.len();
        Ok(())
    }

    fn whitespace(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.position += 1;
        }
    }

    fn expect(&mut self, expected: u8) -> Result<(), ConformanceError> {
        if self.consume(expected) {
            Ok(())
        } else {
            Err(self.error(&format!("expected {:?}", expected as char)))
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }

    fn error(&self, message: &str) -> ConformanceError {
        ConformanceError::Parse(format!("at byte {}: {message}", self.position))
    }
}

fn take(
    object: &mut BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<JsonValue, ConformanceError> {
    object
        .remove(key)
        .ok_or_else(|| ConformanceError::InvalidVector(format!("missing field {key:?}")))
}

fn take_string(
    object: &mut BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<String, ConformanceError> {
    take(object, key)?.into_string(key)
}

fn reject_extra(object: BTreeMap<String, JsonValue>, label: &str) -> Result<(), ConformanceError> {
    if object.is_empty() {
        Ok(())
    } else {
        Err(ConformanceError::InvalidVector(format!(
            "{label} contains unknown field(s): {}",
            object.keys().cloned().collect::<Vec<_>>().join(", ")
        )))
    }
}

fn number_u16(
    object: &mut BTreeMap<String, JsonValue>,
    key: &str,
    label: &str,
) -> Result<u16, ConformanceError> {
    bounded(
        take(object, key)?.into_number(key)?,
        u64::from(u16::MAX),
        &format!("{label}.{key}"),
    )
    .map(|value| value as u16)
}

fn number_u8(
    object: &mut BTreeMap<String, JsonValue>,
    key: &str,
    label: &str,
) -> Result<u8, ConformanceError> {
    bounded(
        take(object, key)?.into_number(key)?,
        u64::from(u8::MAX),
        &format!("{label}.{key}"),
    )
    .map(|value| value as u8)
}

fn number_bool(
    object: &mut BTreeMap<String, JsonValue>,
    key: &str,
    label: &str,
) -> Result<bool, ConformanceError> {
    match take(object, key)? {
        JsonValue::Number(value) if value <= 1 => Ok(value != 0),
        JsonValue::Bool(value) => Ok(value),
        _ => Err(ConformanceError::InvalidVector(format!(
            "{label}.{key} must be 0 or 1"
        ))),
    }
}

fn bounded(value: u64, maximum: u64, label: &str) -> Result<u64, ConformanceError> {
    if value <= maximum {
        Ok(value)
    } else {
        Err(ConformanceError::InvalidVector(format!(
            "{label} value {value} exceeds ${maximum:x}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTOR: &str = r#"[
      {"name":"lda","initial":{"pc":256,"s":511,"p":52,"a":18,"x":3,"y":4,"dbr":0,"d":0,"pbr":0,"e":1,"ram":[[256,169],[257,90]]},
       "final":{"pc":258,"s":511,"p":52,"a":90,"x":3,"y":4,"dbr":0,"d":0,"pbr":0,"e":1,"ram":[[256,169],[257,90]]},
       "cycles":[[256,169,"p"]]}
    ]"#;

    const NATIVE_STACK_OVERLAP_VECTOR: &str = r#"[
      {"name":"native stack overlaps historical scratch","initial":{"pc":256,"s":32781,"p":52,"a":18,"x":3,"y":4,"dbr":0,"d":0,"pbr":0,"e":0,"ram":[[256,234]]},
       "final":{"pc":257,"s":32781,"p":52,"a":18,"x":3,"y":4,"dbr":0,"d":0,"pbr":0,"e":0,"ram":[[256,234]]},
       "cycles":[[256,234,"p"]]}
    ]"#;

    #[test]
    fn parses_vector_shape_and_rejects_unknown_fields() {
        let file = VectorFile::parse(VECTOR).unwrap();
        assert_eq!(file.vectors[0].name, "lda");
        assert_eq!(file.vectors[0].initial.ram[&0x100], 0xa9);
        assert!(VectorFile::parse(
            &VECTOR.replace("\"name\":\"lda\"", "\"extra\":1,\"name\":\"lda\"")
        )
        .is_err());
    }

    #[test]
    fn runs_a_vector_and_compares_cpu_and_memory_state() {
        let file = VectorFile::parse(VECTOR).unwrap();
        let report = run_vector(&file.vectors[0]);
        assert_eq!(
            report.status,
            VectorStatus::Passed,
            "{:?}",
            report.mismatches
        );
    }

    #[test]
    fn moves_scratch_when_native_bootstrap_stack_overlaps_it() {
        let file = VectorFile::parse(NATIVE_STACK_OVERLAP_VECTOR).unwrap();
        let vector = &file.vectors[0];

        assert_eq!(
            choose_scratch(&vector.initial, &vector.final_state),
            Some(SCRATCH_PREFERRED)
        );
        assert_eq!(run_vector(vector).status, VectorStatus::Passed);
    }

    #[test]
    fn scratch_excludes_target_pc_and_ram_and_emulation_stack_pages() {
        let initial = VectorState {
            pc: 0x9001,
            s: 0x8000,
            p: 0x34,
            a: 0,
            x: 0,
            y: 0,
            dbr: 0,
            d: 0,
            pbr: 0,
            e: true,
            ram: [(0x9100, 0)].into_iter().collect(),
        };
        let final_state = initial.clone();

        let scratch = choose_scratch(&initial, &final_state).expect("scratch slot");
        let bootstrap_range = scratch..scratch + BOOTSTRAP_SLOT_SIZE;
        assert!(!bootstrap_range.contains(&u32::from(initial.pc)));
        assert!(initial
            .ram
            .keys()
            .chain(final_state.ram.keys())
            .all(|address| !bootstrap_range.contains(address)));
        assert!((0x000100..0x000200).all(|address| !bootstrap_range.contains(&address)));
    }
}
