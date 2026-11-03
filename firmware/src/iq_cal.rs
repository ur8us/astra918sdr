//! Pure policy and frequency arithmetic for CMX918 pilot I/Q calibration.

use crate::astra::ReferenceClock;
use crate::controls::RfInput;

pub const RETUNE_HZ: u32 = 500_000;
const REF_HZ: u64 = 38_400_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub center: u32,
    pub input: RfInput,
    pub reference: ReferenceClock,
    pub lo_below: bool,
    pub if_hz: u32,
}
impl Context {
    fn same_path(self, other: Self) -> bool {
        self.input == other.input
            && self.reference == other.reference
            && self.lo_below == other.lo_below
            && self.if_hz == other.if_hz
    }
    fn distant(self, other: Self) -> bool {
        !self.same_path(other) || self.center.abs_diff(other.center) >= RETUNE_HZ
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Correction {
    pub i: i16,
    pub q: i16,
}

#[derive(Default)]
pub struct State {
    pub success: Option<Context>,
    pub failed: Option<Context>,
    pub correction: Option<Correction>,
    /// 0 never attempted, 1 successful, 2 pilot could not complete.
    pub status: u8,
}
impl State {
    pub fn needed(&self, context: Context, enabled: bool, force: bool) -> bool {
        if !enabled {
            return false;
        }
        if !force && self.failed.is_some_and(|failed| !context.distant(failed)) {
            return false;
        }
        force || self.success.is_none_or(|success| context.distant(success))
    }
    pub fn compatible_correction(&self, context: Context) -> Option<Correction> {
        self.success
            .filter(|success| success.same_path(context))
            .and(self.correction)
    }
    pub fn succeed(&mut self, context: Context, correction: Correction) {
        self.success = Some(context);
        self.failed = None;
        self.correction = Some(correction);
        self.status = 1;
    }
    pub fn fail(&mut self, context: Context) {
        self.failed = Some(context);
        self.success = None;
        self.correction = None;
        self.status = 2;
    }
    pub fn disable(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pilot {
    pub n: u16,
    pub p: u16,
    pub calibration_center: u32,
}
impl Pilot {
    /// Use the discrete N/P pairs in CMX918 DS §5.9.2 Table 11. The pilot
    /// cannot follow an arbitrary dial frequency; retune briefly so that its
    /// fixed tone lands at the image frequency, then restore the requested RF.
    pub fn for_context(context: Context) -> Option<Self> {
        if !(70_000..=130_000_000).contains(&context.center) {
            return None;
        }
        let (n, p) = if context.center > 102_400_000 {
            (17, 3)
        } else if context.center > 92_800_000 {
            (15, 3)
        } else if context.center > 83_200_000 {
            (14, 3)
        } else if context.center > 70_400_000 {
            (12, 3)
        } else if context.center > 60_800_000 {
            (17, 5)
        } else if context.center > 51_840_000 {
            (12, 4)
        } else if context.center >= 40_000_000 {
            // AUTO input becomes VHF at 40 MHz; keep the temporary tune there.
            (12, 5)
        } else if context.center > 25_600_000 {
            (15, 9)
        } else if context.center > 14_400_000 {
            (14, 14)
        } else if context.center > 6_400_000 {
            (14, 28)
        } else if context.center >= 2_000_000 {
            (15, 75)
        } else if context.center > 1_330_000 {
            (15, 160)
        } else if context.center > 400_000 {
            (13, 260)
        } else if context.center > 256_000 {
            (15, 576)
        } else {
            (13, 640)
        };
        let tone = (REF_HZ * n) / (2 * p);
        let fc = if context.lo_below {
            tone.checked_add(u64::from(context.if_hz) * 2)?
        } else {
            tone.checked_sub(u64::from(context.if_hz) * 2)?
        };
        if !(70_000..=130_000_000).contains(&fc) || (fc < 2_000_000) != (context.center < 2_000_000)
        {
            return None;
        }
        Some(Self {
            n: n as u16,
            p: p as u16,
            calibration_center: fc as u32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(center: u32) -> Context {
        Context {
            center,
            input: RfInput::Hf,
            reference: ReferenceClock::Internal,
            lo_below: true,
            if_hz: 96_000,
        }
    }
    #[test]
    fn accumulated_retune_and_path_changes_trigger_from_last_success() {
        let mut state = State::default();
        let first = context(14_200_000);
        assert!(state.needed(first, true, false));
        state.succeed(first, Correction { i: 12, q: -8 });
        assert!(!state.needed(context(14_400_000), true, false));
        assert!(!state.needed(context(14_600_000), true, false));
        assert!(state.needed(context(14_700_000), true, false));
        assert!(state.needed(
            Context {
                if_hz: 120_000,
                ..first
            },
            true,
            false
        ));
        assert!(!state.needed(context(14_700_000), false, false));
        state.fail(context(14_700_000));
        assert!(!state.needed(context(14_710_000), true, false));
        assert!(state.needed(context(15_200_000), true, false));
        assert!(state.needed(context(14_710_000), true, true));
    }
    #[test]
    fn pilot_tracks_image_for_both_signs_and_ifs() {
        for (center, lo_below, if_hz) in [
            (14_200_000, true, 96_000),
            (7_074_000, false, 120_000),
            (136_000, false, 96_000),
        ] {
            let c = Context {
                lo_below,
                if_hz,
                ..context(center)
            };
            let pilot = Pilot::for_context(c).unwrap();
            assert!(pilot.calibration_center.abs_diff(center) <= 10_000_000);
            assert!((9..=19).contains(&pilot.n));
            assert!((3..=2047).contains(&pilot.p));
        }
        let high = Pilot::for_context(context(130_000_000)).unwrap();
        assert_eq!(
            (high.n, high.p, high.calibration_center),
            (17, 3, 108_992_000)
        );
        assert_eq!(
            Pilot::for_context(Context {
                lo_below: false,
                ..context(100_000)
            })
            .unwrap()
            .calibration_center,
            198_000
        );
        assert!(Pilot::for_context(context(69_900)).is_none());
    }
    #[test]
    fn documented_hf_pilot_uses_a_large_temporary_retune() {
        let pilot = Pilot::for_context(context(10_136_000)).unwrap();
        assert_eq!((pilot.n, pilot.p), (14, 28));
        assert_eq!(pilot.calibration_center, 9_792_000);
        let above = Pilot::for_context(Context {
            lo_below: false,
            ..context(10_136_000)
        })
        .unwrap();
        assert_eq!(above.calibration_center, 9_408_000);
    }
    #[test]
    fn datasheet_pairs_preserve_auto_input_family_at_band_edges() {
        for (center, n, p) in [
            (110_000_000, 17, 3),
            (100_000_000, 15, 3),
            (90_000_000, 14, 3),
            (75_000_000, 12, 3),
            (65_000_000, 17, 5),
            (55_000_000, 12, 4),
            (40_000_000, 12, 5),
            (30_000_000, 15, 9),
            (20_000_000, 14, 14),
            (10_000_000, 14, 28),
            (2_000_000, 15, 75),
            (1_600_000, 15, 160),
            (1_000_000, 13, 260),
            (300_000, 15, 576),
            (100_000, 13, 640),
        ] {
            let context = Context {
                lo_below: center >= 2_000_000,
                ..context(center)
            };
            let pilot = Pilot::for_context(context).unwrap();
            assert_eq!((pilot.n, pilot.p), (n, p), "center {center}");
            let tone = (REF_HZ * u64::from(n)) / (2 * u64::from(p));
            let expected = if context.lo_below {
                tone + u64::from(context.if_hz) * 2
            } else {
                tone - u64::from(context.if_hz) * 2
            };
            assert_eq!(u64::from(pilot.calibration_center), expected);
            assert_eq!(
                RfInput::Auto.actual(pilot.calibration_center),
                RfInput::Auto.actual(center)
            );
        }
    }
}
