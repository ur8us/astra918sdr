//! Experimental wide FIR2 responses; derivation and clock assumptions: docs/WIDE-FIR.md.
use crate::{Config, USB_DECIMATED_RATE};
mod tables {
    include!("wide_fir_tables.rs");
}

pub fn coefficients(config: Config) -> &'static [i16; 40] {
    if config.rate == USB_DECIMATED_RATE {
        &tables::QUARTER // Reject >=60 kHz before CMX240-to-USB120 decimation.
    } else if config.rate == config.bandwidth * 24 / 10 {
        &tables::HALF // FIR2 clock is twice the outgoing sample rate.
    } else {
        &tables::FULL // FIR2 clock equals the outgoing sample rate.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tables_obey_hardware_limits_and_select_before_decimation() {
        for (bandwidth, rate) in crate::PROFILES.into_iter().chain([(100_000, 120_000)]) {
            let config = Config {
                frequency: 14_200_000,
                rate,
                bandwidth,
                flags: 0x18b,
            };
            assert!(config.validate().is_ok());
            let half = coefficients(config);
            assert!(crate::controls::validate_fir(half).is_ok());
            assert!(half.iter().map(|&x| i32::from(x)).sum::<i32>() > 0);
            if rate == 120_000 {
                assert_eq!(half, &tables::QUARTER);
            } else if rate == bandwidth * 24 / 10 {
                assert_eq!(half, &tables::HALF);
            } else {
                assert_eq!(half, &tables::FULL);
            }
        }
    }
}
