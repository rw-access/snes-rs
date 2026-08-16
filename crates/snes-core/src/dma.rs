//! DMA and HDMA ownership boundary.

/// One of the eight future general-DMA channels.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DmaChannel {
    pub index: u8,
}

/// DMA controller state boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Dma;
