//! PPU register, memory, and baseline renderer state.

/// The visible SNES frame used by the headless renderer.
pub const SCREEN_WIDTH: usize = 256;
pub const SCREEN_HEIGHT: usize = 224;

/// State for the registers and memory directly owned by the two PPUs.
///
/// This deliberately models the ordinary background path first.  The state is
/// complete enough for deterministic register tests and for simple Mode 0
/// diagnostic ROMs.  Sprite composition is also modeled for the ordinary
/// non-interlaced path; scanline limits and timing are intentionally outside
/// this renderer.
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
    pub tmw: u8,
    pub tsw: u8,
    pub obsel: u8,
    pub w12sel: u8,
    pub w34sel: u8,
    pub wobjsel: u8,
    pub wh: [u8; 4],
    pub wbglog: u8,
    pub wobjlog: u8,
    pub m7sel: u8,
    pub m7a: i16,
    pub m7b: i16,
    pub m7c: i16,
    pub m7d: i16,
    pub m7x: i16,
    pub m7y: i16,
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
    mode7_latches: [Option<u8>; 6],
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
            tmw: 0,
            tsw: 0,
            obsel: 0,
            w12sel: 0,
            w34sel: 0,
            wobjsel: 0,
            wh: [0; 4],
            wbglog: 0,
            wobjlog: 0,
            m7sel: 0,
            m7a: 0x0100,
            m7b: 0,
            m7c: 0,
            m7d: 0x0100,
            m7x: 0,
            m7y: 0,
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
            mode7_latches: [None; 6],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OamSprite {
    x: i16,
    y: u8,
    tile: u8,
    palette: u8,
    priority: u8,
    name_select: bool,
    h_flip: bool,
    v_flip: bool,
    size_large: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScreenPixel {
    color: u16,
    source: u8,
    priority: u8,
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
            0x2123 => self.w12sel,
            0x2124 => self.w34sel,
            0x2125 => self.wobjsel,
            0x2126..=0x2129 => self.wh[usize::from(address - 0x2126)],
            0x212a => self.wbglog,
            0x212b => self.wobjlog,
            0x212e => self.tmw,
            0x212f => self.tsw,
            0x2130 => self.cgwsel,
            0x2131 => self.cgadsub,
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
            0x211a..=0x2120 => self.write_mode7_register(address, value),
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
            0x2123 => self.w12sel = value,
            0x2124 => self.w34sel = value,
            0x2125 => self.wobjsel = value,
            0x2126..=0x2129 => self.wh[usize::from(address - 0x2126)] = value,
            0x212a => self.wbglog = value,
            0x212b => self.wobjlog = value,
            0x212e => self.tmw = value,
            0x212f => self.tsw = value,
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

    /// Renders a deterministic, non-interlaced frame.
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
        if self.inidisp & 0x80 != 0 {
            return;
        }

        match self.bgmode & 0x07 {
            0 | 1 | 2 => self.render_tile_mode(pixels, self.bgmode & 0x07),
            7 => self.render_mode7(pixels),
            _ => {}
        }
    }

    fn render_tile_mode(&self, pixels: &mut [u32], mode: u8) {
        for y in 0..SCREEN_HEIGHT {
            for x in 0..SCREEN_WIDTH {
                let main = self.screen_pixel(x, y, mode, self.tm, self.tmw, false);
                let sub = self.screen_pixel(x, y, mode, self.ts, self.tsw, true);
                let color = self.compose_screens(main, sub, x);
                pixels[y * SCREEN_WIDTH + x] = self.color_to_rgba(color);
            }
        }
    }

    fn screen_pixel(
        &self,
        x: usize,
        y: usize,
        mode: u8,
        layer_mask: u8,
        window_mask: u8,
        subscreen: bool,
    ) -> ScreenPixel {
        let mut selected = ScreenPixel {
            color: if subscreen {
                self.fixed_color
            } else {
                self.cgram[0]
            },
            source: 0x20, // backdrop
            priority: 0,
        };
        let layers = match mode {
            1 => 3,
            2 => 2,
            _ => 4,
        };
        for &(layer, high) in &[
            (3usize, false),
            (2, false),
            (1, false),
            (0, false),
            (3, true),
            (2, true),
            (1, true),
            (0, true),
        ] {
            if layer >= layers || layer_mask & (1 << layer) == 0 {
                continue;
            }
            if window_mask & (1 << layer) != 0 && self.window_hides(layer, x) {
                continue;
            }
            let (color, tile_priority) = self.bg_pixel_for_mode(layer, x, y, high, mode);
            let priority = self.bg_priority(mode, layer, tile_priority);
            if color != 0 && tile_priority == high && priority > selected.priority {
                let palette_index = if mode == 0 {
                    layer * 32 + usize::from(color)
                } else {
                    usize::from(color)
                };
                selected = ScreenPixel {
                    color: self.cgram[palette_index],
                    source: 1 << layer,
                    priority,
                };
            }
        }

        if layer_mask & 0x10 != 0 {
            if let Some((color, priority, _oam_index)) = self.obj_pixel(x, y, window_mask) {
                if priority > selected.priority {
                    selected = ScreenPixel {
                        color,
                        source: 0x10,
                        priority,
                    };
                }
            }
        }
        selected
    }

    fn compose_screens(&self, main: ScreenPixel, sub: ScreenPixel, x: usize) -> u16 {
        let mut color = if self.main_color_window_applies(x) {
            0
        } else {
            main.color
        };
        if self.cgadsub & main.source != 0 && self.color_math_allowed(x) {
            let operand = if self.cgwsel & 0x02 != 0 {
                sub.color
            } else {
                self.fixed_color
            };
            color = self.apply_color_math(color, operand);
        }
        color
    }

    fn obj_pixel(&self, x: usize, y: usize, window_mask: u8) -> Option<(u16, u8, usize)> {
        if window_mask & 0x10 != 0 && self.window_hides(4, x) {
            return None;
        }
        let mut selected: Option<(u16, u8, usize)> = None;
        for index in 0..128 {
            let sprite = self.decode_sprite(index);
            let (width, height) = self.obj_dimensions(sprite.size_large);
            let local_x = x as i16 - sprite.x;
            if local_x < 0 || local_x >= width as i16 {
                continue;
            }

            // OAM Y coordinates wrap at 256.  This also makes sprites whose
            // bottom crosses the visible region deterministic without trying
            // to model the PPU's per-scanline evaluation limits.
            let local_y = (y as i16 - i16::from(sprite.y)).rem_euclid(256) as usize;
            if local_y >= height {
                continue;
            }

            let draw_x = if sprite.h_flip {
                width - 1 - local_x as usize
            } else {
                local_x as usize
            };
            let draw_y = if sprite.v_flip {
                height - 1 - local_y
            } else {
                local_y
            };
            let tile_x = draw_x / 8;
            let tile_y = draw_y / 8;
            let tile = sprite.tile.wrapping_add((tile_x + tile_y * 16) as u8);
            let row = draw_y & 7;
            let column = draw_x & 7;
            let name_select = usize::from((self.obsel >> 3) & 0x03) << 12;
            let table_base = usize::from(self.obsel & 0x07) << 13;
            let table_base = table_base + if sprite.name_select { name_select } else { 0 };
            let tile_base = (table_base + usize::from(tile) * 16) & 0x7fff;
            let row_base = (tile_base + row * 2) & 0x7fff;
            let bit = 7 - column;
            let color = ((self.vram[row_base] >> bit) & 1)
                | (((self.vram[(row_base + 1) & 0x7fff] >> bit) & 1) << 1)
                | (((self.vram[(tile_base + 8 + row * 2) & 0x7fff] >> bit) & 1) << 2)
                | (((self.vram[(tile_base + 8 + row * 2 + 1) & 0x7fff] >> bit) & 1) << 3);
            if color == 0 {
                continue;
            }

            let priority = Self::mode0_obj_priority(sprite.priority);
            let palette = usize::from(sprite.palette);
            let rgba_color = self.cgram[128 + palette * 16 + usize::from(color)];
            match selected {
                Some((_, old_priority, old_index))
                    if old_priority > priority
                        || (old_priority == priority && old_index < index) => {}
                _ => selected = Some((rgba_color, priority, index)),
            }
        }
        selected
    }

    fn decode_sprite(&self, index: usize) -> OamSprite {
        let base = index * 4;
        let x_low = u16::from(self.oam[base]);
        let y = self.oam[base + 1];
        let tile = self.oam[base + 2];
        let attributes = self.oam[base + 3];
        let high = self.oam[512 + index / 4];
        let shift = (index % 4) * 2;
        let x = x_low | (u16::from((high >> shift) & 1) << 8);
        OamSprite {
            x: if x >= 256 { x as i16 - 512 } else { x as i16 },
            y,
            tile,
            palette: (attributes >> 1) & 0x07,
            priority: (attributes >> 4) & 0x03,
            name_select: attributes & 1 != 0,
            h_flip: attributes & 0x40 != 0,
            v_flip: attributes & 0x80 != 0,
            size_large: high >> shift & 0x02 != 0,
        }
    }

    fn obj_dimensions(&self, large: bool) -> (usize, usize) {
        // OBSEL bits 5-7 select the pair of ordinary/large OBJ sizes.
        const SIZES: [[(usize, usize); 2]; 8] = [
            [(8, 8), (16, 16)],
            [(8, 8), (32, 32)],
            [(8, 8), (64, 64)],
            [(16, 16), (32, 32)],
            [(16, 16), (64, 64)],
            [(32, 32), (64, 64)],
            [(16, 32), (32, 64)],
            [(16, 32), (32, 32)],
        ];
        SIZES[usize::from((self.obsel >> 5) & 0x07)][usize::from(large)]
    }

    fn bg_priority(&self, mode: u8, layer: usize, high: bool) -> u8 {
        if mode == 1 {
            return match (layer, high) {
                (0, true) => 9,
                (1, true) => 8,
                (0, false) => 4,
                (1, false) => 3,
                (2, true) if self.bgmode & 0x08 != 0 => 13,
                (2, true) => 2,
                (2, false) => 1,
                _ => 0,
            };
        }
        if mode == 2 {
            return match (layer, high) {
                (0, true) => 11, // 1H
                (1, true) => 8,  // 2H
                (0, false) => 5, // 1L
                (1, false) => 2, // 2L
                _ => 0,
            };
        }
        match (layer, high) {
            (0, true) => 11, // 1H
            (1, true) => 10, // 2H
            (0, false) => 8, // 1L
            (1, false) => 7, // 2L
            (2, true) => 5,  // 3H
            (3, true) => 4,  // 4H
            (2, false) => 2, // 3L
            (3, false) => 1, // 4L
            _ => 0,
        }
    }

    fn mode0_obj_priority(priority: u8) -> u8 {
        match priority {
            3 => 12, // S3
            2 => 9,  // S2
            1 => 6,  // S1
            _ => 3,  // S0
        }
    }

    fn bg_pixel_for_mode(
        &self,
        layer: usize,
        x: usize,
        y: usize,
        high: bool,
        mode: u8,
    ) -> (u8, bool) {
        let screen = self.bg_screen(layer);
        let (scroll_x, scroll_y) = self.bg_scroll_for_mode(mode, layer, x);
        let sx = (x + scroll_x) & 0x1ff;
        let sy = (y + scroll_y) & 0x1ff;
        let tile_x = sx / 8;
        let tile_y = sy / 8;
        let entry = self.bg_map_entry(screen, tile_x, tile_y);
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
        let mut color = ((self.vram[row_base] >> bit) & 1)
            | (((self.vram[(row_base + 1) & 0x7fff] >> bit) & 1) << 1);
        let four_bpp = matches!(mode, 1 | 2) && layer < 2;
        if four_bpp {
            let tile_row_base = tile_base + usize::from(entry & 0x03ff) * 16 + 8 + row * 2;
            color |= (((self.vram[tile_row_base & 0x7fff] >> bit) & 1) << 2)
                | (((self.vram[(tile_row_base + 1) & 0x7fff] >> bit) & 1) << 3);
        }
        if color == 0 {
            (0, false)
        } else {
            let palette = usize::from((entry >> 10) & 0x07);
            let palette_width = if four_bpp { 16 } else { 4 };
            ((palette * palette_width + usize::from(color)) as u8, high)
        }
    }

    fn bg_scroll_for_mode(&self, mode: u8, layer: usize, x: usize) -> (usize, usize) {
        let normal = (
            usize::from(self.bg_hofs[layer]),
            usize::from(self.bg_vofs[layer]),
        );
        if mode != 2 || layer >= 2 {
            return normal;
        }

        let Some((offset, horizontal)) = self.mode2_offset(layer, x) else {
            return normal;
        };
        if horizontal {
            (
                usize::from((offset & 0x03f8) | (self.bg_hofs[layer] & 0x0007)),
                normal.1,
            )
        } else {
            (normal.0, usize::from(offset & 0x03ff))
        }
    }

    fn mode2_offset(&self, layer: usize, x: usize) -> Option<(u16, bool)> {
        // The first visible tile column is deliberately never offset.  The
        // first entry in the BG3 table therefore belongs to the next column;
        // adding the target layer's fine scroll handles a partially visible
        // first tile as well.
        let visible_column = (x + usize::from(self.bg_hofs[layer] & 0x0007)) / 8;
        let offset_column = visible_column.checked_sub(1)?;
        let bg3_hofs = usize::from(self.bg_hofs[2] & 0x03f8) / 8;
        let bg3_vofs = usize::from(self.bg_vofs[2] & 0x03f8) / 8;
        let map_x = bg3_hofs + offset_column;

        let horizontal = self.bg_map_entry(self.bg3sc, map_x, bg3_vofs);
        if horizontal & (1 << (13 + layer)) != 0 {
            return Some((horizontal, true));
        }

        // The vertical row is the row immediately below the horizontal row.
        // BG3VOFS selects the pair's starting row; its fine scroll is ignored.
        let vertical = self.bg_map_entry(self.bg3sc, map_x, bg3_vofs + 1);
        if vertical & (1 << (13 + layer)) != 0 {
            Some((vertical, false))
        } else {
            None
        }
    }

    fn bg_map_entry(&self, screen: u8, tile_x: usize, tile_y: usize) -> u16 {
        let map_base = (usize::from(screen & 0xfc) << 8) & 0x7fff;
        let map_size_x = if screen & 0x01 != 0 { 64 } else { 32 };
        let map_size_y = if screen & 0x02 != 0 { 64 } else { 32 };
        let map_x = tile_x % map_size_x;
        let map_y = tile_y % map_size_y;
        let quadrant = (map_x / 32) + 2 * (map_y / 32);
        let map_index = (map_base + quadrant * 0x400 + (map_y & 31) * 32 + (map_x & 31)) & 0x7fff;
        self.vram[map_index]
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

    fn apply_color_math_at(&self, color: u16, source: u8, x: usize) -> u16 {
        if self.cgadsub & source == 0 {
            return color;
        }
        if !self.color_math_allowed(x) {
            return color;
        }
        self.apply_color_math(color, self.fixed_color)
    }

    fn apply_color_math(&self, color: u16, operand: u16) -> u16 {
        let add = self.cgadsub & 0x80 == 0;
        let half = self.cgadsub & 0x40 != 0;
        let channel = |left: u16, right: u16| -> u16 {
            let value = if add {
                (left + right).min(31)
            } else {
                left.saturating_sub(right)
            };
            if half {
                value / 2
            } else {
                value
            }
        };
        channel(color & 0x1f, operand & 0x1f)
            | (channel((color >> 5) & 0x1f, (operand >> 5) & 0x1f) << 5)
            | (channel((color >> 10) & 0x1f, (operand >> 10) & 0x1f) << 10)
    }

    /// Returns whether a configured window hides `layer` at this pixel.
    /// Layers 0..=3 are BG1..BG4, layer 4 is OBJ, and layer 5 is the color
    /// window.  The result is the window effect (one means hide), matching
    /// the PPU's window-mask terminology.
    fn window_hides(&self, layer: usize, x: usize) -> bool {
        let (settings, logic, settings_shift) = match layer {
            0 => (self.w12sel, self.wbglog & 0x03, 0),
            1 => (self.w12sel, (self.wbglog >> 2) & 0x03, 4),
            2 => (self.w34sel, (self.wbglog >> 4) & 0x03, 0),
            3 => (self.w34sel, (self.wbglog >> 6) & 0x03, 4),
            4 => (self.wobjsel, self.wobjlog & 0x03, 0),
            5 => (self.wobjsel, (self.wobjlog >> 2) & 0x03, 4),
            _ => return false,
        };
        let first = self.window_signal(settings, 0, x, settings_shift);
        let second = self.window_signal(settings, 1, x, settings_shift);
        match logic {
            0 => first || second,
            1 => first && second,
            2 => first ^ second,
            _ => first == second,
        }
    }

    fn window_signal(&self, settings: u8, window: usize, x: usize, shift: u8) -> bool {
        let bits = (settings >> (shift + (window as u8 * 2))) & 0x03;
        if bits & 0x01 == 0 {
            return false;
        }
        let left = usize::from(self.wh[window * 2]);
        let right = usize::from(self.wh[window * 2 + 1]);
        let inside = left <= right && (left..=right).contains(&x);
        if bits & 0x02 != 0 {
            !inside
        } else {
            inside
        }
    }

    fn color_window_region(&self, x: usize) -> bool {
        self.window_hides(5, x)
    }

    fn region_applies(mode: u8, inside: bool) -> bool {
        match mode {
            0 => false,
            1 => !inside,
            2 => inside,
            _ => true,
        }
    }

    fn main_color_window_applies(&self, x: usize) -> bool {
        let mode = (self.cgwsel >> 6) & 0x03;
        Self::region_applies(mode, self.color_window_region(x))
    }

    fn color_math_allowed(&self, x: usize) -> bool {
        let mode = (self.cgwsel >> 4) & 0x03;
        !Self::region_applies(mode, self.color_window_region(x))
    }

    fn render_mode7(&self, pixels: &mut [u32]) {
        if self.tm & 0x01 == 0 {
            return;
        }
        let a = i32::from(self.m7a);
        let b = i32::from(self.m7b);
        let c = i32::from(self.m7c);
        let d = i32::from(self.m7d);
        let origin_x = i32::from(self.m7x);
        let origin_y = i32::from(self.m7y);
        for y in 0..SCREEN_HEIGHT {
            for x in 0..SCREEN_WIDTH {
                let screen_x = i32::try_from(x).unwrap_or(0) - 128;
                let screen_y = i32::try_from(y).unwrap_or(0) - 112;
                let mut source_x = origin_x + ((a * screen_x + b * screen_y) >> 8);
                let mut source_y = origin_y + ((c * screen_x + d * screen_y) >> 8);
                if self.m7sel & 0x01 != 0 {
                    source_x = -source_x;
                }
                if self.m7sel & 0x02 != 0 {
                    source_y = -source_y;
                }
                if !(0..1024).contains(&source_x) || !(0..1024).contains(&source_y) {
                    continue;
                }
                let tile_x = usize::try_from(source_x).unwrap_or(0) / 8;
                let tile_y = usize::try_from(source_y).unwrap_or(0) / 8;
                let map_index = (tile_y * 128 + tile_x) & 0x7fff;
                let tile = usize::from((self.vram[map_index] & 0x00ff) as u8);
                let tile_data = (0x4000 + tile * 64) & 0x7fff;
                let pixel_index = tile_data
                    + (usize::try_from(source_y).unwrap_or(0) & 7) * 8
                    + (usize::try_from(source_x).unwrap_or(0) & 7);
                let color = (self.vram[pixel_index & 0x7fff] & 0x00ff) as u8;
                if color != 0 && (self.tmw & 0x01 == 0 || !self.window_hides(0, x)) {
                    let color = if self.main_color_window_applies(x) {
                        0
                    } else {
                        self.cgram[usize::from(color)]
                    };
                    let color = self.apply_color_math_at(color, 0x01, x);
                    pixels[y * SCREEN_WIDTH + x] = self.color_to_rgba(color);
                }
            }
        }
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

    fn write_mode7_register(&mut self, address: u16, value: u8) {
        let index = match address {
            0x211a => return self.m7sel = value,
            0x211b => 0,
            0x211c => 1,
            0x211d => 2,
            0x211e => 3,
            0x211f => 4,
            0x2120 => 5,
            _ => return,
        };
        if let Some(low) = self.mode7_latches[index].take() {
            let combined = i16::from_le_bytes([low, value]);
            match index {
                0 => self.m7a = combined,
                1 => self.m7b = combined,
                2 => self.m7c = combined,
                3 => self.m7d = combined,
                4 => self.m7x = combined,
                _ => self.m7y = combined,
            }
        } else {
            self.mode7_latches[index] = Some(value);
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
        assert_eq!(ppu.bg_pixel_for_mode(0, 0, 0, false, 0), (1, false));

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];
        ppu.render_frame(&mut frame);

        // BG1 has the highest normal priority and its palette starts at 0.
        assert_eq!(frame[0], ppu.color_to_rgba(0x001f));
        assert_ne!(frame[8], frame[0]);
    }

    #[test]
    fn oam_decodes_extended_position_size_and_attributes() {
        let mut ppu = Ppu::default();
        ppu.oam[0] = 0x05;
        ppu.oam[1] = 0x17;
        ppu.oam[2] = 0x23;
        ppu.oam[3] = 0x71; // palette 0, priority 3, name select, h-flip
        ppu.oam[512] = 0x03; // X high bit and large-size bit for sprite 0.
        ppu.obsel = 0xa0; // 64x64 large size pair.

        let sprite = ppu.decode_sprite(0);
        assert_eq!(sprite.x, -251);
        assert_eq!(sprite.y, 0x17);
        assert_eq!(sprite.tile, 0x23);
        assert_eq!(sprite.palette, 0);
        assert_eq!(sprite.priority, 3);
        assert!(sprite.name_select);
        assert!(sprite.h_flip);
        assert!(!sprite.v_flip);
        assert!(sprite.size_large);
        assert_eq!(ppu.obj_dimensions(true), (64, 64));
    }

    #[test]
    fn mode_zero_renders_four_bpp_obj_and_h_flip() {
        let mut ppu = Ppu::default();
        ppu.inidisp = 0;
        ppu.tm = 0x10; // OBJ only.
        ppu.cgram[128 + 1] = 0x001f;
        ppu.cgram[128 + 2] = 0x03e0;

        // An 8x8 OBJ tile with color 1 at the left edge and color 2 at the
        // right edge, using the two low bitplanes in the first row.
        ppu.vram[0] = 0x0080;
        ppu.vram[1] = 0x0001;
        ppu.oam[0..4].copy_from_slice(&[10, 20, 0, 0]);
        ppu.oam[512] = 0;

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];
        ppu.render_frame(&mut frame);
        assert_eq!(frame[20 * SCREEN_WIDTH + 10], ppu.color_to_rgba(0x001f));
        assert_eq!(frame[20 * SCREEN_WIDTH + 17], ppu.color_to_rgba(0x03e0));

        ppu.oam[3] = 0x40; // horizontal flip
        frame.fill(0);
        ppu.render_frame(&mut frame);
        assert_eq!(frame[20 * SCREEN_WIDTH + 10], ppu.color_to_rgba(0x03e0));
        assert_eq!(frame[20 * SCREEN_WIDTH + 17], ppu.color_to_rgba(0x001f));
    }

    #[test]
    fn mode_zero_obj_priority_sits_below_high_background() {
        let mut ppu = Ppu::default();
        ppu.inidisp = 0;
        ppu.tm = 0x11; // BG1 and OBJ.
        ppu.bg1sc = 0x04; // Keep the BG map away from the OBJ tile at VRAM 0.
        ppu.cgram[1] = 0x7c00;
        ppu.cgram[128 + 1] = 0x001f;

        // BG1 high-priority color 1 at the origin.
        ppu.vram[16] = 0x0080;
        ppu.vram[0x400] = 1 | (1 << 13);
        // OBJ color 1 at the same screen pixel, with the lowest OBJ priority
        // so that the high BG1 plane remains in front of it.
        ppu.vram[0] = 0x0080;
        ppu.vram[1] = 0;
        ppu.vram[8] = 0;
        ppu.vram[9] = 0;
        ppu.oam[0..4].copy_from_slice(&[0, 0, 0, 0x00]);
        ppu.oam[512] = 0;

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];
        ppu.render_frame(&mut frame);
        assert_eq!(frame[0], ppu.color_to_rgba(0x7c00));
    }

    #[test]
    fn mode7_renders_an_affine_tilemap_sample() {
        let mut ppu = Ppu::default();
        ppu.inidisp = 0;
        ppu.bgmode = 7;
        ppu.tm = 1;
        ppu.m7a = 0x0100;
        ppu.m7d = 0x0100;
        ppu.m7x = 128;
        ppu.m7y = 112;
        ppu.cgram[1] = 0x001f;
        for map in 0..0x4000 {
            ppu.vram[map] = 1;
        }
        for pixel in 0..64 {
            ppu.vram[0x4000 + 64 + pixel] = 1;
        }

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];
        ppu.render_frame(&mut frame);

        assert_eq!(frame[0], ppu.color_to_rgba(0x001f));
        assert_eq!(frame[112 * SCREEN_WIDTH + 128], ppu.color_to_rgba(0x001f));
    }

    #[test]
    fn mode_one_renders_bg1_four_bpp_pixels() {
        let mut ppu = Ppu::default();
        ppu.inidisp = 0;
        ppu.bgmode = 1;
        ppu.tm = 0x01;
        ppu.cgram[13] = 0x001f;

        // Map entry 1 selects a 4bpp tile.  All four bitplanes are set for
        // the first two pixels, producing color 13 (1 + 4 + 8).
        ppu.vram[0] = 1;
        ppu.vram[16] = 0x00c0;
        ppu.vram[24] = 0x00c0;
        ppu.vram[25] = 0x00c0;

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];
        ppu.render_frame(&mut frame);

        assert_eq!(frame[0], ppu.color_to_rgba(0x001f));
        assert_eq!(frame[1], ppu.color_to_rgba(0x001f));
        assert_eq!(frame[8], ppu.color_to_rgba(0));
    }

    #[test]
    fn mode_one_renders_bg2_four_bpp_and_bg3_two_bpp_palette_indices() {
        let mut ppu = Ppu::default();
        ppu.inidisp = 0;
        ppu.bgmode = 1;
        ppu.bg2sc = 0x04;
        ppu.bg12nba = 0x10;
        ppu.bg34nba = 0x01;
        ppu.cgram[17] = 0x03e0;
        ppu.vram[0x0400] = 1 | (1 << 10);
        ppu.vram[0x1010] = 0x0080;
        ppu.tm = 0x02;

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];
        ppu.render_frame(&mut frame);
        assert_eq!(frame[0], ppu.color_to_rgba(0x03e0));

        ppu.bg3sc = 0x08;
        ppu.bg34nba = 0x01;
        ppu.cgram[5] = 0x7c00;
        ppu.vram[0x0800] = 1 | (1 << 10);
        ppu.vram[0x1010] = 0x0080;
        ppu.tm = 0x04;
        frame.fill(0);
        ppu.render_frame(&mut frame);
        assert_eq!(frame[0], ppu.color_to_rgba(0x7c00));
    }

    #[test]
    fn mode_one_bg3_priority_bit_moves_high_bg3_above_the_other_backgrounds() {
        let mut ppu = Ppu::default();
        ppu.inidisp = 0;
        ppu.bgmode = 1;
        ppu.tm = 0x05;
        ppu.bg3sc = 0x04;
        ppu.bg34nba = 0x01;
        ppu.cgram[1] = 0x001f;
        ppu.cgram[5] = 0x03e0;

        ppu.vram[0] = 1;
        ppu.vram[16] = 0x0080;
        ppu.vram[0x0400] = 1 | (1 << 10) | (1 << 13);
        ppu.vram[0x1010] = 0x0080;

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];
        ppu.render_frame(&mut frame);
        assert_eq!(frame[0], ppu.color_to_rgba(0x001f));

        ppu.bgmode |= 0x08;
        frame.fill(0);
        ppu.render_frame(&mut frame);
        assert_eq!(frame[0], ppu.color_to_rgba(0x03e0));
    }

    #[test]
    fn mode_two_uses_four_bpp_palettes_and_mode_two_priority() {
        let mut ppu = Ppu::default();
        ppu.inidisp = 0;
        ppu.bgmode = 2;
        ppu.bg1sc = 0x04;
        ppu.bg2sc = 0x08;
        ppu.bg12nba = 0x10; // BG1 at 0, BG2 at 0x1000.
        ppu.cgram[21] = 0x03e0; // BG2 palette 1, color 5.
        ppu.cgram[1] = 0x001f; // BG1 palette 0, color 1.

        ppu.vram[0x0400] = 1 | (1 << 13);
        ppu.vram[0x0800] = 1 | (1 << 10) | (1 << 13);
        ppu.vram[0x0010] = 0x0080; // BG1 color 1.
        ppu.vram[0x1010] = 0x0080; // BG2 color 1.
        ppu.vram[0x1018] = 0x0080; // BG2 color 4, making color 5.
        assert_eq!(ppu.bg_pixel_for_mode(1, 0, 0, true, 2), (21, true));

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];
        ppu.tm = 0x02;
        ppu.render_frame(&mut frame);
        assert_eq!(frame[0], ppu.color_to_rgba(0x03e0));

        // BG1 high priority is in front of BG2 high priority in Mode 2.
        ppu.tm = 0x03;
        ppu.render_frame(&mut frame);
        assert_eq!(frame[0], ppu.color_to_rgba(0x001f));
    }

    #[test]
    fn mode_two_applies_horizontal_and_vertical_offsets_per_column() {
        let mut ppu = Ppu::default();
        ppu.inidisp = 0;
        ppu.bgmode = 2;
        ppu.tm = 0x01;
        ppu.bg1sc = 0x04;
        ppu.bg12nba = 0x01; // BG1 tile data at 0x1000.
        ppu.cgram[1] = 0x001f;
        ppu.cgram[2] = 0x03e0;

        // The sampled row of tile 1 is color 1 and the sampled row of tile 2
        // is color 2.
        ppu.vram[0x1010] = 0x00ff;
        ppu.vram[0x1021] = 0x00ff;
        ppu.vram[0x0400] = 1;
        ppu.vram[0x0401] = 1;
        ppu.vram[0x0402] = 2;
        ppu.vram[0x0421] = 2;

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];

        // BG3 entry 0 applies to the second visible column.  The first
        // visible column must remain under normal BG1 scrolling.
        ppu.vram[0] = 0x2000 | 8; // Enable BG1 horizontal offset, HOFS = 8.
        ppu.render_frame(&mut frame);
        assert_eq!(frame[0], ppu.color_to_rgba(0x001f));
        assert_eq!(frame[8], ppu.color_to_rgba(0x03e0));

        // Replace the horizontal override with a vertical one.  At x=8 the
        // second column now samples BG1 tilemap row 1 instead.
        ppu.vram[0] = 0;
        ppu.vram[0x20] = 0x2000 | 8;
        ppu.render_frame(&mut frame);
        assert_eq!(frame[0], ppu.color_to_rgba(0x001f));
        assert_eq!(frame[8], ppu.color_to_rgba(0x03e0));
    }

    #[test]
    fn subscreen_color_math_uses_ts_and_respects_subscreen_windows() {
        let mut ppu = Ppu::default();
        ppu.inidisp = 0;
        ppu.bgmode = 1;
        ppu.tm = 0x01;
        ppu.ts = 0x02;
        ppu.bg2sc = 0x04;
        ppu.bg12nba = 0x10;
        ppu.cgram[1] = 0x0010;
        ppu.cgram[17] = 0x000f;
        ppu.vram[0] = 1;
        ppu.vram[16] = 0x00c0;
        ppu.vram[0x0400] = 1 | (1 << 10);
        ppu.vram[0x1010] = 0x00c0;
        ppu.cgwsel = 0x02; // Use the subscreen instead of fixed color.
        ppu.cgadsub = 0x01; // Add BG1 and enable its color math.
        ppu.cgram[0] = 0x001f;
        ppu.fixed_color = 0x0003;

        // Hide BG2 from the subscreen at x=0 only.
        ppu.tsw = 0x02;
        ppu.w12sel = 0x10;
        ppu.wh[0] = 0;
        ppu.wh[1] = 0;

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];
        ppu.render_frame(&mut frame);
        assert_eq!(frame[0], ppu.color_to_rgba(0x0013));
        assert_eq!(frame[1], ppu.color_to_rgba(0x001f));
    }

    #[test]
    fn subscreen_color_math_supports_subtraction_and_half_result() {
        let mut ppu = Ppu::default();
        ppu.inidisp = 0;
        ppu.bgmode = 1;
        ppu.tm = 0x01;
        ppu.ts = 0x02;
        ppu.bg2sc = 0x04;
        ppu.bg12nba = 0x10;
        ppu.cgram[1] = 0x001f;
        ppu.cgram[17] = 0x000f;
        ppu.vram[0] = 1;
        ppu.vram[16] = 0x0080;
        ppu.vram[0x0400] = 1 | (1 << 10);
        ppu.vram[0x1010] = 0x0080;
        ppu.cgwsel = 0x02;
        ppu.cgadsub = 0xc1; // Subtract BG2 from BG1, then halve.

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];
        ppu.render_frame(&mut frame);
        assert_eq!(frame[0], ppu.color_to_rgba(0x0008));
    }

    #[test]
    fn fixed_color_math_adds_selected_channels() {
        let mut ppu = Ppu::default();
        ppu.cgadsub = 0x20;
        ppu.fixed_color = 0x0010;
        assert_eq!(ppu.apply_color_math_at(0x0010, 0x20, 0), 0x001f);

        ppu.cgadsub = 0xa0;
        assert_eq!(ppu.apply_color_math_at(0x0010, 0x20, 0), 0);
    }

    fn opaque_bg1_row(color: u16) -> Ppu {
        let mut ppu = Ppu::default();
        ppu.inidisp = 0;
        ppu.tm = 0x01;
        ppu.cgram[1] = color;
        ppu.vram[0] = 1;
        ppu.vram[16] = 0x00ff;
        ppu
    }

    #[test]
    fn window_mask_hides_bg_pixels_and_combines_two_windows() {
        let mut ppu = opaque_bg1_row(0x001f);
        ppu.write_register(0x2123, 0x05); // BG1: enable windows 1 and 2.
        ppu.write_register(0x2126, 1);
        ppu.write_register(0x2127, 2);
        ppu.write_register(0x2128, 5);
        ppu.write_register(0x2129, 6);
        ppu.write_register(0x212e, 0x01); // Apply BG1's window to the main screen.
        assert_eq!(ppu.read_register(0x2123), 0x05);
        assert_eq!(ppu.read_register(0x2128), 5);

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];
        ppu.render_frame(&mut frame);

        let red = ppu.color_to_rgba(0x001f);
        let black = ppu.color_to_rgba(0);
        for x in 0..8 {
            let expected = if (1..=2).contains(&x) || (5..=6).contains(&x) {
                black
            } else {
                red
            };
            assert_eq!(frame[x], expected, "unexpected BG1 window result at x={x}");
        }

        // AND with an inverted second window leaves only the first window's
        // pixels hidden in this non-overlapping setup.
        ppu.wbglog = 0x01;
        ppu.w12sel = 0x0d; // Window 2 enabled and inverted.
        frame.fill(0);
        ppu.render_frame(&mut frame);
        for x in 0..8 {
            let expected = if (1..=2).contains(&x) { black } else { red };
            assert_eq!(
                frame[x], expected,
                "unexpected AND/inverted result at x={x}"
            );
        }
    }

    #[test]
    fn color_window_can_reject_color_math_inside_its_region() {
        let mut ppu = opaque_bg1_row(0x0010);
        ppu.fixed_color = 0x000f;
        ppu.cgadsub = 0x01; // Add fixed color to BG1.
        ppu.cgwsel = 0x20; // Make the color window's inside region transparent to math.
        ppu.wobjsel = 0x10; // Enable color window 1.
        ppu.wh = [2, 3, 0, 0];

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];
        ppu.render_frame(&mut frame);

        let blended = ppu.color_to_rgba(0x001f);
        let original = ppu.color_to_rgba(0x0010);
        for x in 0..8 {
            let expected = if (2..=3).contains(&x) {
                original
            } else {
                blended
            };
            assert_eq!(
                frame[x], expected,
                "unexpected color-window result at x={x}"
            );
        }
    }

    #[test]
    fn main_color_window_blacks_the_selected_region() {
        let mut ppu = opaque_bg1_row(0x001f);
        ppu.cgwsel = 0x40; // Black the outside of the color window.
        ppu.wobjsel = 0x10;
        ppu.wh = [2, 3, 0, 0];

        let mut frame = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT];
        ppu.render_frame(&mut frame);

        let red = ppu.color_to_rgba(0x001f);
        let black = ppu.color_to_rgba(0);
        for x in 0..8 {
            let expected = if (2..=3).contains(&x) { red } else { black };
            assert_eq!(
                frame[x], expected,
                "unexpected main color-window result at x={x}"
            );
        }
    }
}
