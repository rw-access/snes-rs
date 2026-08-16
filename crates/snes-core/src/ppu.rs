//! PPU register, memory, and baseline renderer state.

/// The visible SNES frame used by the headless renderer.
pub const SCREEN_WIDTH: usize = 256;
pub const SCREEN_HEIGHT: usize = 224;

/// State for the registers and memory directly owned by the two PPUs.
///
/// This deliberately models the ordinary background path first.  The state is
/// complete enough for deterministic register tests and for simple Mode 0
/// diagnostic ROMs; sprite, window, color math, and Mode 7 behavior are added
/// as separate increments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ppu {
    pub inidisp: u8,
    pub bgmode: u8,
    pub bg1sc: u8,
    pub bg12nba: u8,
    pub vmain: u8,
    pub vram_address: u16,
    pub cgram_address: u8,
    pub oam_address: u16,
    pub tm: u8,
    pub ts: u8,
    pub vram: [u16; 32 * 1024],
    pub cgram: [u16; 256],
    pub oam: [u8; 544],
    vram_read_latch: u16,
    cgram_write_latch: Option<u8>,
}

impl Default for Ppu {
    fn default() -> Self {
        Self {
            inidisp: 0x80,
            bgmode: 0,
            bg1sc: 0,
            bg12nba: 0,
            vmain: 0,
            vram_address: 0,
            cgram_address: 0,
            oam_address: 0,
            tm: 0,
            ts: 0,
            vram: [0; 32 * 1024],
            cgram: [0; 256],
            oam: [0; 544],
            vram_read_latch: 0,
            cgram_write_latch: None,
        }
    }
}

impl Ppu {
    pub fn read_register(&mut self, address: u16) -> u8 {
        match address {
            0x2139 => self.vram_read_latch as u8,
            0x213a => {
                let value = (self.vram_read_latch >> 8) as u8;
                self.vram_read_latch = self.vram[self.vram_address as usize & 0x7fff];
                self.increment_vram_address();
                value
            }
            0x213b => self.cgram[self.cgram_address as usize] as u8,
            0x2100 => self.inidisp,
            0x2105 => self.bgmode,
            0x212c => self.tm,
            0x212d => self.ts,
            _ => 0,
        }
    }

    pub fn write_register(&mut self, address: u16, value: u8) {
        match address {
            0x2100 => self.inidisp = value,
            0x2105 => self.bgmode = value,
            0x2107 => self.bg1sc = value,
            0x210b => self.bg12nba = value,
            0x2115 => self.vmain = value,
            0x2116 => self.vram_address = (self.vram_address & 0xff00) | u16::from(value),
            0x2117 => self.vram_address = (self.vram_address & 0x00ff) | (u16::from(value) << 8),
            0x2118 => {
                let index = self.vram_address as usize & 0x7fff;
                self.vram[index] = (self.vram[index] & 0xff00) | u16::from(value);
                if self.vmain & 0x80 == 0 {
                    self.increment_vram_address();
                }
            }
            0x2119 => {
                let index = self.vram_address as usize & 0x7fff;
                self.vram[index] = (self.vram[index] & 0x00ff) | (u16::from(value) << 8);
                if self.vmain & 0x80 != 0 {
                    self.increment_vram_address();
                }
            }
            0x2121 => {
                self.cgram_address = value;
                self.cgram_write_latch = None;
            }
            0x2122 => {
                if let Some(low) = self.cgram_write_latch.take() {
                    self.cgram[self.cgram_address as usize] =
                        u16::from(low) | (u16::from(value & 0x7f) << 8);
                    self.cgram_address = self.cgram_address.wrapping_add(1);
                } else {
                    self.cgram_write_latch = Some(value);
                }
            }
            0x2102 => self.oam_address = (self.oam_address & 0x100) | u16::from(value),
            0x2103 => self.oam_address = (self.oam_address & 0x0ff) | (u16::from(value & 1) << 8),
            0x2104 => {
                self.oam[self.oam_address as usize % self.oam.len()] = value;
                self.oam_address = (self.oam_address + 1) % self.oam.len() as u16;
            }
            0x212c => self.tm = value,
            0x212d => self.ts = value,
            _ => {}
        }
    }

