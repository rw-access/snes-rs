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
    pub bg2sc: u8,
    pub bg3sc: u8,
    pub bg4sc: u8,
    pub bg12nba: u8,
    pub bg34nba: u8,
    pub vmain: u8,
    pub vram_address: u16,
    pub cgram_address: u8,
    pub oam_address: u16,
    pub tm: u8,
    pub ts: u8,
    pub obsel: u8,
    pub cgwsel: u8,
    pub cgadsub: u8,
    pub fixed_color: u16,
    pub bg_hofs: [u16; 4],
    pub bg_vofs: [u16; 4],
    pub vram: [u16; 32 * 1024],
    pub cgram: [u16; 256],
    pub oam: [u8; 544],
    vram_read_latch: u16,
    cgram_write_latch: Option<u8>,
    cgram_read_latch: Option<u8>,
    bg_offset_latches: [Option<u8>; 8],
}

impl Default for Ppu {
    fn default() -> Self {
        Self {
            inidisp: 0x80,
            bgmode: 0,
            bg1sc: 0,
            bg2sc: 0,
            bg3sc: 0,
            bg4sc: 0,
            bg12nba: 0,
            bg34nba: 0,
            vmain: 0,
            vram_address: 0,
            cgram_address: 0,
            oam_address: 0,
            tm: 0,
            ts: 0,
            obsel: 0,
            cgwsel: 0,
            cgadsub: 0,
            fixed_color: 0,
            bg_hofs: [0; 4],
            bg_vofs: [0; 4],
            vram: [0; 32 * 1024],
            cgram: [0; 256],
            oam: [0; 544],
            vram_read_latch: 0,
            cgram_write_latch: None,
            cgram_read_latch: None,
            bg_offset_latches: [None; 8],
        }
    }
}

impl Ppu {
    pub fn read_register(&mut self, address: u16) -> u8 {
        match address {
            0x2139 => self.vram_read_latch as u8,
            0x213a => {
                let value = (self.vram_read_latch >> 8) as u8;
                self.increment_vram_address();
                value
            }
            0x213b => {
                if self.cgram_read_latch.is_some() {
                    let value = (self.cgram[self.cgram_address as usize] >> 8) as u8;
                    self.cgram_address = self.cgram_address.wrapping_add(1);
                    self.cgram_read_latch = None;
                    value
                } else {
                    let value = self.cgram[self.cgram_address as usize] as u8;
                    self.cgram_read_latch = Some(value);
                    value
                }
            }
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
            0x2101 => self.obsel = value,
            0x2105 => self.bgmode = value,
            0x2107 => self.bg1sc = value,
            0x2108 => self.bg2sc = value,
            0x2109 => self.bg3sc = value,
            0x210a => self.bg4sc = value,
            0x210b => self.bg12nba = value,
            0x210c => self.bg34nba = value,
            0x210d..=0x2114 => self.write_bg_offset(address, value),
            0x2115 => self.vmain = value,
            0x2116 => {
                self.vram_address = (self.vram_address & 0xff00) | u16::from(value);
                self.refresh_vram_latch();
            }
            0x2117 => {
                self.vram_address = (self.vram_address & 0x00ff) | (u16::from(value) << 8);
                self.refresh_vram_latch();
            }
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
                self.cgram_read_latch = None;
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
            0x2130 => self.cgwsel = value,
            0x2131 => self.cgadsub = value,
            0x2132 => {
                let component = u16::from(value & 0x1f);
                if value & 0x20 != 0 {
                    self.fixed_color = (self.fixed_color & !0x001f) | component;
                }
                if value & 0x40 != 0 {
                    self.fixed_color = (self.fixed_color & !0x03e0) | (component << 5);
                }
                if value & 0x80 != 0 {
                    self.fixed_color = (self.fixed_color & !0x7c00) | (component << 10);
                }
            }
            _ => {}
        }
    }

    /// Renders the deterministic, non-interlaced Mode 0 main screen.
    ///
    /// Mode 0 has four 2bpp backgrounds.  Keeping the renderer pixel based
    /// makes its priority behavior explicit and gives visual tests a stable
    /// seam while scanline timing is added to the machine clock.
    pub fn render_frame(&mut self, pixels: &mut [u32]) {
        let backdrop = self.color_to_rgba(self.cgram[0]);
        pixels.fill(if self.inidisp & 0x80 != 0 {
            0xff00_0000
        } else {
            backdrop
        });
        if self.inidisp & 0x80 != 0 || self.bgmode & 0x07 != 0 {
            return;
        }

        for y in 0..SCREEN_HEIGHT {
            for x in 0..SCREEN_WIDTH {
                let mut selected = self.cgram[0];
                let mut selected_priority = 0u8;
                for &(layer, high, priority) in &[
                    (3usize, false, 1u8),
                    (2, false, 2),
                    (1, false, 3),
                    (0, false, 4),
                    (3, true, 5),
                    (2, true, 6),
                    (1, true, 7),
                    (0, true, 8),
                ] {
                    if self.tm & (1 << layer) == 0 {
                        continue;
                    }
                    let (color, tile_priority) = self.bg_pixel(layer, x, y, high);
                    if color != 0 && tile_priority == high && priority >= selected_priority {
                        selected = self.cgram[layer * 32 + usize::from(color)];
                        selected_priority = priority;
                    }
                }
                pixels[y * SCREEN_WIDTH + x] = self.color_to_rgba(self.apply_color_math(selected));
            }
        }
    }

