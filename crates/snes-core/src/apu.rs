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
    timer_targets: [u8; 3],
    timer_stage2: [u8; 3],
    timer_outputs: [u8; 3],
    timer_stage1_cycles: [u16; 3],
    dsp_voices: [DspVoice; 8],
    dsp_kon_seen: u8,
    dsp_kof_seen: u8,
    dsp_kon_pending: u8,
    dsp_kof_pending: u8,
}

/// One stereo PCM sample produced by the bounded S-DSP mixer.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AudioSample {
    pub left: i16,
    pub right: i16,
}

/// Runtime state for one S-DSP voice.
///
/// This deliberately stores decoded BRR blocks instead of exposing a second
/// public audio-memory model.  A voice uses Q12.12 phase, matching the useful
/// pitch convention where `$1000` advances one decoded sample per output
/// sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DspVoice {
    active: bool,
    release: bool,
    source: u8,
    block_address: u16,
    block_header: u8,
    block_samples: [i16; 16],
    block_loaded: bool,
    phase: u32,
    previous: i16,
    previous_previous: i16,
    envelope: u16,
}

impl Default for DspVoice {
    fn default() -> Self {
        Self {
            active: false,
            release: false,
            source: 0,
            block_address: 0,
            block_header: 0,
            block_samples: [0; 16],
            block_loaded: false,
            phase: 0,
            previous: 0,
            previous_previous: 0,
            envelope: 0,
        }
    }
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

#[derive(Clone, Copy)]
enum AluOp {
    Or,
    And,
    Eor,
    Compare,
    Adc,
    Sbc,
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
            timer_targets: [0; 3],
            timer_stage2: [0; 3],
            timer_outputs: [0; 3],
            timer_stage1_cycles: [0; 3],
            dsp_voices: [DspVoice::default(); 8],
            dsp_kon_seen: 0,
            dsp_kof_seen: 0,
            dsp_kon_pending: 0,
            dsp_kof_pending: 0,
        }
    }
}

