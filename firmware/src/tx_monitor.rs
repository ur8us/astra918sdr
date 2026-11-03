//! USB playback to single-sideband I/Q. This DSP never controls the transmitter.
use crate::{cat::Mode, dsp::Nco};

const AUDIO_CAPACITY: usize = 256;
const PREFILL: usize = 64;
const HILBERT_TAPS: usize = 255;
const INTERP_TAPS: usize = 32;
const PHASES: usize = 10;
const SCALE: u64 = 1 << 32;
const FADE_SAMPLES: f32 = 120.;
pub const MAX_INPUT: usize = 54;

#[derive(Clone, Copy)]
pub struct Config {
    pub enabled: bool,
    pub offset: i32,
    pub mode: Mode,
}

/// A short playback queue. A stalled I/Q consumer cannot block USB playback.
pub struct AudioBuffer {
    samples: [i16; AUDIO_CAPACITY],
    read: usize,
    len: usize,
    discontinuity: bool,
}

impl AudioBuffer {
    pub const fn new() -> Self {
        Self {
            samples: [0; AUDIO_CAPACITY],
            read: 0,
            len: 0,
            discontinuity: false,
        }
    }

    pub fn clear(&mut self) {
        self.read = 0;
        self.len = 0;
        self.discontinuity = true;
    }

    pub fn push(&mut self, samples: &[i16]) {
        for &sample in samples {
            if self.len == AUDIO_CAPACITY {
                self.read = (self.read + 1) % AUDIO_CAPACITY;
                self.len -= 1;
                self.discontinuity = true;
            }
            self.samples[(self.read + self.len) % AUDIO_CAPACITY] = sample;
            self.len += 1;
        }
    }

    pub fn available(&mut self) -> (usize, bool) {
        let changed = core::mem::take(&mut self.discontinuity);
        (self.len, changed)
    }

    pub fn take(&mut self, destination: &mut [i16]) -> usize {
        let count = destination.len().min(self.len);
        for sample in &mut destination[..count] {
            *sample = self.samples[self.read];
            self.read = (self.read + 1) % AUDIO_CAPACITY;
        }
        self.len -= count;
        count
    }
}

impl Default for AudioBuffer {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Monitor {
    hilbert_coeff: [f32; 64],
    history: [f32; HILBERT_TAPS * 2],
    head: usize,
    interp_coeff: [[f32; INTERP_TAPS]; PHASES + 1],
    analytic: [(f32, f32); INTERP_TAPS * 2],
    analytic_head: usize,
    phase: u32,
    step: u32,
    shift: Nco,
    gain: f32,
    primed: bool,
    ready: bool,
}

fn blackman(position: f32, length: usize) -> f32 {
    if !(0. ..=(length - 1) as f32).contains(&position) {
        return 0.;
    }
    let angle = 2. * core::f32::consts::PI * position / (length - 1) as f32;
    0.42 - 0.5 * libm::cosf(angle) + 0.08 * libm::cosf(2. * angle)
}

impl Default for Monitor {
    fn default() -> Self {
        let mut monitor = Self {
            hilbert_coeff: [0.; 64],
            history: [0.; HILBERT_TAPS * 2],
            head: 0,
            interp_coeff: [[0.; INTERP_TAPS]; PHASES + 1],
            analytic: [(0., 0.); INTERP_TAPS * 2],
            analytic_head: 0,
            phase: 0,
            step: (SCALE / 10) as u32,
            shift: Nco::new(0),
            gain: 0.,
            primed: false,
            ready: false,
        };
        for (index, coefficient) in monitor.hilbert_coeff.iter_mut().enumerate() {
            let distance = (2 * index + 1) as f32;
            *coefficient =
                2. / (core::f32::consts::PI * distance) * blackman(127. + distance, HILBERT_TAPS);
        }
        for (phase, coefficients) in monitor.interp_coeff.iter_mut().enumerate() {
            let fraction = phase as f32 / PHASES as f32;
            let mut total = 0.;
            for (index, coefficient) in coefficients.iter_mut().enumerate() {
                let position = index as f32 + fraction;
                let t = core::f32::consts::PI * (position - 15.5);
                let sinc = if t.abs() < 0.00001 {
                    1.
                } else {
                    libm::sinf(t) / t
                };
                *coefficient = sinc * blackman(position, INTERP_TAPS);
                total += *coefficient;
            }
            for coefficient in coefficients {
                *coefficient /= total;
            }
        }
        monitor
    }
}

impl Monitor {
    fn reset_history(&mut self) {
        self.history.fill(0.);
        self.analytic.fill((0., 0.));
        self.head = 0;
        self.analytic_head = 0;
        self.phase = 0;
    }

