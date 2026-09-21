//! Bounded two-stage complex channelizer. Coefficients are built only on configure.
use crate::dsp_tables::SINE;
use crate::{astra::Settings, cat::Mode};
const DEC_TAPS: usize = 129;
const SSB_TAPS: usize = 513;
const PI: f32 = core::f32::consts::PI;

#[derive(Default)]
struct Nco {
    phase: u32,
    increment: u32,
}
impl Nco {
    fn new(hz: i64) -> Self {
        Self {
            phase: 0,
            increment: ((-hz * (1i64 << 32)) / 120_000) as u32,
        }
    }
    fn rotate(&mut self, i: f32, q: f32) -> (f32, f32) {
        if self.increment == 0 {
            return (i, q);
        }
        let index = (self.phase >> 22) as usize;
        self.phase = self.phase.wrapping_add(self.increment);
        let sin = f32::from(SINE[index]) / 32768.;
        let cos = f32::from(SINE[(index + 256) & 1023]) / 32768.;
        (i * cos - q * sin, i * sin + q * cos)
    }
}
fn window(n: usize, taps: usize) -> f32 {
    let a = 2. * PI * n as f32 / (taps - 1) as f32;
    0.42 - 0.5 * libm::cosf(a) + 0.08 * libm::cosf(2. * a)
}
fn sinc(x: f32) -> f32 {
    if libm::fabsf(x) < 0.00001 {
        1.
    } else {
        libm::sinf(PI * x) / (PI * x)
    }
}
pub struct Demodulator {
    fine: Nco,
    channel: Nco,
    dec_history: [(f32, f32); DEC_TAPS],
    dec_coeff: [f32; DEC_TAPS],
    dec_head: usize,
    dec_phase: usize,
    ssb_history: [(f32, f32); SSB_TAPS],
    ssb_coeff: [(f32, f32); SSB_TAPS],
    ssb_head: usize,
    ssb_phase: bool,
    pub clipped: u32,
}
impl Demodulator {
    pub fn new(settings: Settings) -> Self {
        let mut s = Self {
            fine: Nco::new(settings.center() - i64::from(settings.hardware().frequency)),
            channel: Nco::new(i64::from(settings.offset)),
            dec_history: [(0., 0.); DEC_TAPS],
            dec_coeff: [0.; DEC_TAPS],
            dec_head: 0,
            dec_phase: 0,
            ssb_history: [(0., 0.); SSB_TAPS],
            ssb_coeff: [(0., 0.); SSB_TAPS],
            ssb_head: 0,
            ssb_phase: false,
            clipped: 0,
        };
        let mut total = 0.;
        for n in 0..DEC_TAPS {
            let t = n as f32 - (DEC_TAPS - 1) as f32 / 2.;
            s.dec_coeff[n] =
                (16_000. / 120_000.) * sinc(16_000. * t / 120_000.) * window(n, DEC_TAPS);
            total += s.dec_coeff[n];
        }
        for c in &mut s.dec_coeff {
            *c /= total;
        }
        let width = f32::from(settings.high - settings.low);
        let center = f32::from(settings.high + settings.low) / 2.;
        let sign = if settings.mode == Mode::Usb { 1. } else { -1. };
        for n in 0..SSB_TAPS {
            let t = n as f32 - (SSB_TAPS - 1) as f32 / 2.;
            let amplitude = width / 24_000. * sinc(width * t / 24_000.) * window(n, SSB_TAPS);
            let angle = sign * 2. * PI * center * t / 24_000.;
            s.ssb_coeff[n] = (amplitude * libm::cosf(angle), amplitude * libm::sinf(angle));
        }
        s
    }
    fn sample(&mut self, v: f32) -> i16 {
        if !(-32768. ..=32767.).contains(&v) {
            self.clipped = self.clipped.saturating_add(1);
        }
        v.clamp(-32768., 32767.) as i16
    }
    /// Wide corrected I/Q plus an audio sample every ten inputs.
    pub fn process(&mut self, i: i16, q: i16) -> ((i16, i16), Option<i16>) {
        let (i, q) = self.fine.rotate(f32::from(i), f32::from(q));
        let wide = (self.sample(i), self.sample(q));
        let channel = self.channel.rotate(i, q);
        self.dec_history[self.dec_head] = channel;
        let newest = self.dec_head;
        self.dec_head = (self.dec_head + 1) % DEC_TAPS;
        self.dec_phase += 1;
        if self.dec_phase != 5 {
            return (wide, None);
        }
        self.dec_phase = 0;
        let (mut di, mut dq) = (0., 0.);
        let mut idx = newest;
        for c in self.dec_coeff {
            let (i, q) = self.dec_history[idx];
            di += c * i;
            dq += c * q;
            idx = if idx == 0 { DEC_TAPS - 1 } else { idx - 1 };
        }
        self.ssb_history[self.ssb_head] = (di, dq);
        let newest = self.ssb_head;
        self.ssb_head = (self.ssb_head + 1) % SSB_TAPS;
        self.ssb_phase = !self.ssb_phase;
        if self.ssb_phase {
            return (wide, None);
        }
        let mut value = 0.;
        let mut idx = newest;
        for (re, im) in self.ssb_coeff {
            let (i, q) = self.ssb_history[idx];
            value += re * i - im * q;
            idx = if idx == 0 { SSB_TAPS - 1 } else { idx - 1 };
        }
        (wide, Some(self.sample(value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tone(s: Settings, hz: f64) -> f64 {
        let mut d = Demodulator::new(s);
        let mut e = 0.;
        let mut count = 0;
        for n in 0..24_000 {
            let p = core::f64::consts::TAU * hz * n as f64 / 120_000.;
            if let (_, Some(v)) = d.process(
                (12_000. * libm::cos(p)) as i16,
                (12_000. * libm::sin(p)) as i16,
            ) && n > 12_000
            {
                e += f64::from(v).powi(2);
                count += 1;
            }
        }
        libm::sqrt(e / count as f64)
    }
    #[test]
    fn translated_sidebands_and_aliases() {
        for offset in [-45_000, 0, 45_000] {
            for mode in [Mode::Usb, Mode::Lsb] {
                let s = Settings {
                    offset,
                    mode,
                    dial: 14_200_049,
                    ..Settings::default()
                };
                let correction = s.center() - i64::from(s.hardware().frequency);
                let sign = if mode == Mode::Usb { 1. } else { -1. };
                let f = offset as f64 + correction as f64;
                let wanted = tone(s, f + sign * 1500.);
                assert!(
                    (8000. ..9000.).contains(&wanted),
                    "{offset} {mode:?}: {wanted}"
                );
                assert!(tone(s, f - sign * 1500.) < wanted / 1000.);
                assert!(tone(s, f + sign * 25_500.) < wanted / 1000.);
            }
        }
    }
    #[test]
    fn configurable_filter_and_reset() {
        let s = Settings {
            low: 1200,
            high: 1800,
            ..Settings::default()
        };
        assert!(tone(s, 1500.) > 8000.);
        assert!(tone(s, 800.) < 10.);
        let mut d = Demodulator::new(s);
        let mut count = 0;
        for _ in 0..12_000 {
            if let (_, Some(v)) = d.process(0, 0) {
                assert_eq!(v, 0);
                count += 1;
            }
        }
        assert_eq!(count, 1200);
    }
}
