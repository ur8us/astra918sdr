#![no_std]

pub mod astra;
pub mod audio;
pub mod cat;
pub mod chip;
pub mod control_v2;
pub mod controls;
pub mod dsp;
mod dsp_tables;
pub mod persistence;
pub mod protocol;
pub mod recovery;
pub mod wide_fir;
pub const EXPERIMENTAL_MIN_HZ: u32 = 70_000;
pub const EXPERIMENTAL_MAX_HZ: u32 = 130_000_000;

pub const RATES: [u32; 5] = [12_000, 24_000, 48_000, 96_000, 240_000];
/// This transport mode uses the documented 100 kHz / 240 ksps CMX918 profile,
/// then deliberately retains every second complex sample before USB framing.
pub const USB_DECIMATED_RATE: u32 = 120_000;
pub const PROFILES: [(u32, u32); 7] = [
    (5_000, 12_000),
    (5_000, 24_000),
    (10_000, 24_000),
    (10_000, 48_000),
    (20_000, 48_000),
    (20_000, 96_000),
    (100_000, 240_000),
];
pub const EXPERIMENTAL_RF: u32 = 1;
pub const UNQUALIFIED_RATE: u32 = 2;
pub const SYNTHETIC: u32 = 4;
pub const XTAL_TRIM: u32 = 8;
/// Experimental rate-specific wide FIR2; capability byte 31 bit2.
pub const WIDE_FIR: u32 = 0x100;