    /// Compute a bounded input batch before briefly locking the playback queue.
    pub fn prepare(
        &mut self,
        output_samples: usize,
        active: bool,
        available: usize,
        discontinuity: bool,
    ) -> usize {
        if discontinuity && active {
            self.primed = false;
            self.gain = 0.;
            self.reset_history();
        }
        self.ready = false;
        if !active || (!self.primed && available < PREFILL) {
            self.primed = false;
            return 0;
        }
        if !self.primed {
            self.reset_history();
            self.gain = 0.;
            self.primed = true;
        }
        // Follow small differences between USB playback and I/Q clocks. Limit
        // correction to 200 ppm and retain fractional phase across every frame.
        let ppm = ((available as i32 - PREFILL as i32) * 4).clamp(-200, 200);
        self.step = ((SCALE as i64 * (1_000_000 + i64::from(ppm))) / 10_000_000) as u32;
        let needed = self.input_count(output_samples);
        if needed > available || needed > MAX_INPUT {
            self.primed = false;
            return 0;
        }
        self.ready = true;
        needed
    }

    fn push(&mut self, sample: i16) {
        self.history[self.head] = f32::from(sample);
        self.history[self.head + HILBERT_TAPS] = f32::from(sample);
        self.head = (self.head + 1) % HILBERT_TAPS;
        let history = &self.history[self.head..self.head + HILBERT_TAPS];
        let mut q = 0.;
        for (index, &coefficient) in self.hilbert_coeff.iter().enumerate() {
            let distance = 2 * index + 1;
            q += coefficient * (history[127 - distance] - history[127 + distance]);
        }
        let iq = (history[127], q);
        self.analytic[self.analytic_head] = iq;
        self.analytic[self.analytic_head + INTERP_TAPS] = iq;
        self.analytic_head = (self.analytic_head + 1) % INTERP_TAPS;
    }

    /// Fill signed ci16 at 120 ksps. A short/absent input batch fades to silence.
    pub fn render(&mut self, output: &mut [u8], input: &[i16], config: Config) {
        let expected = self.input_count(output.len() / 4);
        self.ready &= config.enabled && input.len() == expected;
        if !self.ready && self.gain == 0. {
            output.fill(0);
            return;
        }
        self.shift.set_frequency(-i64::from(config.offset));
        let mut used = 0;
        for bytes in output.chunks_exact_mut(4) {
            let phase = u64::from(self.phase) + u64::from(self.step);
            self.phase = phase as u32;
            if phase >= SCALE {
                let sample = if self.ready {
                    let sample = input[used];
                    used += 1;
                    sample
                } else {
                    0
                };
                self.push(sample);
            }
            let table_phase = u64::from(self.phase) * PHASES as u64;
            let index = (table_phase >> 32) as usize;
            let fraction = (table_phase as u32) as f32 / SCALE as f32;
            let history = &self.analytic[self.analytic_head..self.analytic_head + INTERP_TAPS];
            let (mut i, mut q) = (0., 0.);
            for (tap, &(hi, hq)) in history.iter().rev().enumerate() {
                let a = self.interp_coeff[index][tap];
                let coefficient = a + fraction * (self.interp_coeff[index + 1][tap] - a);
                i += coefficient * hi;
                q += coefficient * hq;
            }
            if config.mode == Mode::Lsb {
                q = -q;
            }
            let (i, q) = self.shift.rotate(i, q);
            self.gain = (self.gain
                + if self.ready {
                    1. / FADE_SAMPLES
                } else {
                    -1. / FADE_SAMPLES
                })
            .clamp(0., 1.);
            let scale = 0.5 * self.gain;
            bytes[..2].copy_from_slice(&((i * scale).clamp(-32768., 32767.) as i16).to_le_bytes());
            bytes[2..].copy_from_slice(&((q * scale).clamp(-32768., 32767.) as i16).to_le_bytes());
        }
        if self.gain == 0. && !self.ready {
            self.reset_history();
        }
    }