    fn bg_pixel(&self, layer: usize, x: usize, y: usize, high: bool) -> (u8, bool) {
        let screen = self.bg_screen(layer);
        let scroll_x = usize::from(self.bg_hofs[layer]);
        let scroll_y = usize::from(self.bg_vofs[layer]);
        let sx = (x + scroll_x) & 0x1ff;
        let sy = (y + scroll_y) & 0x1ff;
        let tile_x = sx / 8;
        let tile_y = sy / 8;
        let map_base = (usize::from(screen & 0xfc) << 8) & 0x7fff;
        let map_size_x = if screen & 0x01 != 0 { 64 } else { 32 };
        let map_size_y = if screen & 0x02 != 0 { 64 } else { 32 };
        let map_x = tile_x % map_size_x;
        let map_y = tile_y % map_size_y;
        let quadrant = (map_x / 32) + 2 * (map_y / 32);
        let map_index = (map_base + quadrant * 0x400 + (map_y & 31) * 32 + (map_x & 31)) & 0x7fff;
        let entry = self.vram[map_index];
        if ((entry >> 13) & 1 != 0) != high {
            return (0, false);
        }
        let mut row = sy & 7;
        let mut column = sx & 7;
        if entry & 0x8000 != 0 {
            row = 7 - row;
        }
        if entry & 0x4000 != 0 {
            column = 7 - column;
        }
        let tile_base = self.bg_tile_base(layer);
        let row_base = (tile_base + usize::from(entry & 0x03ff) * 16 + row * 2) & 0x7fff;
        let bit = 7 - column;
        let color = ((self.vram[row_base] >> bit) & 1)
            | (((self.vram[(row_base + 1) & 0x7fff] >> bit) & 1) << 1);
        if color == 0 {
            (0, false)
        } else {
            let palette = usize::from((entry >> 10) & 0x07);
            ((palette * 4 + usize::from(color)) as u8, high)
        }
    }

    fn bg_screen(&self, layer: usize) -> u8 {
        match layer {
            0 => self.bg1sc,
            1 => self.bg2sc,
            2 => self.bg3sc,
            _ => self.bg4sc,
        }
    }

    fn bg_tile_base(&self, layer: usize) -> usize {
        let nibble = match layer {
            0 => self.bg12nba & 0x0f,
            1 => self.bg12nba >> 4,
            2 => self.bg34nba & 0x0f,
            _ => self.bg34nba >> 4,
        };
        usize::from(nibble) << 12
    }

    fn apply_color_math(&self, color: u16) -> u16 {
        if self.cgadsub & 0x20 == 0 {
            return color;
        }
        let fixed = self.fixed_color;
        let add = self.cgadsub & 0x80 == 0;
        let half = self.cgadsub & 0x40 != 0;
        let channel = |left: u16, right: u16| -> u16 {
            let value = if add {
                u16::from((left + right).min(31))
            } else {
                left.saturating_sub(right)
            };
            if half {
                value / 2
            } else {
                value
            }
        };
        channel(color & 0x1f, fixed & 0x1f)
            | (channel((color >> 5) & 0x1f, (fixed >> 5) & 0x1f) << 5)
            | (channel((color >> 10) & 0x1f, (fixed >> 10) & 0x1f) << 10)
    }

    fn write_bg_offset(&mut self, address: u16, value: u8) {
        let register = usize::from(address - 0x210d);
        let index = register / 2;
        let is_vertical = register % 2 != 0;
        if let Some(low) = self.bg_offset_latches[index].take() {
            let combined = u16::from(low) | (u16::from(value) << 8);
            if is_vertical {
                self.bg_vofs[index] = combined;
            } else {
                self.bg_hofs[index] = combined;
            }
        } else {
            self.bg_offset_latches[index] = Some(value);
        }
    }

    fn increment_vram_address(&mut self) {
        let increment = match self.vmain & 0x03 {
            1 => 32,
            2 | 3 => 128,
            _ => 1,
        };
        self.vram_address = self.vram_address.wrapping_add(increment);
        self.refresh_vram_latch();
    }

    fn refresh_vram_latch(&mut self) {
        self.vram_read_latch = self.vram[self.vram_address as usize & 0x7fff];
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
        ppu.refresh_vram_latch();
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
        ppu.write_register(0x2121, 2);
        assert_eq!(ppu.read_register(0x213b), 0x1f);
        assert_eq!(ppu.read_register(0x213b), 0x03);
    }

    #[test]
    fn mode_zero_composes_four_two_bpp_backgrounds() {
        let mut ppu = Ppu::default();
        ppu.inidisp = 0;
        ppu.tm = 0x0f;
        ppu.cgram[1] = 0x001f;
        ppu.cgram[33] = 0x03e0;
        ppu.cgram[65] = 0x7c00;
        ppu.cgram[97] = 0x7fff;

        // One opaque pixel in tile 1 for each layer.  The four map bases are
        // distinct 32x32 map regions in VRAM.
        for layer in 0..4 {
            let tile_base = layer * 16;
            ppu.vram[tile_base + 16] = 0x0080;
            ppu.vram[layer * 0x400] = 1;
        }

        assert_eq!(ppu.vram[0], 1);
        assert_eq!(ppu.vram[16], 0x0080);
        assert_eq!(ppu.bg_pixel(0, 0, 0, false), (1, false));

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];
        ppu.render_frame(&mut frame);

        // BG1 has the highest normal priority and its palette starts at 0.
        assert_eq!(frame[0], ppu.color_to_rgba(0x001f));
        assert_ne!(frame[8], frame[0]);
    }

    #[test]
    fn fixed_color_math_adds_selected_channels() {
        let mut ppu = Ppu::default();
        ppu.cgadsub = 0x20;
        ppu.fixed_color = 0x0010;
        assert_eq!(ppu.apply_color_math(0x0010), 0x001f);

        ppu.cgadsub = 0xa0;
        assert_eq!(ppu.apply_color_math(0x0010), 0);
    }
}
