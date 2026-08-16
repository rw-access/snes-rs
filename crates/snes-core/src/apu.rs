//! S-CPU/S-SMP communication state and APU memory boundary.

/// Minimal APU state shared through the four CPU-visible ports. The SPC700
/// execution and DSP mixer consume this state in later audio increments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Apu {
    pub ram: [u8; 65_536],
    pub ports: [u8; 4],
    pub control: u8,
    pub dsp_address: u8,
    pub dsp: [u8; 128],
    pub spc700: Spc700,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Spc700 {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub psw: u8,
    pub stopped: bool,
}

impl Default for Apu {
    fn default() -> Self {
        Self {
            ram: [0; 65_536],
            ports: [0; 4],
            control: 0,
            dsp_address: 0,
            dsp: [0; 128],
            spc700: Spc700 {
                sp: 0xef,
                ..Spc700::default()
            },
        }
    }
}

impl Apu {
    pub fn read_cpu_port(&self, port: usize) -> u8 {
        self.ports[port & 3]
    }

    pub fn write_cpu_port(&mut self, port: usize, value: u8) {
        self.ports[port & 3] = value;
    }

    pub fn read_ram(&self, address: u16) -> u8 {
        self.ram[address as usize]
    }

    pub fn write_ram(&mut self, address: u16, value: u8) {
        self.ram[address as usize] = value;
    }

    pub fn write_control(&mut self, value: u8) {
        self.control = value;
        if value & 0x10 != 0 {
            self.ports[0] = 0;
            self.ports[1] = 0;
        }
        if value & 0x20 != 0 {
            self.ports[2] = 0;
            self.ports[3] = 0;
        }
    }

    pub fn write_dsp_address(&mut self, value: u8) {
        self.dsp_address = value & 0x7f;
    }

    pub fn read_dsp_data(&self) -> u8 {
        self.dsp[self.dsp_address as usize]
    }

    pub fn write_dsp_data(&mut self, value: u8) {
        self.dsp[self.dsp_address as usize] = value;
    }

    pub fn step_spc700(&mut self) -> u32 {
        self.spc700.step(&mut self.ram)
    }
}

impl Spc700 {
    pub fn step(&mut self, ram: &mut [u8; 65_536]) -> u32 {
        if self.stopped {
            return 0;
        }
        let opcode = ram[self.pc as usize];
        self.pc = self.pc.wrapping_add(1);
        match opcode {
            0x00 => 2,
            0xe8 => {
                self.a = self.fetch(ram);
                self.set_zero_negative(self.a);
                2
            }
            0xcd => {
                self.x = self.fetch(ram);
                self.set_zero_negative(self.x);
                2
            }
            0x8d => {
                self.y = self.fetch(ram);
                self.set_zero_negative(self.y);
                2
            }
            0xc4 => {
                let address = u16::from(self.fetch(ram));
                ram[address as usize] = self.a;
                3
            }
            0xe4 => {
                let address = u16::from(self.fetch(ram));
                self.a = ram[address as usize];
                self.set_zero_negative(self.a);
                3
            }
            0x8f => {
                let value = self.fetch(ram);
                let address = u16::from(self.fetch(ram));
                ram[address as usize] = value;
                5
            }
            0xbc => {
                self.a = self.a.wrapping_add(1);
                self.set_zero_negative(self.a);
                2
            }
            0x9c => {
                self.a = self.a.wrapping_sub(1);
                self.set_zero_negative(self.a);
                2
            }
            0x3d => {
                self.x = self.x.wrapping_add(1);
                self.set_zero_negative(self.x);
                2
            }
            0x1d => {
                self.x = self.x.wrapping_sub(1);
                self.set_zero_negative(self.x);
                2
            }
            0xfc => {
                self.y = self.y.wrapping_add(1);
                self.set_zero_negative(self.y);
                2
            }
            0xdc => {
                self.y = self.y.wrapping_sub(1);
                self.set_zero_negative(self.y);
                2
            }
            0x2f => {
                self.pc = self.pc.wrapping_add_signed(i16::from(self.fetch(ram) as i8));
                4
            }
            0xf0 | 0xd0 => {
                let offset = self.fetch(ram) as i8;
                let take = if opcode == 0xf0 {
                    self.psw & 0x02 != 0
                } else {
                    self.psw & 0x02 == 0
                };
                if take {
                    self.pc = self.pc.wrapping_add_signed(i16::from(offset));
                }
                2
            }
            0x5f => {
                let lo = self.fetch(ram);
                let hi = self.fetch(ram);
                self.pc = u16::from_le_bytes([lo, hi]);
                3
            }
            0x3f => {
                let lo = self.fetch(ram);
                let hi = self.fetch(ram);
                let target = u16::from_le_bytes([lo, hi]);
                self.push(ram, (self.pc >> 8) as u8);
                self.push(ram, self.pc as u8);
                self.pc = target;
                8
            }
            0x6f => {
                let lo = self.pop(ram);
                let hi = self.pop(ram);
                self.pc = u16::from_le_bytes([lo, hi]);
                5
            }
            0x2d => {
                self.push(ram, self.a);
                4
            }
            0xae => {
                self.a = self.pop(ram);
                self.set_zero_negative(self.a);
                4
            }
            0xff => {
                self.stopped = true;
                2
            }
            _ => 2,
        }
    }

    fn fetch(&mut self, ram: &[u8; 65_536]) -> u8 {
        let value = ram[self.pc as usize];
        self.pc = self.pc.wrapping_add(1);
        value
    }

    fn push(&mut self, ram: &mut [u8; 65_536], value: u8) {
        ram[0x0100 | usize::from(self.sp)] = value;
        self.sp = self.sp.wrapping_sub(1);
    }

    fn pop(&mut self, ram: &[u8; 65_536]) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        ram[0x0100 | usize::from(self.sp)]
    }

    fn set_zero_negative(&mut self, value: u8) {
        self.psw = (self.psw & !0x82)
            | if value == 0 { 0x02 } else { 0 }
            | if value & 0x80 != 0 { 0x80 } else { 0 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_ports_and_reset_control_are_snapshot_ready() {
        let mut apu = Apu::default();
        apu.write_cpu_port(0, 0x5a);
        assert_eq!(apu.read_cpu_port(0), 0x5a);
        apu.write_control(0x10);
        assert_eq!(apu.read_cpu_port(0), 0);
    }

    #[test]
    fn spc700_executes_basic_register_and_memory_operations() {
        let mut apu = Apu::default();
        apu.ram[..6].copy_from_slice(&[0xe8, 0x5a, 0xc4, 0x10, 0xff, 0x00]);
        assert_eq!(apu.step_spc700(), 2);
        assert_eq!(apu.step_spc700(), 3);
        assert_eq!(apu.ram[0x10], 0x5a);
        apu.step_spc700();
        assert!(apu.spc700.stopped);
    }
}
