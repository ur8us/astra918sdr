//! Receiver controls, UM918/2.0 pp.15,17–18,23,26,65 and DS pp.20–21.
use crate::{Config, EXPERIMENTAL_MAX_HZ, EXPERIMENTAL_MIN_HZ, Error};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum RfInput {
    #[default]
    Auto = 0,
    Lf = 1,
    Hf = 2,
    Vhf = 3,
}
impl RfInput {
    pub fn parse(value: u8) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Auto),
            1 => Ok(Self::Lf),
            2 => Ok(Self::Hf),
            3 => Ok(Self::Vhf),
            _ => Err(Error::Profile),
        }
    }
    pub fn actual(self, frequency: u32) -> Self {
        if self != Self::Auto {
            self
        } else if frequency < 2_000_000 {
            Self::Lf
        } else if frequency < 40_000_000 {
            Self::Hf
        } else {
            Self::Vhf
        }
    }
    pub fn mask(self, frequency: u32) -> u8 {
        match self.actual(frequency) {
            Self::Lf => 4,
            Self::Hf => 2,
            Self::Vhf => 1,
            Self::Auto => unreachable!(),
        }
    }
    /// Preserve the calibrated LO but select the required mixer family through Fc.
    /// HF below 2 MHz was RF-tested in cmx918audiocat 5e4d7d9. The reverse
    /// override for forced LF is the same mechanism and requires RF validation.
    pub fn routing_carrier(self, config: Config) -> [u8; 3] {
        let hz = match self.actual(config.frequency) {
            Self::Lf => config.frequency.min(1_999_900),
            Self::Hf | Self::Vhf => config.frequency.max(2_000_000),
            Self::Auto => unreachable!(),
        };
        let mut fc = Config {
            frequency: hz,
            ..config
        }
        .carrier();
        fc[0] = (fc[0] & 0x1f) | (config.carrier()[0] & 0xc0);
        fc
    }
}

// LNA voltage gain in tenths of dB, UM $0F table. Nonuniform steps.
pub const RF_GAIN_DB10: [i16; 39] = [
    -89, -79, -65, -56, -46, -37, -26, -16, -5, 6, 16, 25, 36, 45, 56, 66, 76, 87, 96, 108, 120,
    132, 144, 155, 168, 179, 189, 202, 213, 226, 236, 246, 255, 265, 275, 285, 296, 305, 316,
];
// UM $20/$21 specifies 29.4 dB maximum; DS prose says 29.3 dB.
pub const IF_GAIN_DB10: [i16; 32] = {
    let mut a = [0; 32];
    let mut i = 0;
    while i < 32 {
        a[i] = -16 + i as i16 * 10;
        i += 1;
    }
    a
};
// UM918/2.0 p.20, $13. The LF UI uses one code for both stages so that a
// single gain slider has a monotonic, documented combined response.
pub const LF_LNA_GAIN_DB10: [i16; 16] = [
    25, 38, 50, 61, 73, 85, 96, 107, 118, 129, 139, 149, 158, 168, 168, 168,
];
pub const LF_MIX_GAIN_DB10: [i16; 16] = [
    10, 22, 34, 47, 59, 72, 84, 96, 109, 121, 134, 146, 159, 171, 184, 196,
];
// UM918/2.0 p.21, $16. These are voltage gains, hence negative attenuation.
pub const LF_ATTENUATOR_DB10: [i16; 16] = [
    -207, -193, -178, -165, -150, -137, -123, -110, -102, -88, -74, -60, -46, -32, -17, 0,
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
    pub input: RfInput,
    pub rf_auto: bool,
    pub if_auto: bool,
    pub rf_gain: u8,
    pub if_gain: u8,
    pub lf_gain: u8,
    pub lf_attenuator: u8,
    pub lf_mf_capacitor: u16,
}
impl Default for Controls {
    fn default() -> Self {
        Self {
            input: RfInput::Auto,
            rf_auto: true,
            if_auto: true,
            rf_gain: 38,
            if_gain: 31,
            lf_gain: 15,
            lf_attenuator: 15,
            lf_mf_capacitor: 0,
        }
    }
}
impl Controls {
    pub fn validate(self, _config: Config) -> Result<Self, Error> {
        if self.rf_gain >= 39
            || self.if_gain >= 32
            || self.lf_gain >= 16
            || self.lf_attenuator >= 16
            || self.lf_mf_capacitor > 4095
        {
            return Err(Error::Profile);
        }
        Ok(self)
    }
}
pub fn round_frequency(hz: u64) -> Result<u32, Error> {
    if !(u64::from(EXPERIMENTAL_MIN_HZ)..=u64::from(EXPERIMENTAL_MAX_HZ)).contains(&hz) {
        return Err(Error::Frequency);
    }
    Ok(((hz + 50) / 100 * 100) as u32)
}
pub const FIR2_DEFAULT: [i16; 40] = [
    28, 44, 54, 38, -11, -82, -146, -163, -107, 16, 157, 244, 211, 43, -201, -402, -435, -236, 142,
    531, 716, 545, 28, -629, -1092, -1057, -424, 602, 1551, 1876, 1229, -310, -2162, -3410, -3145,
    -870, 3214, 8185, 12674, 15332,
];
pub fn validate_fir(coefficients: &[i16]) -> Result<(), Error> {
    if coefficients.len() != 40
        || coefficients
            .iter()
            .enumerate()
            .any(|(i, &c)| i32::from(c).abs() >= if i < 20 { 4096 } else { 16384 })
    {
        return Err(Error::Profile);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tuning_and_routes() {
        assert_eq!(round_frequency(70_049), Ok(70_000));
        assert_eq!(round_frequency(70_050), Ok(70_100));
        assert!(round_frequency(u64::MAX).is_err());
        for (hz, input) in [
            (70_000, RfInput::Lf),
            (1_999_900, RfInput::Lf),
            (2_000_000, RfInput::Hf),
            (39_999_900, RfInput::Hf),
            (40_000_000, RfInput::Vhf),
            (130_000_000, RfInput::Vhf),
            (170_000_000, RfInput::Vhf),
        ] {
            assert_eq!(RfInput::Auto.actual(hz), input);
        }
        let c = Config {
            frequency: 474_200,
            rate: 24_000,
            bandwidth: 10_000,
            flags: 3,
        };
        assert_eq!(RfInput::Hf.routing_carrier(c), [0, 0x4e, 0x20]);
        assert_eq!(RfInput::Auto.routing_carrier(c), c.carrier());
        assert!(
            Controls {
                rf_auto: false,
                ..Controls::default()
            }
            .validate(c)
            .is_ok()
        );
    }
    #[test]
    fn coefficient_limits() {
        assert!(validate_fir(&FIR2_DEFAULT).is_ok());
        let mut c = FIR2_DEFAULT;
        c[0] = -4096;
        assert!(validate_fir(&c).is_err());
        assert!(validate_fir(&c[..39]).is_err());
    }
}
