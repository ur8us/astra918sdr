//! PLL plan for the experimental CMX918 LDIV to GP20/GP21 path.
use crate::Error;

const LDIV_MIN_MILLIHZ: u64 = 240_000_000_000;
const LDIV_MAX_MILLIHZ: u64 = 316_000_000_000;
const PREFERRED_MIN_MILLIHZ: u64 = 250_000_000_000;
const PREFERRED_MAX_MILLIHZ: u64 = 300_000_000_000;

// Range takes priority over parity. Within the extended fallback, prefer even.
fn preferred_dividers(hz: u64) -> Option<(u64, u64)> {
    if hz == 0 {
        return None;
    }
    for (low, high, odd) in [
        (PREFERRED_MIN_MILLIHZ, PREFERRED_MAX_MILLIHZ, false),
        (PREFERRED_MIN_MILLIHZ, PREFERRED_MAX_MILLIHZ, true),
        (LDIV_MIN_MILLIHZ, LDIV_MAX_MILLIHZ, false),
        (LDIV_MIN_MILLIHZ, LDIV_MAX_MILLIHZ, true),
    ] {
        let mut first = low.div_ceil(hz).max(if odd { 1 } else { 2 });
        if first % 2 != u64::from(odd) {
            first += 1;
        }
        let last = (high / hz).min(65535);
        if first <= last {
            return Some((first, last));
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub divider: u16,
    pub l: u8,
    pub n: u16,
    pub fraction: u32,
    pub carrier: u32,
}

pub fn plan(hz: u64) -> Result<Plan, Error> {
    plan_millihz(hz.checked_mul(1000).ok_or(Error::Frequency)?)
}

pub fn plan_millihz(hz: u64) -> Result<Plan, Error> {
    let (first, last) = preferred_dividers(hz).ok_or(Error::Frequency)?;
    let mut best = None;
    let mut error = u64::MAX;
    for divider in (first..=last).step_by(2) {
        let Some(ldiv) = hz.checked_mul(divider) else {
            break;
        };
        if ldiv > LDIV_MAX_MILLIHZ {
            break;
        }
        if ldiv < LDIV_MIN_MILLIHZ {
            continue;
        }
        let l = if ldiv < 250_000_000_000 { 7u64 } else { 6u64 };
        let vco = ldiv * l;
        if !(1_500_000_000_000..=1_900_000_000_000).contains(&vco) {
            continue;
        }
        let distance = ldiv.abs_diff(280_000_000_000);
        if distance < error {
            let scaled = ((u128::from(vco) * (1 << 24) + 19_200_000_000) / 38_400_000_000) as u64;
            let carrier = ((ldiv / 4000).saturating_sub(96_000) + 50) / 100;
            best = Some(Plan {
                divider: divider as u16,
                l: l as u8,
                n: (scaled >> 24) as u16,
                fraction: (scaled & 0xffffff) as u32,
                carrier: carrier as u32,
            });
            error = distance;
        }
    }
    best.ok_or(Error::Frequency)
}

pub fn fraction(hz: u64, plan: Plan) -> Result<u32, Error> {
    fraction_millihz(hz.checked_mul(1000).ok_or(Error::Frequency)?, plan)
}

pub fn fraction_millihz(hz: u64, plan: Plan) -> Result<u32, Error> {
    let updated = retune_millihz(hz, plan)?;
    if updated.n != plan.n {
        return Err(Error::Frequency);
    }
    Ok(updated.fraction)
}

/// Hold both output divisors for the entire transmission. Only N/F may vary;
/// tier preference is evaluated at TX entry, never during a keyed frame.
pub fn retune_millihz(hz: u64, plan: Plan) -> Result<Plan, Error> {
    let ldiv = hz
        .checked_mul(u64::from(plan.divider))
        .ok_or(Error::Frequency)?;
    if !(LDIV_MIN_MILLIHZ..=LDIV_MAX_MILLIHZ).contains(&ldiv) {
        return Err(Error::Frequency);
    }
    let vco = ldiv
        .checked_mul(u64::from(plan.l))
        .ok_or(Error::Frequency)?;
    if !(1_500_000_000_000..=1_900_000_000_000).contains(&vco) {
        return Err(Error::Frequency);
    }
    let scaled = ((u128::from(vco) * (1 << 24) + 19_200_000_000) / 38_400_000_000) as u64;
    Ok(Plan {
        n: (scaled >> 24) as u16,
        fraction: (scaled & 0xffffff) as u32,
        ..plan
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_retune_rejects_ldiv_outside_limits_even_with_legal_vco() {
        for (start, target) in [(40_000_000, 39_500_000), (79_000_000, 79_125_000)] {
            let p = if start == 40_000_000 {
                // Legacy plan: /6 at LDIV=240 MHz, L=7, N=43.
                Plan {
                    divider: 6,
                    l: 7,
                    n: 43,
                    ..plan(start).unwrap()
                }
            } else {
                plan(start).unwrap()
            };
            let vco = target * u64::from(p.divider) * u64::from(p.l);
            // Both cases used to pass: the VCO and integer N stay valid.
            assert!((1_500_000_000..=1_900_000_000).contains(&vco));
            assert_eq!(vco / 38_400_000, u64::from(p.n));
            assert_eq!(fraction(target, p), Err(Error::Frequency));
        }
        let rebuilt = plan(39_500_000).unwrap();
        assert_eq!(rebuilt.divider, 7);
        assert!(fraction(39_500_000, rebuilt).is_ok());
        // No alternative integer divider fits this target; the TX loop must
        // reject the retune and recover RX rather than use 316.5 MHz LDIV.
        assert_eq!(plan(79_125_000), Err(Error::Frequency));
    }

    #[test]
    fn live_retune_checks_ldiv_limits_at_millihertz_resolution() {
        let low = plan(240_000_000).unwrap();
        assert!(fraction_millihz(240_000_000_000, low).is_ok());
        assert_eq!(
            fraction_millihz(240_000_000_000 - 1, low),
            Err(Error::Frequency)
        );
        let high = plan(79_000_000).unwrap();
        assert!(fraction_millihz(79_000_000_000, high).is_ok());
        assert_eq!(
            fraction_millihz(79_000_000_000 + 1, high),
            Err(Error::Frequency)
        );
    }

    #[test]
    fn chooses_prioritized_divider_and_legal_vco() {
        for hz in [
            1_800_000, 7_074_000, 14_074_000, 28_075_000, 50_313_000, 70_070_000,
        ] {
            let p = plan(hz).unwrap();
            let ldiv = hz * u64::from(p.divider);
            assert!((240_000_000..=316_000_000).contains(&ldiv));
            assert!((1_500_000_000..=1_900_000_000).contains(&(ldiv * u64::from(p.l))));
        }
        assert_eq!(plan(0), Err(Error::Frequency));
    }

    #[test]
    fn range_has_priority_then_even_divisors_then_distance() {
        for (hz, divider) in [
            (14_074_000, 20),
            (23_000_000, 12),
            (26_000_000, 10), // even /10 wins over odd /11 closer to 280 MHz
            (40_000_000, 7),
            (40_679_500, 7),
            (50_313_000, 5),
            (52_000_000, 5),
            (79_000_000, 4),
            (81_234_567, 3),
            (90_000_000, 3),
            (275_000_000, 1),
        ] {
            assert_eq!(plan(hz).unwrap().divider, divider, "RF {hz}");
        }
    }

    #[test]
    fn keyed_retune_keeps_dividers_across_tier_and_n_boundaries() {
        let wide_even = plan(79_000_000).unwrap(); // /4 at 316 MHz
        let changed_n = retune_millihz(75_000_000_000, wide_even).unwrap();
        assert_ne!(changed_n.n, wide_even.n);
        assert_eq!(
            (changed_n.l, changed_n.divider),
            (wide_even.l, wide_even.divider)
        );
        let narrow_even = plan(50_000_000).unwrap();
        assert_eq!(narrow_even.divider, 6);
        let held_even = retune_millihz(50_000_000_001, narrow_even).unwrap();
        assert_eq!(
            (held_even.l, held_even.divider),
            (narrow_even.l, narrow_even.divider)
        );
        let narrow_odd = plan_millihz(50_000_000_001).unwrap();
        assert_eq!(narrow_odd.divider, 5);
        assert!(fraction_millihz(50_000_000_001, narrow_odd).is_ok());
        // A newly preferable even divider must not replace the keyed odd one.
        let held_odd = retune_millihz(50_000_000_000, narrow_odd).unwrap();
        assert_eq!(
            (held_odd.l, held_odd.divider),
            (narrow_odd.l, narrow_odd.divider)
        );
    }

    #[test]
    fn resolves_ft4_and_ft8_shifts_below_one_hertz() {
        for hz in [
            1_838_000, 3_575_000, 7_047_500, 14_080_000, 28_180_000, 50_318_000, 70_100_000,
        ] {
            let plan = plan(hz).unwrap();
            for offset in [1, 6250, 20833, 43750, 145831] {
                let target = hz * 1000 + offset;
                let f = fraction_millihz(target, plan).unwrap();
                let actual = 38_400_000.0 * (f64::from(plan.n) + f64::from(f) / 16_777_216.0)
                    / f64::from(plan.l)
                    / f64::from(plan.divider);
                assert!((actual - target as f64 / 1000.0).abs() < 0.06);
            }
        }
    }
}
