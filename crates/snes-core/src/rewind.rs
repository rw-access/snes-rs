//! Bounded deterministic rewind checkpoints.

use std::collections::VecDeque;

use crate::console::{Console, ConsoleState, RestoreError};

/// A bounded history of complete-console snapshots.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RewindBuffer {
    capacity: usize,
    states: VecDeque<ConsoleState>,
}

impl RewindBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            states: VecDeque::with_capacity(capacity),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Records the current complete machine state, retaining the newest
    /// checkpoints when the configured capacity is reached.
    pub fn capture(&mut self, console: &Console) {
        if self.capacity == 0 {
            return;
        }
        if self.states.len() == self.capacity {
            self.states.pop_front();
        }
        self.states.push_back(console.snapshot());
    }

    /// Restores and discards the newest checkpoint.
    ///
    /// Callers normally capture at frame boundaries before advancing. A
    /// sequence of calls therefore walks backward through retained history.
    pub fn rewind(&mut self, console: &mut Console) -> Result<bool, RestoreError> {
        let Some(target) = self.states.back().cloned() else {
            return Ok(false);
        };
        console.restore(&target)?;
        self.states.pop_back();
        Ok(true)
    }

    pub fn clear(&mut self) {
        self.states.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cartridge::RomImage, Address24};

    #[test]
    fn rewind_restores_complete_machine_checkpoint() {
        let mut rom = vec![0xea; 0x8000];
        rom[0x7ffc] = 0;
        rom[0x7ffd] = 0x80;
        let mut console = Console::new(RomImage::new(rom)).unwrap();
        console.reset();
        let mut rewind = RewindBuffer::new(3);
        rewind.capture(&console);
        console.run_for_master_ticks(100);
        let changed = console.snapshot();
        assert!(changed.timing.master_ticks > 0);
        rewind.capture(&console);
        console.run_for_master_ticks(100);
        assert!(rewind.rewind(&mut console).unwrap());
        assert_eq!(console.snapshot(), changed);
        assert_eq!(console.read_memory(Address24::new(0x7e0000)), 0);
    }
}
