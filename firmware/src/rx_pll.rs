//! Manual CMX918 receive PLL coefficients (UM918/2.0 $2A–$34).
//!
//! The 38.4 MHz reference uses R=1. LO is RF minus IF for `lo_below`,
//! otherwise RF plus IF; VCO = 4 * LO * L. Fc's 100 Hz metadata must never
//! enter this calculation. Only the final 24-bit fractional ratio is rounded.
//!
//! Up to 130 MHz, choose ceil(1.5 GHz / (4 * LO)), with L at least 4 and at
//! most 2047. This fits the nominal 1.5–1.9 GHz VCO window whenever possible
//! under that minimum-L constraint. Near 130 MHz, L=4 necessarily exceeds
//! 1.9 GHz; retain this experimental coverage without claiming lock margin.
//! Preserve the existing experimental overrides: L=3 above 130 MHz through
//! 170 MHz, L=2 above 170 MHz. These may also exceed the nominal VCO window;
//! successful planning does not imply the chip will lock.
//!
//! A positive LO below ceil(1.5 GHz / (4 * 2047)) cannot reach the minimum
//! VCO frequency and returns `Error::Frequency`, rather than silently clamping
//! L. In particular, 70 kHz RF with a high-side 96 kHz IF is not representable;
//! a high-side 120 kHz IF is. An unsupported IF returns `Error::Profile`.

use crate::{EXPERIMENTAL_MAX_HZ, EXPERIMENTAL_MIN_HZ, Error};

const REFERENCE_HZ: u64 = 38_400_000;
const FRACTION_SCALE: u64 = 1 << 24;
const VCO_MIN_HZ: u64 = 1_500_000_000;
const L_MAX: u64 = 2047;

/// Nominal synthesized RF minus requested RF, rounded to signed microhertz.
/// IF translation cancels; audio NCO error is handled separately by the caller.
/// Reference-clock error is unknown.
pub fn error_micro_hz(frequency: u32, lo_below: bool, if_hz: u32) -> Result<i64, Error> {
    let p = plan(frequency, lo_below, if_hz)?;
    let lo = if lo_below {
        frequency - if_hz
    } else {
        frequency + if_hz
    };
    let ratio = (u64::from(p.n) << 24) | u64::from(p.fraction);
    let residual = (ratio * REFERENCE_HZ) as i64
        - (4 * u64::from(lo) * u64::from(p.l) * FRACTION_SCALE) as i64;
    let denominator = (4 * u64::from(p.l) * FRACTION_SCALE) as i64;
    let numerator = residual * 1_000_000;
    Ok(numerator.signum() * ((numerator.abs() + denominator / 2) / denominator))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub n: u16,
    pub fraction: u32,
    pub l: u16,
}