    /// Renders the ordinary 4bpp BG1 path used by Mode 0 diagnostics.
    pub fn render_frame(&mut self, pixels: &mut [u32]) {
        let backdrop = self.color_to_rgba(self.cgram[0]);
        pixels.fill(if self.inidisp & 0x80 != 0 { 0xff00_0000 } else { backdrop });
        if self.inidisp & 0x80 != 0 || self.bgmode & 0x07 != 0 || self.tm & 0x01 == 0 {
            return;
        }

        let map_base = (usize::from(self.bg1sc & 0xfc) << 8) & 0x7fff;
        let tile_base = (usize::from(self.bg12nba & 0x0f) << 11) & 0x7fff;
        for y in 0..SCREEN_HEIGHT {
            for x in 0..SCREEN_WIDTH {
                let tile_x = x / 8;
                let tile_y = y / 8;
                let map_index = (map_base + tile_y * 32 + tile_x) & 0x7fff;
                let entry = self.vram[map_index];
                let tile = usize::from(entry & 0x03ff);
                let palette = usize::from((entry >> 10) & 0x07);
                let row = y & 7;
                let column = x & 7;
                let row_base = (tile_base + tile * 16 + row * 2) & 0x7fff;
                let plane0 = self.vram[row_base];
                let plane1 = self.vram[(row_base + 1) & 0x7fff];
                let plane2 = self.vram[(row_base + 8) & 0x7fff];
                let plane3 = self.vram[(row_base + 9) & 0x7fff];
                let bit = 7 - column;
                let color = ((plane0 >> bit) & 1)
                    | (((plane1 >> bit) & 1) << 1)
                    | (((plane2 >> bit) & 1) << 2)
                    | (((plane3 >> bit) & 1) << 3);
                if color != 0 {
                    let cgram_index = palette * 16 + usize::from(color);
                    pixels[y * SCREEN_WIDTH + x] = self.color_to_rgba(self.cgram[cgram_index]);
                }
            }
        }
    }

    fn increment_vram_address(&mut self) {
        let increment = match self.vmain & 0x03 {
            1 => 32,
            2 | 3 => 128,
            _ => 1,
        };
        self.vram_address = self.vram_address.wrapping_add(increment);
    }

    fn color_to_rgba(&self, color: u16) -> u32 {
        let red = u32::from((color & 0x1f) as u8);
        let green = u32::from(((color >> 5) & 0x1f) as u8);
        let blue = u32::from(((color >> 10) & 0x1f) as u8);
        0xff00_0000
            | ((red << 3 | red >> 2) << 16)
            | ((green << 3 | green >> 2) << 8)
            | (blue << 3 | blue >> 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vram_registers_round_trip_words() {
        let mut ppu = Ppu::default();
        ppu.write_register(0x2115, 0x80);
        ppu.write_register(0x2116, 3);
        ppu.write_register(0x2117, 0);
        ppu.write_register(0x2118, 0x34);
        ppu.write_register(0x2119, 0x12);
        assert_eq!(ppu.vram[3], 0x1234);
        ppu.vram_address = 3;
        ppu.vram_read_latch = ppu.vram[3];
        assert_eq!(ppu.read_register(0x2139), 0x34);
        assert_eq!(ppu.read_register(0x213a), 0x12);
    }

    #[test]
    fn cgram_registers_round_trip_colors() {
        let mut ppu = Ppu::default();
        ppu.write_register(0x2121, 2);
        ppu.write_register(0x2122, 0x1f);
        ppu.write_register(0x2122, 0x03);
        assert_eq!(ppu.cgram[2], 0x031f);
    }
}