    /// Input required for the next output chunk, retaining fractional phase.
    pub fn input_count(&self, output_samples: usize) -> usize {
        ((u64::from(self.phase) + output_samples as u64 * u64::from(self.step)) >> 32) as usize
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn render_tone(tone: f32, mode: Mode, offset: i32) -> Vec<(f32, f32)> {
        let mut monitor = Monitor::default();
        let mut buffer = AudioBuffer::new();
        let config = Config {
            enabled: true,
            mode,
            offset,
        };
        let mut sample = 0;
        let mut result = Vec::new();
        for _ in 0..45 {
            let mut audio = [0i16; PREFILL];
            let (available, _) = buffer.available();
            let count = PREFILL - available;
            for value in &mut audio[..count] {
                *value = (12000.
                    * libm::cosf(2. * core::f32::consts::PI * tone * sample as f32 / 12000.))
                    as i16;
                sample += 1;
            }
            buffer.push(&audio[..count]);
            let (available, discontinuity) = buffer.available();
            let count = monitor.prepare(512, true, available, discontinuity);
            let mut input = [0; MAX_INPUT];
            assert_eq!(buffer.take(&mut input[..count]), count);
            let mut frame = [0; 2048];
            monitor.render(&mut frame, &input[..count], config);
            result.extend(frame.chunks_exact(4).map(|sample| {
                (
                    f32::from(i16::from_le_bytes(sample[..2].try_into().unwrap())),
                    f32::from(i16::from_le_bytes(sample[2..].try_into().unwrap())),
                )
            }));
        }
        result
    }

    fn amplitude(samples: &[(f32, f32)], frequency: f32) -> f32 {
        let mut sum = (0., 0.);
        for (index, &(i, q)) in samples.iter().enumerate() {
            let phase = 2. * core::f32::consts::PI * frequency * index as f32 / 120000.;
            let (sin, cos) = (libm::sinf(phase), libm::cosf(phase));
            sum.0 += i * cos + q * sin;
            sum.1 += q * cos - i * sin;
        }
        libm::sqrtf(sum.0 * sum.0 + sum.1 * sum.1) / samples.len() as f32
    }

    #[test]
    fn usb_lsb_and_offsets_preserve_audio_and_reject_the_mirror() {
        for mode in [Mode::Usb, Mode::Lsb] {
            for (tone, offset) in [(100., 0), (1500., 10000), (5000., -45000)] {
                let all = render_tone(tone, mode, offset);
                let samples = &all[6000..18000];
                let sign = if mode == Mode::Usb { 1. } else { -1. };
                let wanted = amplitude(samples, offset as f32 + sign * tone);
                let mirror = amplitude(samples, offset as f32 - sign * tone);
                assert!(
                    wanted > 5000. && wanted < 6200.,
                    "tone={tone}, wanted={wanted}"
                );
                assert!(
                    mirror < wanted * 0.012,
                    "tone={tone}, mirror={mirror}, wanted={wanted}"
                );
            }
        }
    }

    #[test]
    fn missing_audio_and_disabled_monitor_fade_to_zero() {
        let mut monitor = Monitor::default();
        let config = Config {
            enabled: true,
            offset: 0,
            mode: Mode::Usb,
        };
        let mut frame = [0xff; 2048];
        for active in [false, true] {
            assert_eq!(monitor.prepare(512, active, 0, false), 0);
            monitor.render(&mut frame, &[], config);
            assert!(frame.iter().all(|&b| b == 0));
        }
        let needed = monitor.prepare(512, true, PREFILL, false);
        monitor.render(&mut frame, &[12000; MAX_INPUT][..needed], config);
        monitor.prepare(512, false, 0, false);
        monitor.render(&mut frame, &[], config);
        assert!(frame[120 * 4..].iter().all(|&b| b == 0));
        let needed = monitor.prepare(512, true, PREFILL, false);
        monitor.render(
            &mut frame,
            &[12000; MAX_INPUT][..needed],
            Config {
                enabled: false,
                ..config
            },
        );
        assert!(frame.iter().all(|&b| b == 0));
    }

    #[test]
    fn playback_queue_overflow_keeps_latest_audio_and_marks_discontinuity() {
        let mut buffer = AudioBuffer::new();
        for sample in 0..300 {
            buffer.push(&[sample]);
        }
        assert_eq!(buffer.available(), (256, true));
        assert_eq!(buffer.available(), (256, false));
        let mut samples = [0; 256];
        assert_eq!(buffer.take(&mut samples), 256);
        assert_eq!((samples[0], samples[255]), (44, 299));
        buffer.clear();
        assert_eq!(buffer.available(), (0, true));
    }

    #[test]
    fn fractional_input_phase_is_continuous_across_frames() {
        let mut monitor = Monitor::default();
        let config = Config {
            enabled: true,
            offset: 0,
            mode: Mode::Usb,
        };
        let mut consumed = 0;
        for _ in 0..100 {
            let needed = monitor.prepare(512, true, PREFILL, false);
            consumed += needed;
            monitor.render(&mut [0; 2048], &[0; MAX_INPUT][..needed], config);
        }
        assert!((consumed as i32 - 5120).abs() <= 1);
    }

    #[test]
    fn yielding_between_chunks_does_not_change_the_waveform() {
        let config = Config {
            enabled: true,
            offset: 10000,
            mode: Mode::Lsb,
        };
        let mut whole = Monitor::default();
        let mut chunked = Monitor::default();
        for index in 0..20 {
            let count = whole.prepare(512, true, PREFILL, false);
            assert_eq!(chunked.prepare(512, true, PREFILL, false), count);
            let mut input = [0; MAX_INPUT];
            for (n, sample) in input.iter_mut().enumerate() {
                *sample = ((n + index) * 347) as i16;
            }
            let mut expected = [0; 2048];
            let mut actual = [0; 2048];
            whole.render(&mut expected, &input[..count], config);
            let mut used = 0;
            for chunk in actual.chunks_exact_mut(256) {
                let end = used + chunked.input_count(64);
                chunked.render(chunk, &input[used..end], config);
                used = end;
            }
            assert_eq!(used, count);
            assert_eq!(actual, expected);
        }
    }
}