const SPC_IPL_ROM: [u8; 64] = [
    0xcd, 0xef, 0xbd, 0xe8, 0x00, 0xc6, 0x1d, 0xd0, 0xfc, 0x8f, 0xaa, 0xf4, 0x8f, 0xbb, 0xf5, 0x78,
    0xcc, 0xf4, 0xd0, 0xfb, 0x2f, 0x19, 0xeb, 0xf4, 0xd0, 0xfc, 0x7e, 0xf4, 0xd0, 0x0b, 0xe4, 0xf5,
    0xcb, 0xf4, 0xd7, 0x00, 0xfc, 0xd0, 0xf3, 0xab, 0x01, 0x10, 0xef, 0x7e, 0xf4, 0x10, 0xeb, 0xba,
    0xf6, 0xda, 0x00, 0xba, 0xf4, 0xc4, 0xf4, 0xdd, 0x5d, 0xd0, 0xdb, 0x1f, 0x00, 0x00, 0xc0, 0xff,
];

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
        self.write_spc_control(value);
    }

    pub fn write_dsp_address(&mut self, value: u8) {
        self.dsp_address = value & 0x7f;
    }

    pub fn read_dsp_data(&self) -> u8 {
        self.read_dsp_register(self.dsp_address)
    }

    pub fn write_dsp_data(&mut self, value: u8) {
        self.write_dsp_register(self.dsp_address, value);
    }

    fn read_dsp_register(&self, address: u8) -> u8 {
        match address & 0x7f {
            0x08 | 0x18 | 0x28 | 0x38 | 0x48 | 0x58 | 0x68 | 0x78 => {
                let voice = usize::from(address >> 4);
                let level = self.dsp_voices[voice].envelope >> 8;
                level as u8
            }
            0x09 | 0x19 | 0x29 | 0x39 | 0x49 | 0x59 | 0x69 | 0x79 => {
                let voice = usize::from(address >> 4);
                let sample = self.dsp_voices[voice]
                    .block_samples
                    .get((self.dsp_voices[voice].phase >> 12) as usize)
                    .copied()
                    .unwrap_or(0);
                sample.clamp(-32_768, 32_767).to_be_bytes()[0]
            }
            0x7c => self.dsp[0x7c],
            _ => self.dsp[usize::from(address & 0x7f)],
        }
    }

    fn write_dsp_register(&mut self, address: u8, value: u8) {
        let address = address & 0x7f;
        match address {
            // ENVX and OUTX are generated by the DSP and are not writable.
            0x08 | 0x09 | 0x18 | 0x19 | 0x28 | 0x29 | 0x38 | 0x39 | 0x48 | 0x49 | 0x58 | 0x59
            | 0x68 | 0x69 | 0x78 | 0x79 => {}
            0x4c => {
                self.dsp[address as usize] = value;
                self.dsp_kon_pending |= value;
            }
            0x5c => {
                self.dsp[address as usize] = value;
                self.dsp_kof_pending |= value;
            }
            0x6c => {
                self.dsp[address as usize] = value;
                if value & 0x80 != 0 {
                    self.reset_dsp_voices();
                }
            }
            // Writing any value to ENDX clears the latched end flags.
            0x7c => self.dsp[address as usize] = 0,
            _ => self.dsp[address as usize] = value,
        }
    }

    /// Render a deterministic bounded batch of S-DSP samples.
    ///
    /// The implementation covers the useful ROM-facing path: BRR sample
    /// directory lookup, BRR decoding, pitch stepping, key-on/key-off, ADSR
    /// direct gain, per-voice volume, and master volume.  It intentionally
    /// does not claim full S-DSP parity: Gaussian interpolation, noise,
    /// echo, surround, and exact envelope-rate timing remain outside this
    /// bounded mixer.
    pub fn render_audio(&mut self, sample_count: usize) -> Vec<AudioSample> {
        self.sync_dsp_controls();
        let mut output = Vec::with_capacity(sample_count);

        for _ in 0..sample_count {
            let mut left = 0i32;
            let mut right = 0i32;

            if self.dsp[0x6c] & 0x40 == 0 {
                for voice in 0..8 {
                    if !self.dsp_voices[voice].active {
                        continue;
                    }

                    let sample = i32::from(self.next_voice_sample(voice));
                    let envelope = i32::from(self.step_voice_envelope(voice));
                    let base = voice * 0x10;
                    let volume_left = i32::from(self.dsp[base] as i8);
                    let volume_right = i32::from(self.dsp[base + 1] as i8);
                    let scaled = sample * envelope / 32_768;
                    left += scaled * volume_left / 128;
                    right += scaled * volume_right / 128;
                    self.update_voice_output_register(voice, sample);
                }
            }

            let master_left = i32::from(self.dsp[0x0c] as i8);
            let master_right = i32::from(self.dsp[0x1c] as i8);
            output.push(AudioSample {
                left: (left * master_left / 128).clamp(-32_768, 32_767) as i16,
                right: (right * master_right / 128).clamp(-32_768, 32_767) as i16,
            });
            self.sync_dsp_controls();
        }

        output
    }

    fn sync_dsp_controls(&mut self) {
        let kon = self.dsp[0x4c];
        let kof = self.dsp[0x5c];
        let key_on = self.dsp_kon_pending | (kon & !self.dsp_kon_seen);
        let key_off = self.dsp_kof_pending | (kof & !self.dsp_kof_seen);
        self.dsp_kon_pending = 0;
        self.dsp_kof_pending = 0;
        self.dsp_kon_seen = kon;
        self.dsp_kof_seen = kof;

        for voice in 0..8 {
            let bit = 1 << voice;
            if key_off & bit != 0 {
                self.dsp_voices[voice].release = true;
            }
            if key_on & bit != 0 {
                self.start_voice(voice);
            }
        }
    }

    fn start_voice(&mut self, voice: usize) {
        let source = self.dsp[voice * 0x10 + 4];
        let directory = u16::from(self.dsp[0x5d]) << 8;
        let entry = directory.wrapping_add(u16::from(source) * 4);
        let start = u16::from_le_bytes([
            self.ram[entry as usize],
            self.ram[entry.wrapping_add(1) as usize],
        ]);
        self.dsp_voices[voice] = DspVoice {
            active: true,
            release: false,
            source,
            block_address: start,
            envelope: 0,
            ..DspVoice::default()
        };
    }

    fn reset_dsp_voices(&mut self) {
        self.dsp_voices = [DspVoice::default(); 8];
        self.dsp[0x7c] = 0;
        self.dsp_kon_pending = 0;
        self.dsp_kof_pending = 0;
    }

    fn next_voice_sample(&mut self, voice: usize) -> i16 {
        if !self.dsp_voices[voice].block_loaded {
            self.load_brr_block(voice);
        }

        let index = (self.dsp_voices[voice].phase >> 12) as usize;
        let sample = self.dsp_voices[voice]
            .block_samples
            .get(index.min(15))
            .copied()
            .unwrap_or(0);
        let pitch = u32::from(self.dsp[voice * 0x10 + 2])
            | (u32::from(self.dsp[voice * 0x10 + 3] & 0x3f) << 8);
        self.dsp_voices[voice].phase = self.dsp_voices[voice].phase.wrapping_add(pitch);

        while self.dsp_voices[voice].phase >> 12 >= 16 && self.dsp_voices[voice].active {
            self.dsp_voices[voice].phase -= 16 << 12;
            if self.dsp_voices[voice].block_header & 0x01 != 0 {
                if self.dsp_voices[voice].block_header & 0x02 == 0 {
                    self.dsp_voices[voice].active = false;
                    self.dsp[0x7c] |= 1 << voice;
                    break;
                }
                let directory = u16::from(self.dsp[0x5d]) << 8;
                let entry = directory.wrapping_add(u16::from(self.dsp_voices[voice].source) * 4);
                self.dsp_voices[voice].block_address = u16::from_le_bytes([
                    self.ram[entry.wrapping_add(2) as usize],
                    self.ram[entry.wrapping_add(3) as usize],
                ]);
            } else {
                self.dsp_voices[voice].block_address =
                    self.dsp_voices[voice].block_address.wrapping_add(9);
            }
            self.dsp_voices[voice].block_loaded = false;
            self.load_brr_block(voice);
        }

        sample
    }

    fn load_brr_block(&mut self, voice: usize) {
        let address = self.dsp_voices[voice].block_address;
        let header = self.ram[address as usize];
        let range = (header >> 4).min(12);
        let filter = (header >> 2) & 0x03;
        let mut previous = i32::from(self.dsp_voices[voice].previous);
        let mut previous_previous = i32::from(self.dsp_voices[voice].previous_previous);
        let mut samples = [0i16; 16];

        for (index, sample) in samples.iter_mut().enumerate() {
            let packed = self.ram[address.wrapping_add(1 + (index / 2) as u16) as usize];
            let nibble = if index & 1 == 0 {
                packed >> 4
            } else {
                packed & 0x0f
            };
            let signed = i32::from((nibble as i8) << 4) >> 4;
            let mut decoded = (signed << 12) >> range;
            decoded = match filter {
                1 => decoded + previous * 15 / 16,
                2 => decoded + previous * 61 / 32 - previous_previous * 15 / 16,
                3 => decoded + previous * 115 / 64 - previous_previous * 13 / 16,
                _ => decoded,
            };
            decoded = decoded.clamp(-32_768, 32_767);
            *sample = decoded as i16;
            previous_previous = previous;
            previous = decoded;
        }

        self.dsp_voices[voice].block_header = header;
        self.dsp_voices[voice].block_samples = samples;
        self.dsp_voices[voice].previous_previous = previous_previous as i16;
        self.dsp_voices[voice].previous = previous as i16;
        self.dsp_voices[voice].block_loaded = true;
    }

    fn step_voice_envelope(&mut self, voice: usize) -> u16 {
        let base = voice * 0x10;
        let gain = self.dsp[base + 7];
        let state = &mut self.dsp_voices[voice];
        if state.release {
            state.envelope = state.envelope.saturating_sub(0x0400);
            if state.envelope == 0 {
                state.active = false;
            }
            return state.envelope;
        }

        if gain & 0x80 == 0 {
            state.envelope = u16::from(gain & 0x7f) << 8;
        } else {
            // Bounded ADSR approximation: attack uses ADSR1's rate nibble,
            // while sustain/decay are represented by a slow approach to the
            // configured sustain level. Exact DSP rate tables are deferred.
            let attack = u16::from((self.dsp[base + 5] & 0x0f).max(1));
            let sustain = u16::from((self.dsp[base + 6] >> 5) & 0x07) * 0x1000;
            state.envelope = state
                .envelope
                .saturating_add(attack * 0x0100)
                .min(0x7fff)
                .max(sustain);
        }
        state.envelope
    }

    fn update_voice_output_register(&mut self, voice: usize, sample: i32) {
        self.dsp[voice * 0x10 + 9] = (sample.clamp(-32_768, 32_767) >> 8) as i8 as u8;
    }

    /// Reads one of the SPC700's memory-mapped I/O registers.
    ///
    /// The timer output registers are read-and-clear, matching the hardware
    /// overlay rather than the backing RAM.  The write-only registers return
    /// zero; `$F8` and `$F9` remain ordinary RAM.
    pub fn read_spc_io(&mut self, address: u8) -> u8 {
        let value = match address {
            0xf0 | 0xf1 | 0xfa | 0xfb | 0xfc => 0,
            0xf2 => self.dsp_address,
            0xf3 => self.read_dsp_data(),
            0xf4..=0xf7 => self.ports[usize::from(address - 0xf4)],
            0xfd..=0xff => {
                let timer = usize::from(address - 0xfd);
                let value = self.timer_outputs[timer] & 0x0f;
                self.timer_outputs[timer] = 0;
                value
            }
            _ => self.ram[usize::from(address)],
        };
        self.ram[usize::from(address)] = value;
        value
    }

    /// Writes one of the SPC700's memory-mapped I/O registers.
    pub fn write_spc_io(&mut self, address: u8, value: u8) {
        match address {
            0xf1 => self.write_spc_control(value),
            0xf2 => self.write_dsp_address(value),
            0xf3 => self.write_dsp_data(value),
            0xf4..=0xf7 => {
                // These are SPC700 output ports.  The current public port
                // boundary intentionally keeps the pre-existing shared-port
                // behavior used by the console tests.
                self.ports[usize::from(address - 0xf4)] = value;
            }
            0xfa..=0xfc => {
                self.timer_targets[usize::from(address - 0xfa)] = value;
            }
            0xfd..=0xff => {}
            _ => {}
        }
        // I/O writes also reach the underlying RAM, as on the SPC700.
        self.ram[usize::from(address)] = value;
    }

    /// Enters the hardware IPL uploader without changing the simple PC=0
    /// setup used by the existing instruction tests.
    pub fn boot_spc700(&mut self) {
        self.control = 0xb0;
        self.timer_targets = [0; 3];
        self.timer_stage2 = [0; 3];
        self.timer_outputs = [0; 3];
        self.timer_stage1_cycles = [0; 3];
        self.spc700 = Spc700 {
            sp: 0xef,
            pc: 0xffc0,
            ..Spc700::default()
        };
    }

    pub fn step_spc700(&mut self) -> u32 {
        self.stage_spc_io();
        if self.control & 0x80 != 0 {
            self.ram[0xffc0..].copy_from_slice(&SPC_IPL_ROM);
        }
        let pc = self.spc700.pc;
        let opcode = self.ram[usize::from(pc)];
        let direct_io_write = self.direct_io_write_address(pc, opcode);
        let direct_io_read = self.direct_io_read_address(pc, opcode);
        let cycles = self.spc700.step(&mut self.ram);
        self.sync_spc_io(direct_io_write);
        if let Some(address @ 0xfd..=0xff) = direct_io_read {
            self.timer_outputs[usize::from(address - 0xfd)] = 0;
            self.ram[usize::from(address)] = 0;
        }
        self.advance_timers(cycles);
        cycles
    }

    fn stage_spc_io(&mut self) {
        self.ram[0x00f0] = 0;
        self.ram[0x00f1] = 0;
        self.ram[0x00f2] = self.dsp_address;
        self.ram[0x00f3] = self.read_dsp_data();
        self.ram[0x00f4..0x00f8].copy_from_slice(&self.ports);
        self.ram[0x00fa..0x00fd].fill(0);
        for timer in 0..3 {
            self.ram[0x00fd + timer] = self.timer_outputs[timer] & 0x0f;
        }
    }

    fn sync_spc_io(&mut self, direct_io_write: Option<u8>) {
        if self.ram[0x00f1] != 0 || direct_io_write == Some(0xf1) {
            self.write_spc_io(0xf1, self.ram[0x00f1]);
        }
        if self.ram[0x00f2] != self.dsp_address {
            self.write_spc_io(0xf2, self.ram[0x00f2]);
        }
        if self.ram[0x00f3] != self.read_dsp_data() {
            self.write_spc_io(0xf3, self.ram[0x00f3]);
        }
        for timer in 0..3 {
            let address = 0xfa + timer as u8;
            if self.ram[usize::from(address)] != 0 || direct_io_write == Some(address) {
                self.write_spc_io(address, self.ram[usize::from(address)]);
            }
        }
        self.ports.copy_from_slice(&self.ram[0x00f4..0x00f8]);
    }

    fn direct_io_write_address(&self, pc: u16, opcode: u8) -> Option<u8> {
        let operand = |offset: u16| self.ram[usize::from(pc.wrapping_add(offset))];
        let direct = |value: u8| self.spc700.direct_address(value) as u8;
        match opcode {
            0x8f => Some(direct(operand(2))), // MOV d,#i
            0xc4 => Some(direct(operand(1))), // MOV d,A
            0xd4 => Some(direct(operand(1).wrapping_add(self.spc700.x))),
            0xc6 => Some(direct(self.spc700.x)),
            0xd8 => Some(direct(operand(1))),
            0xcb => Some(direct(operand(1))),
            0xfa => Some(direct(operand(2))), // MOV dd,ds
            0x18 | 0x29 | 0x49 | 0x78 | 0x98 | 0xa9 => Some(direct(operand(2))),
            _ => None,
        }
    }

    fn direct_io_read_address(&self, pc: u16, opcode: u8) -> Option<u8> {
        let operand = |offset: u16| self.ram[usize::from(pc.wrapping_add(offset))];
        let direct = |value: u8| self.spc700.direct_address(value) as u8;
        match opcode {
            0x04 | 0x24 | 0x44 | 0x64 | 0x84 | 0xa4 | 0xe4 | 0xf8 | 0xeb => {
                Some(direct(operand(1)))
            }
            0x14 | 0x34 | 0x54 | 0x74 | 0x94 | 0xb4 | 0xf4 | 0xfb => {
                Some(direct(operand(1).wrapping_add(self.spc700.x)))
            }
            0x06 | 0x26 | 0x46 | 0x66 | 0x86 | 0xa6 | 0xe6 => Some(direct(self.spc700.x)),
            0xba => Some(direct(operand(1))),
            _ => None,
        }
    }

    fn write_spc_control(&mut self, value: u8) {
        let old = self.control;
        self.control = value;
        if value & 0x10 != 0 {
            self.ports[0] = 0;
            self.ports[1] = 0;
        }
        if value & 0x20 != 0 {
            self.ports[2] = 0;
            self.ports[3] = 0;
        }
        self.ram[0x00f4..0x00f8].copy_from_slice(&self.ports);
        for timer in 0..3 {
            let mask = 1 << timer;
            if old & mask == 0 && value & mask != 0 {
                self.timer_stage2[timer] = 0;
                self.timer_outputs[timer] = 0;
            }
        }
    }

    fn advance_timers(&mut self, cycles: u32) {
        for timer in 0..3 {
            let period = if timer == 2 { 16 } else { 128 };
            self.timer_stage1_cycles[timer] =
                self.timer_stage1_cycles[timer].saturating_add(cycles as u16);
            while self.timer_stage1_cycles[timer] >= period {
                self.timer_stage1_cycles[timer] -= period;
                if self.control & (1 << timer) == 0 {
                    continue;
                }
                self.timer_stage2[timer] = self.timer_stage2[timer].wrapping_add(1);
                if self.timer_stage2[timer] == self.timer_targets[timer] {
                    self.timer_stage2[timer] = 0;
                    self.timer_outputs[timer] = (self.timer_outputs[timer] + 1) & 0x0f;
                }
            }
        }
    }
}