/// PIO shifts MSB-first wire data (I high16, Q low16) into a u32.
/// USB stores each signed component little endian without changing any bits.
pub fn spi_word_to_iq(word: u32) -> [u8; 4] {
    word.rotate_left(16).to_le_bytes()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Config {
    pub frequency: u32,
    pub rate: u32,
    pub bandwidth: u32,
    pub flags: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    Protocol = 1,
    Frequency = 2,
    Profile = 3,
    Unqualified = 4,
    I2c = 5,
    Timeout = 6,
    Readback = 7,
    NotConfigured = 8,
    Capture = 9,
    Usb = 10,
}

impl Config {
    pub fn validate(self) -> Result<Self, Error> {
        if self.flags & !0x1ff != 0 || self.flags & 0xf0 != 0 && self.flags & XTAL_TRIM == 0 {
            return Err(Error::Protocol);
        }
        if !self.frequency.is_multiple_of(100) || !(70_000..=130_000_000).contains(&self.frequency)
        {
            return Err(Error::Frequency);
        }
        if !(150_000..=108_000_000).contains(&self.frequency) && self.flags & EXPERIMENTAL_RF == 0 {
            return Err(Error::Frequency);
        }
        if !PROFILES.contains(&(self.bandwidth, self.rate))
            && (self.rate, self.bandwidth) != (USB_DECIMATED_RATE, 100_000)
        {
            return Err(Error::Profile);
        }
        // This laboratory firmware has no board-qualified rate yet. The host
        // must explicitly opt into candidate rates to perform qualification.
        if self.flags & UNQUALIFIED_RATE == 0 {
            return Err(Error::Unqualified);
        }
        Ok(self)
    }

    pub fn synthetic(self) -> bool {
        self.flags & SYNTHETIC != 0
    }

    /// CMX918 sample rate. This differs from the USB stream rate only for the
    /// explicit 120 ksps decimated transport mode.
    pub fn chip_rate(self) -> u32 {
        if self.rate == USB_DECIMATED_RATE {
            240_000
        } else {
            self.rate
        }
    }

    pub fn decimation(self) -> usize {
        if self.rate == USB_DECIMATED_RATE {
            2
        } else {
            1
        }
    }

    pub fn xtal_control(self) -> u8 {
        // UM918/2.0 p.61, $97: cap code 0..15 = 1..16 pF; enable bit0.
        // Default cap code 6 unless explicitly supplied in flags bits7:4.
        let code = if self.flags & XTAL_TRIM != 0 {
            (self.flags >> 4) as u8
        } else {
            6
        };
        (code << 4) | 1
    }

    pub fn carrier(self) -> [u8; 3] {
        let word = self.frequency / 100;
        // UM918/2.0 p.13: bit7=0 high-side LO; unlike the contradictory DS
        // prose, this agrees with trace-backed LF tuning. Use high-side below
        // 2 MHz, low-side elsewhere; 96 kHz IF in all included profiles.
        [
            (word >> 16) as u8 | if self.frequency >= 2_000_000 { 0x80 } else { 0 },
            (word >> 8) as u8,
            word as u8,
        ]
    }

    pub fn bandwidth_code(self) -> u8 {
        match self.bandwidth {
            5_000 => 0,
            10_000 => 1,
            20_000 => 2,
            _ => 3,
        }
    }

    pub fn output_control(self, muted: bool) -> u8 {
        // UM p.28: SPI, active-low CS, I then Q, clock ratio x1 (smart=11).
        let chip_rate = self.chip_rate();
        let half_rate = PROFILES
            .iter()
            .any(|&(bw, rate)| bw == self.bandwidth && rate == chip_rate * 2);
        // 100 kHz has 240/480 ksps; 480 is deliberately not in PROFILES.
        0x93 | if half_rate || chip_rate == 240_000 {
            4
        } else {
            0
        } | if muted { 8 } else { 0 }
    }
}

/// Tracks a DMA circular write cursor. Hardware must be sampled more often
/// than one ring revolution; violation means unknown loss and stops capture.
pub struct RingCursor {
    pub consumed: u64,
    pub produced: u64,
    previous: usize,
    ring_words: usize,
}

impl RingCursor {
    pub fn new(ring_words: usize) -> Self {
        assert!(ring_words.is_power_of_two());
        Self {
            consumed: 0,
            produced: 0,
            previous: 0,
            ring_words,
        }
    }

    pub fn update(&mut self, position: usize, elapsed_us: u64, max_rate: u32) -> Result<(), Error> {
        if position >= self.ring_words
            || elapsed_us * u64::from(max_rate) >= self.ring_words as u64 * 1_000_000
        {
            return Err(Error::Capture);
        }
        let delta = position.wrapping_sub(self.previous) & (self.ring_words - 1);
        self.produced += delta as u64;
        self.previous = position;
        if self.produced - self.consumed >= self.ring_words as u64 {
            return Err(Error::Capture);
        }
        Ok(())
    }

    pub fn available(&self) -> usize {
        (self.produced - self.consumed) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        Config {
            frequency: 14_200_000,
            rate: 24_000,
            bandwidth: 5_000,
            flags: 2,
        }
    }
    #[test]
    fn accepted_profiles_and_explicit_usb_decimation() {
        for (bandwidth, rate) in PROFILES {
            let c = Config {
                bandwidth,
                rate,
                ..config()
            };
            assert_eq!(c.validate(), Ok(c));
            assert!(rate * 32 < 12_000_000);
        }
        for rate in [480_000, 960_000] {
            assert_eq!(Config { rate, ..config() }.validate(), Err(Error::Profile));
        }
        let decimated = Config {
            bandwidth: 100_000,
            rate: USB_DECIMATED_RATE,
            ..config()
        };
        assert_eq!(decimated.validate(), Ok(decimated));
        assert_eq!(decimated.chip_rate(), 240_000);
        assert_eq!(decimated.decimation(), 2);
    }
    #[test]
    fn frequency_boundaries_are_explicit() {
        assert_eq!(config().carrier(), [0x82, 0x2a, 0xb0]);
        for frequency in [70_000, 100_000, 149_900, 108_000_100, 130_000_000] {
            let c = Config {
                frequency,
                ..config()
            };
            assert_eq!(c.validate(), Err(Error::Frequency));
            assert!(Config { flags: 3, ..c }.validate().is_ok());
        }
        for frequency in [69_900, 130_000_100, 1_234_567, u32::MAX] {
            assert!(
                Config {
                    frequency,
                    flags: 3,
                    ..config()
                }
                .validate()
                .is_err()
            );
        }
        assert_eq!(
            Config {
                flags: 0,
                ..config()
            }
            .validate(),
            Err(Error::Unqualified)
        );
    }
    #[test]
    fn output_rate_bits() {
        for (bw, rate, half) in [
            (5000, 12000, true),
            (5000, 24000, false),
            (10000, 24000, true),
            (10000, 48000, false),
            (20000, 48000, true),
            (20000, 96000, false),
            (100000, 240000, true),
        ] {
            let c = Config {
                bandwidth: bw,
                rate,
                ..config()
            };
            assert_eq!(c.output_control(false) & 4 != 0, half);
            assert_eq!(c.output_control(true) & 8, 8);
        }
        let decimated = Config {
            bandwidth: 100_000,
            rate: USB_DECIMATED_RATE,
            ..config()
        };
        assert_eq!(decimated.output_control(false) & 4, 4);
    }
    #[test]
    fn ring_wrap_and_unknown_loss() {
        let mut cursor = RingCursor::new(1024);
        cursor.update(900, 1000, 240000).unwrap();
        cursor.consumed = 900;
        cursor.update(100, 1000, 240000).unwrap();
        assert_eq!(cursor.available(), 224);
        assert!(cursor.update(100, 5000, 240000).is_err());
        assert!(RingCursor::new(1024).update(1024, 1, 240000).is_err());
    }

    #[test]
    fn spi_conversion_preserves_signed_extremes_and_channel_order() {
        assert_eq!(spi_word_to_iq(0x8000_7fff), [0x00, 0x80, 0xff, 0x7f]);
        assert_eq!(spi_word_to_iq(0xffff_0001), [0xff, 0xff, 0x01, 0x00]);
    }

    #[test]
    fn unread_ring_overwrite_is_rejected() {
        let mut ring = RingCursor::new(1024);
        ring.update(900, 1000, 240000).unwrap();
        assert_eq!(ring.update(100, 1000, 240000), Err(Error::Capture));
    }
}
