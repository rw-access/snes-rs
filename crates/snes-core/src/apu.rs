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
}

impl Default for Apu {
    fn default() -> Self {
        Self {
            ram: [0; 65_536],
            ports: [0; 4],
            control: 0,
            dsp_address: 0,
            dsp: [0; 128],
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
}