/// Validate exact-Hz RF and the 96/120 kHz IF selection before any chip I/O.
/// Sample-rate/profile validation remains the responsibility of `Config`.
pub fn plan(frequency: u32, lo_below: bool, if_hz: u32) -> Result<Plan, Error> {
    if !(EXPERIMENTAL_MIN_HZ..=EXPERIMENTAL_MAX_HZ).contains(&frequency) {
        return Err(Error::Frequency);
    }
    if !matches!(if_hz, 96_000 | 120_000) {
        return Err(Error::Profile);
    }
    let lo = if lo_below {
        u64::from(frequency.checked_sub(if_hz).ok_or(Error::Frequency)?)
    } else {
        u64::from(frequency) + u64::from(if_hz)
    };
    if lo == 0 {
        return Err(Error::Frequency);
    }
    let l = if frequency > 170_000_000 {
        2
    } else if frequency > 130_000_000 {
        3
    } else {
        VCO_MIN_HZ.div_ceil(4 * lo).max(4)
    };
    if l > L_MAX {
        return Err(Error::Frequency);
    }
    // Bounds above keep the VCO * 2^24 product safely within u64. R=1.
    let scaled = (4 * lo * l * FRACTION_SCALE + REFERENCE_HZ / 2) / REFERENCE_HZ;
    Ok(Plan {
        n: (scaled >> 24) as u16,
        fraction: (scaled & 0x00ff_ffff) as u32,
        l: l as u16,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_hertz_offsets_and_both_if_signs_have_known_coefficients() {
        let expected = Plan {
            n: 40,
            fraction: 7,
            l: 4,
        };
        for (frequency, lo_below, if_hz) in [
            (96_096_001, true, 96_000),
            (95_904_001, false, 96_000),
            (96_120_001, true, 120_000),
            (95_880_001, false, 120_000),
        ] {
            assert_eq!(plan(frequency, lo_below, if_hz), Ok(expected));
            let integer = plan(frequency - 1, lo_below, if_hz).unwrap();
            assert_eq!((integer.n, integer.fraction, integer.l), (40, 0, 4));
            let below = plan(frequency - 2, lo_below, if_hz).unwrap();
            assert_eq!((below.n, below.fraction, below.l), (39, 0xff_fff9, 4));
        }
    }

    #[test]
    fn normal_plans_choose_smallest_legal_l_and_round_only_the_ratio() {
        for frequency in [
            310_001,
            474_237,
            1_999_999,
            2_000_001,
            7_074_037,
            14_200_037,
            40_000_003,
            93_650_001,
            108_000_000,
            118_000_099,
        ] {
            for lo_below in [false, true] {
                for if_hz in [96_000, 120_000] {
                    let p = plan(frequency, lo_below, if_hz).unwrap();
                    let lo = if lo_below {
                        frequency - if_hz
                    } else {
                        frequency + if_hz
                    };
                    let vco = 4 * u64::from(lo) * u64::from(p.l);
                    assert!((4..=2047).contains(&p.l));
                    assert!((1_500_000_000..=1_900_000_000).contains(&vco));
                    if p.l > 4 {
                        assert!(4 * u64::from(lo) * u64::from(p.l - 1) < VCO_MIN_HZ);
                    }
                    let reconstructed =
                        ((u64::from(p.n) << 24) | u64::from(p.fraction)) * REFERENCE_HZ;
                    // At most half a fractional LSB, without floating-point tolerance.
                    assert!(reconstructed.abs_diff(vco * FRACTION_SCALE) <= REFERENCE_HZ / 2);
                }
            }
        }
    }

    #[test]
    fn low_lo_limits_reject_impossible_l_instead_of_clamping() {
        let minimum_lo = VCO_MIN_HZ.div_ceil(4 * L_MAX) as u32;
        for if_hz in [96_000, 120_000] {
            assert_eq!(plan(if_hz, true, if_hz), Err(Error::Frequency));
            assert_eq!(plan(if_hz - 1, true, if_hz), Err(Error::Frequency));
            let rf = minimum_lo + if_hz;
            assert_eq!(plan(rf, true, if_hz).unwrap().l, 2047);
            assert_eq!(plan(rf - 1, true, if_hz), Err(Error::Frequency));
            let rf = minimum_lo - if_hz;
            if rf >= EXPERIMENTAL_MIN_HZ {
                assert_eq!(plan(rf, false, if_hz).unwrap().l, 2047);
                assert_eq!(plan(rf - 1, false, if_hz), Err(Error::Frequency));
            }
        }
        assert_eq!(plan(70_000, false, 96_000), Err(Error::Frequency));
        assert!(plan(70_000, false, 120_000).is_ok());
    }

    #[test]
    fn experimental_divider_boundaries_and_out_of_spec_vco_are_preserved() {
        for (frequency, expected_l) in [
            (130_000_000, 4),
            (130_000_001, 3),
            (170_000_000, 3),
            (170_000_001, 2),
            (260_000_000, 2),
        ] {
            for lo_below in [false, true] {
                for if_hz in [96_000, 120_000] {
                    assert_eq!(plan(frequency, lo_below, if_hz).unwrap().l, expected_l);
                }
            }
        }
    }

    #[test]
    fn invalid_rf_and_if_are_rejected_without_quantizing_rf() {
        for frequency in [0, 69_999, 260_000_001, u32::MAX] {
            assert_eq!(plan(frequency, false, 96_000), Err(Error::Frequency));
        }
        for if_hz in [0, 95_999, 96_001, 119_999, 120_001, u32::MAX] {
            assert_eq!(plan(14_200_037, false, if_hz), Err(Error::Profile));
        }
        assert_ne!(
            plan(14_200_037, true, 96_000),
            plan(14_200_000, true, 96_000)
        );
    }
}