impl Spc700 {
    pub fn step(&mut self, ram: &mut [u8; 65_536]) -> u32 {
        if self.stopped {
            return 0;
        }
        let opcode = ram[self.pc as usize];
        self.pc = self.pc.wrapping_add(1);
        if let Some(cycles) = self.step_extended(opcode, ram) {
            return cycles;
        }
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
            0x07 | 0x27 | 0x47 | 0x67 | 0x87 | 0xa7 => {
                let address = self.fetch_indirect_indexed_address(ram, self.x);
                let value = ram[usize::from(address)];
                match opcode {
                    0x07 => self.a |= value,
                    0x27 => self.a &= value,
                    0x47 => self.a ^= value,
                    0x67 => self.compare(self.a, value),
                    0x87 => self.adc(value),
                    0xa7 => self.sbc(value),
                    _ => unreachable!(),
                }
                if !matches!(opcode, 0x67 | 0x87 | 0xa7) {
                    self.set_zero_negative(self.a);
                }
                6
            }
            0x17 | 0x37 | 0x57 | 0x77 | 0x97 | 0xb7 => {
                let address = self.fetch_direct_indirect_y_address(ram);
                let value = ram[usize::from(address)];
                match opcode {
                    0x17 => self.a |= value,
                    0x37 => self.a &= value,
                    0x57 => self.a ^= value,
                    0x77 => self.compare(self.a, value),
                    0x97 => self.adc(value),
                    0xb7 => self.sbc(value),
                    _ => unreachable!(),
                }
                if !matches!(opcode, 0x77 | 0x97 | 0xb7) {
                    self.set_zero_negative(self.a);
                }
                6
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
                self.psw &= !0x48;
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
            0xc7 => {
                let address = self.fetch_indirect_indexed_address(ram, self.x);
                ram[usize::from(address)] = self.a;
                7
            }
            0xe6 => {
                let address = self.direct_address(self.x);
                self.a = ram[usize::from(address)];
                self.set_zero_negative(self.a);
                3
            }
            0xe7 => {
                let address = self.fetch_indirect_indexed_address(ram, self.x);
                self.a = ram[usize::from(address)];
                self.set_zero_negative(self.a);
                6
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
                self.x = self.sp;
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
                4
            }
            0xce => {
                self.x = self.pop(ram);
                4
            }
            0xee => {
                self.y = self.pop(ram);
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

    /// Execute the opcode families which are not part of the original
    /// bootstrap slice above.  Keeping this decoder separate makes the
    /// incremental coverage easy to audit while preserving the public CPU
    /// and snapshot surface.
    fn step_extended(&mut self, opcode: u8, ram: &mut [u8; 65_536]) -> Option<u32> {
        let cycles = match opcode {
            // TCALL n: the vector table occupies $FFC0-$FFDE, in reverse
            // order, with TCALL0 at $FFDE.
            op if op & 0x0f == 0x01 => {
                let vector = 0xffde_u16.wrapping_sub(u16::from(op >> 4) * 2);
                let target = self.read_word(ram, vector);
                self.push(ram, (self.pc >> 8) as u8);
                self.push(ram, self.pc as u8);
                self.pc = target;
                8
            }
            // SET1/CLR1 and their branch-on-bit counterparts.
            op if op & 0x1f == 0x02 => {
                let address = self.fetch_direct_address(ram);
                ram[usize::from(address)] |= 1 << (op >> 5);
                4
            }
            op if op & 0x1f == 0x12 => {
                let address = self.fetch_direct_address(ram);
                ram[usize::from(address)] &= !(1 << (op >> 5));
                4
            }
            op if op & 0x1f == 0x03 || op & 0x1f == 0x13 => {
                let address = self.fetch_direct_address(ram);
                let offset = self.fetch(ram) as i8;
                let bit = 1 << (op >> 5);
                let set = ram[usize::from(address)] & bit != 0;
                let take = if op & 0x1f == 0x03 { set } else { !set };
                if take {
                    self.pc = self.pc.wrapping_add_signed(i16::from(offset));
                }
                if take {
                    7
                } else {
                    5
                }
            }

            // Absolute and absolute-indexed A ALU forms.
            0x05 | 0x25 | 0x45 | 0x65 | 0x85 | 0xa5 => {
                let address = self.fetch_absolute_address(ram);
                self.apply_alu_to_a(ram[usize::from(address)], Self::alu_op(opcode));
                4
            }
            0x15 | 0x35 | 0x55 | 0x75 | 0x95 | 0xb5 => {
                let address = self
                    .fetch_absolute_address(ram)
                    .wrapping_add(u16::from(self.x));
                self.apply_alu_to_a(ram[usize::from(address)], Self::alu_op(opcode));
                5
            }
            0x16 | 0x36 | 0x56 | 0x76 | 0x96 | 0xb6 => {
                let address = self
                    .fetch_absolute_address(ram)
                    .wrapping_add(u16::from(self.y));
                self.apply_alu_to_a(ram[usize::from(address)], Self::alu_op(opcode));
                5
            }
            // Direct-page immediate and direct-page-to-direct-page ALU forms
            // encode the source operand before the destination operand.
            0x18 | 0x38 | 0x58 | 0x78 | 0x98 | 0xb8 => {
                let value = self.fetch(ram);
                let destination = self.fetch_direct_address(ram);
                self.apply_alu_to_memory(ram, destination, value, Self::alu_op(opcode));
                5
            }
            0x09 | 0x29 | 0x49 | 0x69 | 0x89 | 0xa9 => {
                let source = self.fetch_direct_address(ram);
                let destination = self.fetch_direct_address(ram);
                let value = ram[usize::from(source)];
                self.apply_alu_to_memory(ram, destination, value, Self::alu_op(opcode));
                6
            }
            0x19 | 0x39 | 0x59 | 0x79 | 0x99 | 0xb9 => {
                let source = self.direct_address(self.y);
                let destination = self.direct_address(self.x);
                let value = ram[usize::from(source)];
                self.apply_alu_to_memory(ram, destination, value, Self::alu_op(opcode));
                5
            }

            // Remaining register and memory transfers.
            0xe5 | 0xf5 | 0xf6 => {
                let base = self.fetch_absolute_address(ram);
                let index = if opcode == 0xf5 {
                    self.x
                } else if opcode == 0xf6 {
                    self.y
                } else {
                    0
                };
                self.a = ram[usize::from(base.wrapping_add(u16::from(index)))];
                self.set_zero_negative(self.a);
                if opcode == 0xe5 {
                    4
                } else {
                    5
                }
            }
            0xf7 => {
                let address = self.fetch_direct_indirect_y_address(ram);
                self.a = ram[usize::from(address)];
                self.set_zero_negative(self.a);
                6
            }
            0xf8 | 0xf9 | 0xe9 => {
                let address = if opcode == 0xe9 {
                    self.fetch_absolute_address(ram)
                } else {
                    let index = if opcode == 0xf9 { self.y } else { 0 };
                    self.fetch_direct_address(ram)
                        .wrapping_add(u16::from(index))
                };
                self.x = ram[usize::from(address)];
                self.set_zero_negative(self.x);
                if opcode == 0xe9 {
                    4
                } else if opcode == 0xf9 {
                    4
                } else {
                    3
                }
            }
            0xeb | 0xfb | 0xec => {
                let address = if opcode == 0xec {
                    self.fetch_absolute_address(ram)
                } else {
                    let index = if opcode == 0xfb { self.x } else { 0 };
                    self.fetch_direct_address(ram)
                        .wrapping_add(u16::from(index))
                };
                self.y = ram[usize::from(address)];
                self.set_zero_negative(self.y);
                if opcode == 0xec {
                    4
                } else {
                    4 - u32::from(opcode == 0xeb)
                }
            }
            0xaf => {
                let address = self.direct_address(self.x);
                ram[usize::from(address)] = self.a;
                self.x = self.x.wrapping_add(1);
                4
            }
            0xbf => {
                let address = self.direct_address(self.x);
                self.a = ram[usize::from(address)];
                self.x = self.x.wrapping_add(1);
                self.set_zero_negative(self.a);
                4
            }
            0xc5 | 0xd5 | 0xd6 => {
                let base = self.fetch_absolute_address(ram);
                let index = if opcode == 0xd5 {
                    self.x
                } else if opcode == 0xd6 {
                    self.y
                } else {
                    0
                };
                ram[usize::from(base.wrapping_add(u16::from(index)))] = self.a;
                if opcode == 0xc5 {
                    5
                } else {
                    6
                }
            }
            0xd7 => {
                let address = self.fetch_direct_indirect_y_address(ram);
                ram[usize::from(address)] = self.a;
                7
            }
            0xd8 | 0xd9 | 0xc9 => {
                let address = if opcode == 0xc9 {
                    self.fetch_absolute_address(ram)
                } else {
                    let index = if opcode == 0xd9 { self.y } else { 0 };
                    self.fetch_direct_address(ram)
                        .wrapping_add(u16::from(index))
                };
                ram[usize::from(address)] = self.x;
                if opcode == 0xc9 {
                    5
                } else if opcode == 0xd9 {
                    5
                } else {
                    4
                }
            }
            0xcb | 0xdb | 0xcc => {
                let address = if opcode == 0xcc {
                    self.fetch_absolute_address(ram)
                } else {
                    let index = if opcode == 0xdb { self.x } else { 0 };
                    self.fetch_direct_address(ram)
                        .wrapping_add(u16::from(index))
                };
                ram[usize::from(address)] = self.y;
                if opcode == 0xcc {
                    5
                } else if opcode == 0xdb {
                    5
                } else {
                    4
                }
            }
            0xfa => {
                let source = self.fetch_direct_address(ram);
                let destination = self.fetch_direct_address(ram);
                ram[usize::from(destination)] = ram[usize::from(source)];
                5
            }

            // Register compares.
            0xc8 => {
                let value = self.fetch(ram);
                self.compare(self.x, value);
                2
            }
            0x3e | 0x1e => {
                let address = if opcode == 0x3e {
                    self.fetch_direct_address(ram)
                } else {
                    self.fetch_absolute_address(ram)
                };
                self.compare(self.x, ram[usize::from(address)]);
                if opcode == 0x3e {
                    3
                } else {
                    4
                }
            }
            0xad => {
                let value = self.fetch(ram);
                self.compare(self.y, value);
                2
            }
            0x7e | 0x5e => {
                let address = if opcode == 0x7e {
                    self.fetch_direct_address(ram)
                } else {
                    self.fetch_absolute_address(ram)
                };
                self.compare(self.y, ram[usize::from(address)]);
                if opcode == 0x7e {
                    3
                } else {
                    4
                }
            }

            // Memory increment/decrement and shifts/rotates.
            0xab | 0xbb | 0xac => {
                let address = if opcode == 0xac {
                    self.fetch_absolute_address(ram)
                } else {
                    let index = if opcode == 0xbb { self.x } else { 0 };
                    self.fetch_direct_address(ram)
                        .wrapping_add(u16::from(index))
                };
                self.increment_memory(ram, address, true);
                if opcode == 0xac {
                    5
                } else if opcode == 0xbb {
                    5
                } else {
                    4
                }
            }
            0x8b | 0x9b | 0x8c => {
                let address = if opcode == 0x8c {
                    self.fetch_absolute_address(ram)
                } else {
                    let index = if opcode == 0x9b { self.x } else { 0 };
                    self.fetch_direct_address(ram)
                        .wrapping_add(u16::from(index))
                };
                self.increment_memory(ram, address, false);
                if opcode == 0x8c {
                    5
                } else if opcode == 0x9b {
                    5
                } else {
                    4
                }
            }
            0x0b | 0x1b | 0x0c | 0x2b | 0x3b | 0x2c | 0x4b | 0x5b | 0x4c | 0x6b | 0x7b | 0x6c => {
                let (address, operation) = match opcode {
                    0x0b => (self.fetch_direct_address(ram), 0),
                    0x1b => (self.direct_indexed_address(ram, self.x), 0),
                    0x0c => (self.fetch_absolute_address(ram), 0),
                    0x2b => (self.fetch_direct_address(ram), 2),
                    0x3b => (self.direct_indexed_address(ram, self.x), 2),
                    0x2c => (self.fetch_absolute_address(ram), 2),
                    0x4b => (self.fetch_direct_address(ram), 1),
                    0x5b => (self.direct_indexed_address(ram, self.x), 1),
                    0x4c => (self.fetch_absolute_address(ram), 1),
                    0x6b => (self.fetch_direct_address(ram), 3),
                    0x7b => (self.direct_indexed_address(ram, self.x), 3),
                    0x6c => (self.fetch_absolute_address(ram), 3),
                    _ => unreachable!(),
                };
                self.shift_memory(ram, address, operation);
                match opcode {
                    0x0c | 0x2c | 0x4c | 0x6c => 5,
                    0x1b | 0x3b | 0x5b | 0x7b => 5,
                    _ => 4,
                }
            }
            0x1c | 0x3c | 0x5c | 0x7c => {
                let operation = match opcode {
                    0x1c => 0,
                    0x3c => 2,
                    0x5c => 1,
                    0x7c => 3,
                    _ => unreachable!(),
                };
                self.a = self.shift_value(self.a, operation);
                2
            }

            // YA word operations and multiply/divide.
            0xba => {
                let address = self.fetch_direct_address(ram);
                let value = self.read_direct_word(ram, address);
                self.a = value as u8;
                self.y = (value >> 8) as u8;
                self.set_word_zero_negative(value);
                5
            }
            0xda => {
                let address = self.fetch_direct_address(ram);
                self.write_direct_word(ram, address, u16::from_le_bytes([self.a, self.y]));
                5
            }
            0x3a | 0x1a => {
                let address = self.fetch_direct_address(ram);
                let next = self.read_direct_word(ram, address);
                let result = if opcode == 0x3a {
                    next.wrapping_add(1)
                } else {
                    next.wrapping_sub(1)
                };
                self.write_direct_word(ram, address, result);
                self.set_word_zero_negative(result);
                6
            }
            0x7a | 0x9a | 0x5a => {
                let address = self.fetch_direct_address(ram);
                let rhs = self.read_direct_word(ram, address);
                let lhs = u16::from_le_bytes([self.a, self.y]);
                match opcode {
                    0x7a => self.add_word(lhs, rhs),
                    0x9a => self.sub_word(lhs, rhs),
                    0x5a => self.compare_word(lhs, rhs),
                    _ => unreachable!(),
                }
                5 - u32::from(opcode == 0x5a)
            }
            0xcf => {
                let result = u16::from(self.a) * u16::from(self.y);
                self.a = result as u8;
                self.y = (result >> 8) as u8;
                self.set_zero_negative(self.y);
                9
            }
            0x9e => {
                // DIV is not a simple unsigned divide when YA/X does not
                // fit in A.  This is the SPC700's documented overflow
                // behavior, including the X=0 case.
                let dividend = u32::from(u16::from_le_bytes([self.a, self.y]));
                let divisor = u32::from(self.x);
                let old_y = self.y;
                let (quotient, remainder) = if u32::from(old_y) < divisor * 2 {
                    if divisor == 0 {
                        (0xff, dividend & 0xff)
                    } else {
                        (dividend / divisor, dividend % divisor)
                    }
                } else {
                    (
                        0xff - (dividend - (divisor << 9)) / (256 - divisor),
                        divisor + (dividend - (divisor << 9)) % (256 - divisor),
                    )
                };
                self.a = quotient as u8;
                self.y = remainder as u8;
                self.psw = (self.psw & !0x48)
                    | if (self.x & 0x0f) <= (old_y & 0x0f) {
                        0x08
                    } else {
                        0
                    }
                    | if divisor == 0 || dividend / divisor > 0xff {
                        0x40
                    } else {
                        0
                    };
                self.set_zero_negative(self.a);
                12
            }
            0x9f => {
                self.a = self.a.rotate_left(4);
                self.set_zero_negative(self.a);
                5
            }

            // Decimal adjust.
            0xdf => {
                self.daa();
                3
            }
            0xbe => {
                self.das();
                3
            }

            // Looping branches and the absolute indirect jump.
            0x2e | 0xde => {
                let address = if opcode == 0x2e {
                    self.fetch_direct_address(ram)
                } else {
                    self.direct_indexed_address(ram, self.x)
                };
                let offset = self.fetch(ram) as i8;
                let take = self.a != ram[usize::from(address)];
                if take {
                    self.pc = self.pc.wrapping_add_signed(i16::from(offset));
                }
                if opcode == 0x2e {
                    if take {
                        7
                    } else {
                        5
                    }
                } else if take {
                    8
                } else {
                    6
                }
            }
            0x6e => {
                let address = self.fetch_direct_address(ram);
                let offset = self.fetch(ram) as i8;
                ram[usize::from(address)] = ram[usize::from(address)].wrapping_sub(1);
                let take = ram[usize::from(address)] != 0;
                if take {
                    self.pc = self.pc.wrapping_add_signed(i16::from(offset));
                }
                if take {
                    7
                } else {
                    5
                }
            }
            0xfe => {
                let offset = self.fetch(ram) as i8;
                self.y = self.y.wrapping_sub(1);
                if self.y != 0 {
                    self.pc = self.pc.wrapping_add_signed(i16::from(offset));
                    6
                } else {
                    4
                }
            }
            0x1f => {
                let address = self
                    .fetch_absolute_address(ram)
                    .wrapping_add(u16::from(self.x));
                self.pc = self.read_word(ram, address);
                6
            }

            // Calls, software interrupt, and return from interrupt.
            0x4f => {
                let target = 0xff00 | u16::from(self.fetch(ram));
                self.push(ram, (self.pc >> 8) as u8);
                self.push(ram, self.pc as u8);
                self.pc = target;
                6
            }
            0x0f => {
                let return_pc = self.pc;
                self.push(ram, (return_pc >> 8) as u8);
                self.push(ram, return_pc as u8);
                self.push(ram, self.psw);
                self.psw = (self.psw | 0x10) & !0x04;
                self.pc = self.read_word(ram, 0xffde);
                8
            }
            0x7f => {
                self.psw = self.pop(ram);
                let low = self.pop(ram);
                let high = self.pop(ram);
                self.pc = u16::from_le_bytes([low, high]);
                6
            }

            // Memory bit operations.  The encoded operand is a 13-bit
            // address plus a three-bit bit number.
            0x0a | 0x2a | 0x4a | 0x6a | 0x8a | 0xaa | 0xca | 0xea => {
                let (address, bit) = self.fetch_bit_address(ram);
                let mask = 1 << bit;
                let value = ram[usize::from(address)] & mask != 0;
                match opcode {
                    0x0a => self.set_carry(self.carry() || value),
                    0x2a => self.set_carry(self.carry() || !value),
                    0x4a => self.set_carry(self.carry() && value),
                    0x6a => self.set_carry(self.carry() && !value),
                    0x8a => self.set_carry(self.carry() ^ value),
                    0xaa => self.set_carry(value),
                    0xca => {
                        if self.carry() {
                            ram[usize::from(address)] |= mask;
                        } else {
                            ram[usize::from(address)] &= !mask;
                        }
                    }
                    0xea => ram[usize::from(address)] ^= mask,
                    _ => unreachable!(),
                }
                if matches!(opcode, 0x0a | 0x2a | 0x4a | 0x6a | 0x8a | 0xaa) {
                    4
                } else if opcode == 0xca {
                    6
                } else {
                    5
                }
            }
            0x0e | 0x4e => {
                let address = self.fetch_absolute_address(ram);
                let old = ram[usize::from(address)];
                self.set_zero_negative(self.a.wrapping_sub(old));
                if opcode == 0x0e {
                    ram[usize::from(address)] = old | self.a;
                } else {
                    ram[usize::from(address)] = old & !self.a;
                }
                6
            }
            _ => return None,
        };
        Some(cycles)
    }

    fn alu_op(opcode: u8) -> AluOp {
        match opcode {
            0x05 | 0x15 | 0x16 | 0x08 | 0x04 | 0x14 | 0x06 | 0x07 | 0x17 | 0x09 | 0x18 | 0x19 => {
                AluOp::Or
            }
            0x25 | 0x35 | 0x36 | 0x28 | 0x24 | 0x34 | 0x26 | 0x27 | 0x37 | 0x29 | 0x38 | 0x39 => {
                AluOp::And
            }
            0x45 | 0x55 | 0x56 | 0x48 | 0x44 | 0x54 | 0x46 | 0x47 | 0x57 | 0x49 | 0x58 | 0x59 => {
                AluOp::Eor
            }
            0x65 | 0x75 | 0x76 | 0x68 | 0x64 | 0x74 | 0x66 | 0x67 | 0x77 | 0x69 | 0x78 | 0x79 => {
                AluOp::Compare
            }
            0x85 | 0x95 | 0x96 | 0x88 | 0x84 | 0x94 | 0x86 | 0x87 | 0x97 | 0x89 | 0x98 | 0x99 => {
                AluOp::Adc
            }
            0xa5 | 0xb5 | 0xb6 | 0xa8 | 0xa4 | 0xb4 | 0xa6 | 0xa7 | 0xb7 | 0xa9 | 0xb8 | 0xb9 => {
                AluOp::Sbc
            }
            _ => unreachable!("not an SPC700 ALU opcode: {opcode:02x}"),
        }
    }

    fn apply_alu_to_a(&mut self, value: u8, operation: AluOp) {
        match operation {
            AluOp::Or => {
                self.a |= value;
                self.set_zero_negative(self.a);
            }
            AluOp::And => {
                self.a &= value;
                self.set_zero_negative(self.a);
            }
            AluOp::Eor => {
                self.a ^= value;
                self.set_zero_negative(self.a);
            }
            AluOp::Compare => self.compare(self.a, value),
            AluOp::Adc => self.adc(value),
            AluOp::Sbc => self.sbc(value),
        }
    }

    fn apply_alu_to_memory(
        &mut self,
        ram: &mut [u8; 65_536],
        destination: u16,
        value: u8,
        operation: AluOp,
    ) {
        let left = ram[usize::from(destination)];
        match operation {
            AluOp::Or => {
                let result = left | value;
                ram[usize::from(destination)] = result;
                self.set_zero_negative(result);
            }
            AluOp::And => {
                let result = left & value;
                ram[usize::from(destination)] = result;
                self.set_zero_negative(result);
            }
            AluOp::Eor => {
                let result = left ^ value;
                ram[usize::from(destination)] = result;
                self.set_zero_negative(result);
            }
            AluOp::Compare => self.compare(left, value),
            AluOp::Adc | AluOp::Sbc => {
                let saved_a = self.a;
                self.a = left;
                if matches!(operation, AluOp::Adc) {
                    self.adc(value);
                } else {
                    self.sbc(value);
                }
                let result = self.a;
                self.a = saved_a;
                ram[usize::from(destination)] = result;
            }
        }
    }

    fn fetch_absolute_address(&mut self, ram: &[u8; 65_536]) -> u16 {
        let low = self.fetch(ram);
        let high = self.fetch(ram);
        u16::from_le_bytes([low, high])
    }

    fn read_word(&self, ram: &[u8; 65_536], address: u16) -> u16 {
        let low = ram[usize::from(address)];
        let high = ram[usize::from(address.wrapping_add(1))];
        u16::from_le_bytes([low, high])
    }

    fn read_direct_word(&self, ram: &[u8; 65_536], address: u16) -> u16 {
        let low = ram[usize::from(address)];
        let high_address = self.direct_address((address as u8).wrapping_add(1));
        let high = ram[usize::from(high_address)];
        u16::from_le_bytes([low, high])
    }

    fn write_direct_word(&self, ram: &mut [u8; 65_536], address: u16, value: u16) {
        let [low, high] = value.to_le_bytes();
        ram[usize::from(address)] = low;
        let high_address = self.direct_address((address as u8).wrapping_add(1));
        ram[usize::from(high_address)] = high;
    }

    fn fetch_bit_address(&mut self, ram: &[u8; 65_536]) -> (u16, u8) {
        let encoded = self.fetch_absolute_address(ram);
        (encoded & 0x1fff, (encoded >> 13) as u8)
    }

    fn increment_memory(&mut self, ram: &mut [u8; 65_536], address: u16, increment: bool) {
        let value = if increment {
            ram[usize::from(address)].wrapping_add(1)
        } else {
            ram[usize::from(address)].wrapping_sub(1)
        };
        ram[usize::from(address)] = value;
        self.set_zero_negative(value);
    }

    fn shift_memory(&mut self, ram: &mut [u8; 65_536], address: u16, operation: u8) {
        let value = ram[usize::from(address)];
        let result = self.shift_value(value, operation);
        ram[usize::from(address)] = result;
    }

    fn shift_value(&mut self, value: u8, operation: u8) -> u8 {
        let carry_in = u8::from(self.carry());
        let (result, carry_out) = match operation {
            0 => (value << 1, value & 0x80 != 0),
            1 => (value >> 1, value & 0x01 != 0),
            2 => (value << 1 | carry_in, value & 0x80 != 0),
            3 => (value >> 1 | carry_in << 7, value & 0x01 != 0),
            _ => unreachable!(),
        };
        self.set_carry(carry_out);
        self.set_zero_negative(result);
        result
    }

    fn add_word(&mut self, left: u16, right: u16) {
        // ADDW starts a fresh word addition; the incoming C flag is not a
        // carry-in (unlike ADC).  Carry is produced by the 16-bit result.
        let sum = u32::from(left) + u32::from(right);
        let result = sum as u16;
        let overflow = (!(left ^ right) & (left ^ result) & 0x8000) != 0;
        let half_carry = (left & 0x0fff) + (right & 0x0fff) > 0x0fff;
        self.a = result as u8;
        self.y = (result >> 8) as u8;
        self.set_word_flags(result, sum > 0xffff, overflow, half_carry);
    }

    fn sub_word(&mut self, left: u16, right: u16) {
        // SUBW also starts a fresh word operation; C is the result flag and
        // does not provide an incoming borrow.
        let difference = i32::from(left) - i32::from(right);
        let result = difference as u16;
        let overflow = ((left ^ right) & (left ^ result) & 0x8000) != 0;
        let half_borrow = i32::from(left & 0x0fff) - i32::from(right & 0x0fff) < 0;
        self.a = result as u8;
        self.y = (result >> 8) as u8;
        self.set_word_flags(result, difference >= 0, overflow, !half_borrow);
    }

    fn compare_word(&mut self, left: u16, right: u16) {
        let result = left.wrapping_sub(right);
        self.psw = (self.psw & !0x83)
            | if result == 0 { 0x02 } else { 0 }
            | if result & 0x8000 != 0 { 0x80 } else { 0 }
            | if left >= right { 0x01 } else { 0 };
    }

    fn set_word_flags(&mut self, result: u16, carry: bool, overflow: bool, half_carry: bool) {
        self.psw = (self.psw & !0xc9)
            | if result & 0x8000 != 0 { 0x80 } else { 0 }
            | if overflow { 0x40 } else { 0 }
            | if half_carry { 0x08 } else { 0 }
            | if result == 0 { 0x02 } else { 0 }
            | if carry { 0x01 } else { 0 };
    }

    fn daa(&mut self) {
        let mut result = self.a;
        let mut carry = self.carry();
        if result > 0x99 || carry {
            result = result.wrapping_add(0x60);
            carry = true;
        }
        if result & 0x0f > 0x09 || self.psw & 0x08 != 0 {
            result = result.wrapping_add(0x06);
        }
        self.a = result;
        self.set_carry(carry);
        self.set_zero_negative(result);
    }

    fn das(&mut self) {
        let mut result = self.a;
        let mut carry = self.carry();
        if result > 0x99 || !carry {
            result = result.wrapping_sub(0x60);
            carry = false;
        }
        if result & 0x0f > 0x09 || self.psw & 0x08 == 0 {
            result = result.wrapping_sub(0x06);
        }
        self.a = result;
        self.set_carry(carry);
        self.set_zero_negative(result);
    }

    fn carry(&self) -> bool {
        self.psw & 0x01 != 0
    }

    fn set_carry(&mut self, carry: bool) {
        if carry {
            self.psw |= 0x01;
        } else {
            self.psw &= !0x01;
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

    fn fetch_indirect_indexed_address(&mut self, ram: &[u8; 65_536], index: u8) -> u16 {
        let operand = self.fetch(ram);
        let pointer = self.direct_address(operand.wrapping_add(index));
        let low = ram[usize::from(pointer)];
        let high =
            ram[usize::from(self.direct_address(operand.wrapping_add(index).wrapping_add(1)))];
        u16::from_le_bytes([low, high])
    }

    fn fetch_direct_indirect_y_address(&mut self, ram: &[u8; 65_536]) -> u16 {
        let operand = self.fetch(ram);
        let pointer = self.direct_address(operand);
        let low = ram[usize::from(pointer)];
        let high = ram[usize::from(self.direct_address(operand.wrapping_add(1)))];
        u16::from_le_bytes([low, high]).wrapping_add(u16::from(self.y))
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

    fn set_word_zero_negative(&mut self, value: u16) {
        self.psw = (self.psw & !0x82)
            | if value == 0 { 0x02 } else { 0 }
            | if value & 0x8000 != 0 { 0x80 } else { 0 };
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

    #[test]
    fn spc700_indirect_pointer_forms_read_and_write_through_direct_page() {
        let mut apu = Apu::default();
        apu.ram[0x10] = 0x00;
        apu.ram[0x11] = 0x20;
        apu.ram[0x2000] = 0x0f;
        apu.ram[..8].copy_from_slice(&[
            0xe8, 0xf0, // MOV A,#$f0
            0x07, 0x10, // OR A,[10+X]
            0xe7, 0x10, // MOV A,[10+X]
            0xc7, 0x10, // MOV [10+X],A
        ]);

        apu.step_spc700();
        assert_eq!(apu.spc700.a, 0xf0);
        apu.step_spc700();
        assert_eq!(apu.spc700.a, 0xff);
        apu.step_spc700();
        assert_eq!(apu.spc700.a, 0x0f);
        apu.step_spc700();
        assert_eq!(apu.spc700.a, 0x0f);
        assert_eq!(apu.ram[0x2000], 0x0f);
    }

    #[test]
    fn spc700_absolute_indexed_and_register_compare_families() {
        let mut apu = Apu::default();
        apu.ram[0x2000] = 0x0f;
        apu.ram[0x2001] = 0xf0;
        apu.ram[0x2002] = 0x01;
        apu.ram[..24].copy_from_slice(&[
            0xe8, 0x0f, // MOV A,#$0f
            0x05, 0x00, 0x20, // OR A,!$2000
            0x25, 0x01, 0x20, // AND A,!$2001
            0x45, 0x00, 0x20, // EOR A,!$2000
            0xcd, 0x01, // MOV X,#$01
            0x15, 0x00, 0x20, // OR A,!$2000+X
            0x8d, 0x02, // MOV Y,#$02
            0x85, 0x02, 0x20, // ADC A,!$2002
            0x65, 0x02, 0x20, // CMP A,!$2002
        ]);

        for _ in 0..9 {
            apu.step_spc700();
        }

        assert_eq!(apu.spc700.a, 0x00);
        assert_eq!(apu.spc700.psw & 0x01, 0);
        assert_eq!(apu.spc700.x, 1);
        assert_eq!(apu.spc700.y, 2);
    }

    #[test]
    fn spc700_direct_memory_alu_and_xy_pointer_families() {
        let mut apu = Apu::default();
        apu.spc700.x = 0x20;
        apu.spc700.y = 0x21;
        apu.ram[0x30] = 0x0f;
        apu.ram[0x31] = 0x01;
        apu.ram[0x40] = 0x0f;
        apu.ram[0x20] = 0x55;
        apu.ram[0x21] = 0x0f;
        apu.ram[..19].copy_from_slice(&[
            0x18, 0xf0, 0x40, // OR $40,#$f0
            0x29, 0x30, 0x40, // AND $40,$30
            0x49, 0x31, 0x40, // EOR $40,$31
            0x19, // OR (X),(Y)
            0x39, // AND (X),(Y)
            0x59, // EOR (X),(Y)
            0x80, // SETC
            0x89, 0x31, 0x40, // ADC $40,$31
            0xa9, 0x31, 0x40, // SBC $40,$31
        ]);

        for _ in 0..8 {
            apu.step_spc700();
        }

        // The eighth instruction is ADC: $0e + $01 + carry = $10.
        assert_eq!(apu.ram[0x40], 0x10);
        assert_eq!(apu.ram[0x20], 0x00);
    }

    #[test]
    fn spc700_absolute_and_indexed_memory_transfers_round_trip() {
        let mut apu = Apu::default();
        apu.ram[0x2000] = 0xa5;
        apu.ram[0x40] = 0x42;
        apu.ram[..30].copy_from_slice(&[
            0xe5, 0x00, 0x20, // MOV A,!$2000
            0xc5, 0x01, 0x20, // MOV !$2001,A
            0xcd, 0x01, // MOV X,#$01
            0xd5, 0x00, 0x20, // MOV !$2000+X,A
            0xf5, 0x00, 0x20, // MOV A,!$2000+X
            0x8d, 0x02, // MOV Y,#$02
            0xd6, 0x00, 0x20, // MOV !$2000+Y,A
            0xf6, 0x00, 0x20, // MOV A,!$2000+Y
            0xf8, 0x40, // MOV X,$40
            0xeb, 0x40, // MOV Y,$40
            0xd8, 0x11, // MOV $11,X
            0xcb, 0x12, // MOV $12,Y
        ]);

        for _ in 0..12 {
            apu.step_spc700();
        }

        assert_eq!(apu.ram[0x2001], 0xa5);
        assert_eq!(apu.ram[0x2002], 0xa5);
        assert_eq!(apu.ram[0x11], 0x42);
        assert_eq!(apu.ram[0x12], 0x42);
    }

    #[test]
    fn spc700_memory_modify_shift_and_bit_families_update_results() {
        let mut apu = Apu::default();
        apu.ram[0x40] = 0x01;
        apu.ram[0x2000] = 0x0f;
        apu.ram[..19].copy_from_slice(&[
            0xab, 0x40, // INC $40
            0x0b, 0x40, // ASL $40
            0x4b, 0x40, // LSR $40
            0x02, 0x40, // SET1 $40.0
            0x12, 0x40, // CLR1 $40.0
            0xaa, 0x00, 0x20, // MOV1 C,$1000.0 (clear)
            0xca, 0x00, 0x20, // MOV1 $1000.0,C
            0x0e, 0x00, 0x20, // TSET1 !$2000
        ]);

        for _ in 0..8 {
            apu.step_spc700();
        }

        assert_eq!(apu.ram[0x40], 0x02);
        assert_eq!(apu.ram[0x2000], 0x0f | apu.spc700.a);
        assert_eq!(apu.spc700.psw & 0x02, 0);
    }

    #[test]
    fn spc700_word_arithmetic_multiply_divide_and_decimal_families() {
        let mut apu = Apu::default();
        apu.ram[0x20] = 0x34;
        apu.ram[0x21] = 0x12;
        apu.ram[0x22] = 0x01;
        apu.ram[0x23] = 0x00;
        apu.ram[..19].copy_from_slice(&[
            0xba, 0x20, // MOVW YA,$20 -> $1234
            0x7a, 0x22, // ADDW YA,$22 -> $1235
            0xda, 0x24, // MOVW $24,YA
            0x3a, 0x24, // INCW $24
            0x1a, 0x24, // DECW $24
            0xcf, // MUL YA
            0xcd, 0x03, // MOV X,#$03
            0x9e, // DIV YA,X
            0xe0, // CLRV: clear the DIV-produced half-carry before DAA
            0xe8, 0x99, // MOV A,#$99
            0x80, // SETC
            0xdf, // DAA
        ]);

        for _ in 0..12 {
            apu.step_spc700();
        }

        assert_eq!(apu.ram[0x24], 0x35);
        assert_eq!(apu.ram[0x25], 0x12);
        assert_eq!(apu.spc700.a, 0xf9);
    }

    #[test]
    fn spc700_word_add_and_sub_start_without_an_incoming_carry() {
        let mut add = Apu::default();
        add.spc700.a = 0xff;
        add.spc700.y = 0x00;
        add.ram[0x20] = 0x01;
        add.ram[..3].copy_from_slice(&[0x80, 0x7a, 0x20]); // SETC; ADDW YA,$20
        add.step_spc700();
        add.step_spc700();
        assert_eq!(
            (u16::from(add.spc700.y) << 8) | u16::from(add.spc700.a),
            0x0100
        );

        let mut sub = Apu::default();
        sub.spc700.a = 0x00;
        sub.spc700.y = 0x01;
        sub.ram[0x20] = 0x01;
        sub.ram[..3].copy_from_slice(&[0x9a, 0x20, 0xff]); // SUBW YA,$20; STOP
        sub.step_spc700();
        assert_eq!(
            (u16::from(sub.spc700.y) << 8) | u16::from(sub.spc700.a),
            0x00ff
        );
    }

    #[test]
    fn spc700_word_load_and_modify_flags_use_the_full_word() {
        let mut load = Apu::default();
        load.ram[0x20] = 0x01;
        load.ram[0x21] = 0x00;
        load.ram[..2].copy_from_slice(&[0xba, 0x20]); // MOVW YA,$20 -> $0001
        assert_eq!(load.step_spc700(), 5);
        assert_eq!(
            (u16::from(load.spc700.y) << 8) | u16::from(load.spc700.a),
            0x0001
        );
        assert_eq!(load.spc700.psw & 0x82, 0);

        let mut increment = Apu::default();
        increment.ram[0x20] = 0x00;
        increment.ram[0x21] = 0x00;
        increment.ram[..2].copy_from_slice(&[0x3a, 0x20]); // INCW $20 -> $0001
        assert_eq!(increment.step_spc700(), 6);
        assert_eq!(increment.ram[0x20..=0x21], [0x01, 0x00]);
        assert_eq!(increment.spc700.psw & 0x02, 0);

        let mut decrement = Apu::default();
        decrement.ram[0x20] = 0x00;
        decrement.ram[0x21] = 0x01;
        decrement.ram[..2].copy_from_slice(&[0x1a, 0x20]); // DECW $20 -> $00ff
        assert_eq!(decrement.step_spc700(), 6);
        assert_eq!(decrement.ram[0x20..=0x21], [0xff, 0x00]);
        assert_eq!(decrement.spc700.psw & 0x02, 0);
    }

    #[test]
    fn spc700_division_handles_overflow_and_zero_divisor_cases() {
        let mut overflow = Apu::default();
        overflow.spc700.a = 0x00;
        overflow.spc700.y = 0x20;
        overflow.spc700.x = 0x10;
        overflow.ram[0] = 0x9e; // DIV YA,X: YA/X is outside A's range.
        overflow.step_spc700();
        assert_eq!(overflow.spc700.a, 0xff);
        assert_eq!(overflow.spc700.y, 0x10);
        assert_ne!(overflow.spc700.psw & 0x40, 0);

        let mut zero_divisor = Apu::default();
        zero_divisor.spc700.a = 0x34;
        zero_divisor.spc700.y = 0x12;
        zero_divisor.ram[0] = 0x9e;
        zero_divisor.step_spc700();
        assert_eq!(zero_divisor.spc700.a, 0xed);
        assert_eq!(zero_divisor.spc700.y, 0x34);
        assert_ne!(zero_divisor.spc700.psw & 0x40, 0);
    }

    #[test]
    fn spc700_loop_bit_branch_and_control_flow_families() {
        let mut apu = Apu::default();
        apu.ram[0x40] = 1;
        apu.ram[0xffde] = 0x00;
        apu.ram[0xffdf] = 0x01;
        apu.ram[..13].copy_from_slice(&[
            0x03, 0x40, 0x01, // BBS $40.0, skip the NOP
            0x00, 0x2e, 0x40, 0x02, // CBNE $40,+2
            0x00, 0x00, 0x6e, 0x40, 0xfb, // DBNZ $40,-5
            0x0f, // BRK
        ]);
        apu.ram[0x0100] = 0x7f; // RETI from the BRK vector

        for _ in 0..5 {
            apu.step_spc700();
        }

        assert_eq!(apu.spc700.pc, 13);
        assert_eq!(apu.ram[0x40], 0);
        assert_eq!(apu.spc700.sp, 0xef);
    }

    #[test]
    fn spc700_io_overlay_reads_ports_and_round_trips_dsp_registers() {
        let mut apu = Apu::default();
        apu.write_cpu_port(0, 0x12);
        apu.ram[..11].copy_from_slice(&[
            0xe4, 0xf4, // MOV A,$F4
            0x8f, 0x25, 0xf2, // MOV $F2,#$25
            0x8f, 0x5a, 0xf3, // MOV $F3,#$5A
            0xe4, 0xf3, // MOV A,$F3
            0xff,
        ]);

        assert_eq!(apu.step_spc700(), 3);
        assert_eq!(apu.spc700.a, 0x12);
        assert_eq!(apu.step_spc700(), 5);
        assert_eq!(apu.dsp_address, 0x25);
        assert_eq!(apu.step_spc700(), 5);
        assert_eq!(apu.dsp[0x25], 0x5a);
        assert_eq!(apu.step_spc700(), 3);
        assert_eq!(apu.spc700.a, 0x5a);

        apu.write_spc_io(0xf1, 0x01);
        apu.ram[apu.spc700.pc as usize..][..3].copy_from_slice(&[0x8f, 0x00, 0xf1]);
        assert_eq!(apu.step_spc700(), 5); // MOV $F1,#$00 disables timer 0
        assert_eq!(apu.control & 0x01, 0);
    }

    #[test]
    fn spc700_timers_tick_at_hardware_dividers_and_read_clear_outputs() {
        let mut apu = Apu::default();
        apu.write_spc_io(0xfa, 0x01); // timer 0: one stage-1 tick per output tick
        apu.write_spc_io(0xf1, 0x01); // enable timer 0
        apu.ram[0] = 0x00; // NOP, two SPC cycles

        for _ in 0..64 {
            assert_eq!(apu.step_spc700(), 2);
        }

        let pc = apu.spc700.pc as usize;
        apu.ram[pc..pc + 2].copy_from_slice(&[0xe4, 0xfd]); // MOV A,$FD
        assert_eq!(apu.step_spc700(), 3);
        assert_eq!(apu.spc700.a, 1);
        assert_eq!(apu.read_spc_io(0xfd), 0);

        let mut timer2 = Apu::default();
        timer2.write_spc_io(0xfc, 0x01);
        timer2.write_spc_io(0xf1, 0x04); // enable timer 2
        timer2.ram[0] = 0x00;
        for _ in 0..8 {
            timer2.step_spc700();
        }
        assert_eq!(timer2.read_spc_io(0xff), 1);
    }

    #[test]
    fn spc700_boot_entry_exposes_the_ipl_uploader() {
        let mut apu = Apu::default();
        apu.boot_spc700();

        assert_eq!(apu.spc700.pc, 0xffc0);
        assert_eq!(apu.spc700.sp, 0xef);
        assert_eq!(apu.control, 0xb0);
        assert_eq!(apu.step_spc700(), 2); // MOV X,#$EF
        assert_eq!(apu.spc700.x, 0xef);
        assert_eq!(apu.spc700.pc, 0xffc2);
    }

    #[test]
    fn dsp_registers_mask_addresses_and_protect_generated_state() {
        let mut apu = Apu::default();

        apu.write_dsp_address(0xff);
        assert_eq!(apu.dsp_address, 0x7f);
        apu.write_dsp_data(0x55);
        assert_eq!(apu.dsp[0x7f], 0x55);

        apu.write_dsp_address(0x78);
        apu.write_dsp_data(0x55);
        assert_eq!(apu.read_dsp_data(), 0);

        apu.dsp[0x7c] = 0xff;
        apu.write_dsp_address(0x7c);
        apu.write_dsp_data(0x01);
        assert_eq!(apu.dsp[0x7c], 0);
    }

    #[test]
    fn dsp_mixer_decodes_brr_and_applies_voice_and_master_volume() {
        let mut apu = Apu::default();
        // Source 0 in the directory at $1000 starts and loops at $2000.
        apu.dsp[0x5d] = 0x10;
        apu.ram[0x1000..0x1004].copy_from_slice(&[0x00, 0x20, 0x00, 0x20]);
        // End + loop, range 0/filter 0.  Nibbles are +7, 0, ... .
        apu.ram[0x2000] = 0x03;
        apu.ram[0x2001] = 0x70;
        apu.dsp[0x00] = 0x7f;
        apu.dsp[0x01] = 0x7f;
        apu.dsp[0x02] = 0x00;
        apu.dsp[0x03] = 0x10; // pitch $1000: one BRR sample per output sample
        apu.dsp[0x04] = 0;
        apu.dsp[0x07] = 0x7f; // direct gain
        apu.dsp[0x0c] = 0x7f;
        apu.dsp[0x1c] = 0x7f;

        apu.write_dsp_address(0x4c);
        apu.write_dsp_data(0x01);
        let samples = apu.render_audio(2);

        assert!(samples[0].left > 20_000);
        assert_eq!(samples[0].left, samples[0].right);
        assert_eq!(samples[1], AudioSample::default());
    }

    #[test]
    fn dsp_mixer_latches_endx_and_stops_non_looping_brr_voice() {
        let mut apu = Apu::default();
        apu.dsp[0x5d] = 0x10;
        apu.ram[0x1000..0x1004].copy_from_slice(&[0x00, 0x20, 0x00, 0x20]);
        apu.ram[0x2000] = 0x01; // end without loop
        apu.ram[0x2001] = 0x12;
        apu.dsp[0x00] = 0x7f;
        apu.dsp[0x01] = 0x7f;
        apu.dsp[0x02] = 0x00;
        apu.dsp[0x03] = 0x10;
        apu.dsp[0x07] = 0x7f;
        apu.dsp[0x0c] = 0x7f;
        apu.dsp[0x1c] = 0x7f;

        apu.write_dsp_address(0x4c);
        apu.write_dsp_data(0x01);
        let samples = apu.render_audio(17);

        assert_ne!(samples[0], AudioSample::default());
        assert_eq!(samples[16], AudioSample::default());
        assert_eq!(apu.dsp[0x7c] & 0x01, 0x01);
    }
}
