//! Host-testable 12 kHz TX audio level and tone estimator.
//!
//! The estimator uses interpolated positive zero crossings over a rolling
//! window. Its outputs are measurements, not permission to key the radio.
pub const SAMPLE_RATE: u32 = 12_000;
const WINDOW: usize = 120;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measurement {
    pub rms: f32,
    pub frequency_hz: Option<f32>,
}

/// Audio-triggered PTT state. Call once per millisecond; missing or invalid
/// measurements are treated as silence. The caller owns all physical outputs.
#[derive(Default)]
pub struct Vox {
    keyed: bool,
    qualified_ms: u16,
    missing_ms: u16,
}

impl Vox {
    pub fn tick(
        &mut self,
        enabled: bool,
        measurement: Option<Measurement>,
        threshold_dbfs: i8,
        key_ms: u8,
        unkey_ms: u8,
    ) -> bool {
        if !enabled {
            *self = Self::default();
            return false;
        }
        let threshold = libm::powf(10.0, threshold_dbfs as f32 / 20.0);
        let present = measurement.is_some_and(|m| {
            m.rms >= threshold
                && m.frequency_hz
                    .is_some_and(|f| (100.0..=5000.0).contains(&f))
        });
        if present {
            self.missing_ms = 0;
            self.qualified_ms = self.qualified_ms.saturating_add(1);
            if self.qualified_ms >= u16::from(key_ms).max(1) {
                self.keyed = true;
            }
        } else {
            self.qualified_ms = 0;
            self.missing_ms = self.missing_ms.saturating_add(1);
            if self.missing_ms >= u16::from(unkey_ms).max(1) {
                self.keyed = false;
            }
        }
        self.keyed
    }
}

pub struct ToneEstimator {
    samples: [i16; WINDOW],
    next: usize,
    count: usize,
}

impl Default for ToneEstimator {
    fn default() -> Self {
        Self {
            samples: [0; WINDOW],
            next: 0,
            count: 0,
        }
    }
}

impl ToneEstimator {
    pub fn push(&mut self, samples: &[i16]) {
        for &sample in samples {
            self.samples[self.next] = sample;
            self.next = (self.next + 1) % WINDOW;
            self.count = (self.count + 1).min(WINDOW);
        }
    }

    pub fn measure(&self) -> Option<Measurement> {
        if self.count < WINDOW {
            return None;
        }
        let mut energy = 0.0f32;
        let mut first = None;
        let mut last = 0.0f32;
        let mut crossings = 0u32;
        let mut shortest = f32::MAX;
        let mut longest = 0.0f32;
        let mut previous = self.samples[self.next];
        energy += (previous as f32) * (previous as f32);
        for i in 1..WINDOW {
            let sample = self.samples[(self.next + i) % WINDOW];
            energy += (sample as f32) * (sample as f32);
            if previous < 0 && sample >= 0 {
                let crossing =
                    (i - 1) as f32 + -(previous as f32) / (sample as f32 - previous as f32);
                if first.is_none() {
                    first = Some(crossing);
                } else {
                    let period = crossing - last;
                    shortest = shortest.min(period);
                    longest = longest.max(period);
                }
                last = crossing;
                crossings += 1;
            }
            previous = sample;
        }
        let frequency_hz = match first {
            Some(start)
                if crossings >= 2
                    && last > start
                    && longest - shortest
                        <= (0.1 * (last - start) / (crossings - 1) as f32).max(0.25) =>
            {
                Some((crossings - 1) as f32 * SAMPLE_RATE as f32 / (last - start))
            }
            _ => None,
        };
        Some(Measurement {
            rms: libm::sqrtf(energy / WINDOW as f32) / 32768.0,
            frequency_hz,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_tone_and_silence() {
        let mut estimator = ToneEstimator::default();
        let mut tone = [0i16; WINDOW];
        for (i, sample) in tone.iter_mut().enumerate() {
            *sample = (12_000.0
                * libm::sinf(2.0 * core::f32::consts::PI * 1500.0 * i as f32 / SAMPLE_RATE as f32))
                as i16;
        }
        estimator.push(&tone);
        let measurement = estimator.measure().unwrap();
        assert!((measurement.frequency_hz.unwrap() - 1500.0).abs() < 1.0);
        assert!((measurement.rms - 0.259).abs() < 0.01);
        estimator.push(&[0; WINDOW]);
        let silence = estimator.measure().unwrap();
        assert_eq!(silence.frequency_hz, None);
        assert_eq!(silence.rms, 0.0);
    }

    #[test]
    fn follows_ft4_tone_changes_and_rejects_missing_playback() {
        let mut estimator = ToneEstimator::default();
        let mut phase = 0.0;
        for tone in [1200.0, 1220.8333, 1262.5, 1241.6667, 1200.0] {
            for _ in 0..20 {
                let mut packet = [0; 12];
                for sample in &mut packet {
                    phase += 2.0 * core::f32::consts::PI * tone / SAMPLE_RATE as f32;
                    phase %= 2.0 * core::f32::consts::PI;
                    *sample = (15000.0 * libm::sinf(phase)) as i16;
                }
                estimator.push(&packet);
            }
            assert!((estimator.measure().unwrap().frequency_hz.unwrap() - tone).abs() < 1.0);
        }
        let mut vox = Vox::default();
        for _ in 0..10 {
            vox.tick(true, estimator.measure(), -40, 10, 100);
        }
        assert!(vox.keyed);
        for _ in 0..100 {
            vox.tick(true, None, -40, 10, 100);
        }
        assert!(!vox.keyed);
    }

    #[test]
    fn vox_keys_and_unkeys_with_independent_delays() {
        let tone = Some(Measurement {
            rms: 0.1,
            frequency_hz: Some(1500.0),
        });
        let mut vox = Vox::default();
        for _ in 0..9 {
            assert!(!vox.tick(true, tone, -40, 10, 100));
        }
        assert!(vox.tick(true, tone, -40, 10, 100));
        for _ in 0..99 {
            assert!(vox.tick(true, None, -40, 10, 100));
        }
        assert!(!vox.tick(true, None, -40, 10, 100));
        for _ in 0..10 {
            vox.tick(true, tone, -40, 10, 100);
        }
        assert!(!vox.tick(false, tone, -40, 10, 100));
    }

    #[test]
    fn loud_noise_is_not_a_valid_tone() {
        let mut estimator = ToneEstimator::default();
        let mut noise = [0; WINDOW];
        let mut seed = 12345u32;
        for sample in &mut noise {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *sample = (seed >> 16) as i16;
        }
        estimator.push(&noise);
        assert_eq!(estimator.measure().unwrap().frequency_hz, None);
    }
}
