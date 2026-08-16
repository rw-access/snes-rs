//! Shared bus and address types.

/// A 24-bit SNES bus address.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Address24(pub u32);

impl Address24 {
    /// The largest address representable on the SNES bus.
    pub const MAX: Self = Self(0x00ff_ffff);

    /// Creates an address, retaining only the bus-visible 24 bits.
    pub const fn new(value: u32) -> Self {
        Self(value & Self::MAX.0)
    }

    /// Returns the raw 24-bit value.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Logical kind of a bus access, kept separate from the access address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BusAccessKind {
    OpcodeFetch,
    OperandFetch,
    DataRead,
    DataWrite,
    DummyRead,
    DummyWrite,
    Internal,
    VectorFetch,
}

/// One observable access in an instruction's ordered execution plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BusAccess {
    pub kind: BusAccessKind,
    pub address: Address24,
    pub value: Option<u8>,
}

/// Receives ordered bus accesses when diagnostic tracing is enabled.
pub trait TraceSink {
    fn record(&mut self, access: BusAccess);
}

/// Simple collecting trace sink for tests and diagnostics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VecTrace {
    accesses: Vec<BusAccess>,
}

impl VecTrace {
    pub fn as_slice(&self) -> &[BusAccess] {
        &self.accesses
    }

    pub fn into_inner(self) -> Vec<BusAccess> {
        self.accesses
    }
}

impl TraceSink for VecTrace {
    fn record(&mut self, access: BusAccess) {
        self.accesses.push(access);
    }
}

/// Coarse bus-speed class used by the future timing domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessSpeed {
    Fast,
    Slow,
}
