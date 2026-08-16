//! Instruction metadata boundary.

/// Stable identifier used by CPU steps and future trace output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstructionId {
    Placeholder,
}
