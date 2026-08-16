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
        // The four CPU-visible ports are also the SPC700's F4-F7 I/O
        // registers.  Mirroring them at the execution boundary keeps the
        // small core useful for boot-RAM and communication tests without
        // pretending that the DSP is ordinary RAM.
        self.ram[0x00f4..0x00f8].copy_from_slice(&self.ports);
        let cycles = self.spc700.step(&mut self.ram);
        self.ports.copy_from_slice(&self.ram[0x00f4..0x00f8]);
        cycles
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
            0x08 => {
                self.a = self.a | self.fetch(ram);
                self.set_zero_negative(self.a);
                2
            }
            0x28 => {
                self.a &= self.fetch(ram);
                self.set_zero_negative(self.a);
                2
            }
            0x48 => {
                self.a ^= self.fetch(ram);
                self.set_zero_negative(self.a);
                2
            }
            0x68 => {
                let value = self.fetch(ram);
                self.compare(self.a, value);
                2
            }
            0x88 => {
                let value = self.fetch(ram);
                self.adc(value);
                2
            }
            0xa8 => {
                let value = self.fetch(ram);
                self.sbc(value);
                2
            }
            0x04 => {
                self.a |= self.read_direct(ram);
                self.set_zero_negative(self.a);
                3
            }
            0x24 => {
                self.a &= self.read_direct(ram);
                self.set_zero_negative(self.a);
                3
            }
            0x44 => {
                self.a ^= self.read_direct(ram);
                self.set_zero_negative(self.a);
                3
            }
            0x64 => {
                let value = self.read_direct(ram);
                self.compare(self.a, value);
                3
            }
            0x84 => {
                let value = self.read_direct(ram);
                self.adc(value);
                3
            }
            0xa4 => {
                let value = self.read_direct(ram);
                self.sbc(value);
                3
            }
            0x14 => {
                self.a |= self.read_direct_indexed(ram, self.x);
                self.set_zero_negative(self.a);
                4
            }
            0x34 => {
                self.a &= self.read_direct_indexed(ram, self.x);
                self.set_zero_negative(self.a);
                4
            }
            0x54 => {
                self.a ^= self.read_direct_indexed(ram, self.x);
                self.set_zero_negative(self.a);
                4
            }
            0x74 => {
                let value = self.read_direct_indexed(ram, self.x);
                self.compare(self.a, value);
                4
            }
            0x94 => {
                let value = self.read_direct_indexed(ram, self.x);
                self.adc(value);
                4
            }
            0xb4 => {
                let value = self.read_direct_indexed(ram, self.x);
                self.sbc(value);
                4
            }
            0x06 => {
                self.a |= ram[usize::from(self.direct_address(self.x))];
                self.set_zero_negative(self.a);
                3
            }
            0x26 => {
                self.a &= ram[usize::from(self.direct_address(self.x))];
                self.set_zero_negative(self.a);
                3
            }
            0x46 => {
                self.a ^= ram[usize::from(self.direct_address(self.x))];
                self.set_zero_negative(self.a);
                3
            }
            0x66 => {
                self.compare(self.a, ram[usize::from(self.direct_address(self.x))]);
                3
            }
            0x86 => {
                self.adc(ram[usize::from(self.direct_address(self.x))]);
                3
            }
            0xa6 => {
                self.sbc(ram[usize::from(self.direct_address(self.x))]);
                3
            }
            0x20 => {
                self.psw &= !0x20;
                2
            }
            0x40 => {
                self.psw |= 0x20;
                2
            }
            0x60 => {
                self.psw &= !0x01;
                2
            }
            0x80 => {
                self.psw |= 0x01;
                2
            }
            0xa0 => {
                self.psw |= 0x04;
                2
            }
            0xc0 => {
                self.psw &= !0x04;
                2
            }
            0xe0 => {
                self.psw &= !0x40;
                2
            }
            0xed => {
                self.psw ^= 0x01;
                3
            }
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
                let address = self.fetch_direct_address(ram);
                ram[usize::from(address)] = self.a;
                3
            }
            0xe4 => {
                let address = self.fetch_direct_address(ram);
                self.a = ram[usize::from(address)];
                self.set_zero_negative(self.a);
                3
            }
            0x8f => {
                let value = self.fetch(ram);
                let address = self.fetch_direct_address(ram);
                ram[usize::from(address)] = value;
                5
            }
            0xc6 => {
                let address = self.direct_address(self.x);
                ram[usize::from(address)] = self.a;
                3
            }
            0xe6 => {
                let address = self.direct_address(self.x);
                self.a = ram[usize::from(address)];
                self.set_zero_negative(self.a);
                3
            }
            0xd4 => {
                let address = self.direct_indexed_address(ram, self.x);
                ram[usize::from(address)] = self.a;
                4
            }
            0xf4 => {
                let address = self.direct_indexed_address(ram, self.x);
                self.a = ram[usize::from(address)];
                self.set_zero_negative(self.a);
                4
            }
            0x7d => {
                self.x = self.a;
                self.set_zero_negative(self.x);
                2
            }
            0x5d => {
                self.a = self.x;
                self.set_zero_negative(self.a);
                2
            }
            0xdd => {
                self.y = self.a;
                self.set_zero_negative(self.y);
                2
            }
            0xfd => {
                self.a = self.y;
                self.set_zero_negative(self.a);
                2
            }
            0x9d => {
                self.x = self.y;
                self.set_zero_negative(self.x);
                2
            }
            0xbd => {
                self.y = self.x;
                self.set_zero_negative(self.y);
                2
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
                self.pc = self
                    .pc
                    .wrapping_add_signed(i16::from(self.fetch(ram) as i8));
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
            0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xb0 => {
                let offset = self.fetch(ram) as i8;
                let take = match opcode {
                    0x10 => self.psw & 0x02 == 0,
                    0x30 => self.psw & 0x80 != 0,
                    0x50 => self.psw & 0x40 == 0,
                    0x70 => self.psw & 0x40 != 0,
                    0x90 => self.psw & 0x01 == 0,
                    0xb0 => self.psw & 0x01 != 0,
                    _ => false,
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
            0x4d => {
                self.push(ram, self.x);
                4
            }
            0x6d => {
                self.push(ram, self.y);
                4
            }
            0x0d => {
                self.push(ram, self.psw);
                4
            }
            0xae => {
                self.a = self.pop(ram);
                self.set_zero_negative(self.a);
                4
            }
            0xce => {
                self.x = self.pop(ram);
                self.set_zero_negative(self.x);
                4
            }
            0xee => {
                self.y = self.pop(ram);
                self.set_zero_negative(self.y);
                4
            }
            0x8e => {
                self.psw = self.pop(ram);
                4
            }
            0xef => {
                self.stopped = true;
                3
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

    fn direct_address(&self, operand: u8) -> u16 {
        u16::from(operand) | if self.psw & 0x20 != 0 { 0x0100 } else { 0 }
    }

    fn fetch_direct_address(&mut self, ram: &[u8; 65_536]) -> u16 {
        let operand = self.fetch(ram);
        self.direct_address(operand)
    }

    fn direct_indexed_address(&mut self, ram: &[u8; 65_536], index: u8) -> u16 {
        let operand = self.fetch(ram);
        self.direct_address(operand.wrapping_add(index))
    }

    fn read_direct(&mut self, ram: &[u8; 65_536]) -> u8 {
        let address = self.fetch_direct_address(ram);
        ram[usize::from(address)]
    }

    fn read_direct_indexed(&mut self, ram: &[u8; 65_536], index: u8) -> u8 {
        let address = self.direct_indexed_address(ram, index);
        ram[usize::from(address)]
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

    fn compare(&mut self, left: u8, right: u8) {
        let result = left.wrapping_sub(right);
        self.psw = (self.psw & !0x83)
            | if result == 0 { 0x02 } else { 0 }
            | if result & 0x80 != 0 { 0x80 } else { 0 }
            | if left >= right { 0x01 } else { 0 };
    }

    fn adc(&mut self, value: u8) {
        let carry = u16::from(self.psw & 0x01);
        let left = u16::from(self.a);
        let right = u16::from(value);
        let sum = left + right + carry;
        let result = sum as u8;
        let overflow = (!(self.a ^ value) & (self.a ^ result) & 0x80) != 0;
        let half_carry = (self.a & 0x0f) as u16 + (value & 0x0f) as u16 + carry > 0x0f;
        self.a = result;
        self.set_zero_negative(result);
        self.psw = (self.psw & !0x49)
            | if sum > 0xff { 0x01 } else { 0 }
            | if overflow { 0x40 } else { 0 }
            | if half_carry { 0x08 } else { 0 };
    }

    fn sbc(&mut self, value: u8) {
        let borrow = i16::from(1 - (self.psw & 0x01));
        let difference = i16::from(self.a) - i16::from(value) - borrow;
        let result = difference as u8;
        let overflow = ((self.a ^ value) & (self.a ^ result) & 0x80) != 0;
        let half_carry = i16::from(self.a & 0x0f) - i16::from(value & 0x0f) - borrow >= 0;
        self.a = result;
        self.set_zero_negative(result);
        self.psw = (self.psw & !0x49)
            | if difference >= 0 { 0x01 } else { 0 }
            | if overflow { 0x40 } else { 0 }
            | if half_carry { 0x08 } else { 0 };
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

    #[test]
    fn spc700_alu_updates_condition_codes_for_immediate_forms() {
        let mut apu = Apu::default();
        apu.ram[..13].copy_from_slice(&[
            0xe8, 0x7f, // MOV A,#$7f
            0x80, // SETC
            0x88, 0x00, // ADC A,#$00 -> $80, with V and H set
            0x60, // CLRC
            0xa8, 0x80, // SBC A,#$80 -> $ff, with borrow
            0x80, // SETC
            0xa8, 0xff, // SBC A,#$ff -> $00, no borrow
            0x68, 0x00, // CMP A,#$00
        ]);

        assert_eq!(apu.step_spc700(), 2);
        assert_eq!(apu.step_spc700(), 2);
        assert_eq!(apu.step_spc700(), 2);
        assert_eq!(apu.spc700.a, 0x80);
        assert_eq!(apu.spc700.psw & 0xc8, 0xc8);
        assert_eq!(apu.spc700.psw & 0x01, 0);

        apu.step_spc700();
        apu.step_spc700();
        assert_eq!(apu.spc700.a, 0xff);
        assert_eq!(apu.spc700.psw & 0xc3, 0x80);

        apu.step_spc700();
        apu.step_spc700();
        apu.step_spc700();
        assert_eq!(apu.spc700.a, 0x00);
        assert_eq!(apu.spc700.psw & 0x43, 0x03);
    }

    #[test]
    fn spc700_condition_branches_use_their_psw_flags() {
        let cases = [
            (0x10, 0x00), // BPL
            (0x30, 0x80), // BMI
            (0x50, 0x00), // BVC
            (0x70, 0x40), // BVS
            (0x90, 0x00), // BCC
            (0xb0, 0x01), // BCS
        ];

        for (opcode, psw) in cases {
            let mut apu = Apu::default();
            apu.spc700.pc = 0x0100;
            apu.spc700.psw = psw;
            apu.ram[0x0100..0x0102].copy_from_slice(&[opcode, 0x02]);
            assert_eq!(apu.step_spc700(), 2);
            assert_eq!(apu.spc700.pc, 0x0104, "opcode {opcode:02x}");
        }

        let mut apu = Apu::default();
        apu.ram[..2].copy_from_slice(&[0xb0, 0x02]);
        assert_eq!(apu.step_spc700(), 2);
        assert_eq!(apu.spc700.pc, 0x0002);
    }

    #[test]
    fn spc700_direct_page_indexed_and_x_indirect_forms_honor_p_flag() {
        let mut apu = Apu::default();
        apu.ram[0x0110] = 0x0f;
        apu.ram[..20].copy_from_slice(&[
            0x40, // SETP: direct page is $0100
            0xcd, 0x20, // MOV X,#$20
            0xe8, 0x33, // MOV A,#$33
            0xd4, 0x10, // MOV $10+X,A -> $0130
            0xe8, 0x0f, // MOV A,#$0f
            0x14, 0x10, // OR A,$10+X
            0xc6, // MOV (X),A -> $0120
            0xe8, 0x00, // MOV A,#$00
            0xf4, 0x10, // MOV A,$10+X
            0x24, 0x10, // AND A,$10 -> $0110
            0x20, // CLRP
            0xff, // STOP
        ]);

        for _ in 0..12 {
            apu.step_spc700();
        }

        assert_eq!(apu.ram[0x0130], 0x33);
        assert_eq!(apu.ram[0x0120], 0x3f);
        assert_eq!(apu.spc700.a, 0x03);
        assert!(apu.spc700.stopped);
    }

    #[test]
    fn spc700_register_transfers_and_stack_forms_round_trip() {
        let mut apu = Apu::default();
        apu.ram[..16].copy_from_slice(&[
            0xe8, 0xa5, // MOV A,#$a5
            0x7d, // MOV X,A
            0xdd, // MOV Y,A
            0x4d, // PUSH X
            0x6d, // PUSH Y
            0xe8, 0x00, // MOV A,#$00
            0xee, // POP Y
            0xce, // POP X
            0xfd, // MOV A,Y
            0x0d, // PUSH PSW
            0xe8, 0x00, // MOV A,#$00
            0x8e, // POP PSW
            0xfd, // MOV A,Y
        ]);

        for _ in 0..13 {
            apu.step_spc700();
        }

        assert_eq!(apu.spc700.a, 0xa5);
        assert_eq!(apu.spc700.x, 0xa5);
        assert_eq!(apu.spc700.y, 0xa5);
        assert_eq!(apu.spc700.sp, 0xef);
        assert_eq!(apu.spc700.psw & 0x82, 0x80);
    }
}
