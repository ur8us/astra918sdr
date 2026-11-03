//! RP2350 pin override policy for selectable polarity copies of GPOUT0.
pub const NORMAL_PIN: usize = 21;
pub const INVERTED_PIN: usize = 13;
pub const OUTPUT_MASK: u32 = (1 << NORMAL_PIN) | (1 << INVERTED_PIN);
#[cfg(target_arch = "arm")]
use embassy_rp::pac::io::{
    regs::GpioCtrl,
    vals::{Oeover, Outover},
};
#[cfg(target_arch = "arm")]
use embassy_rp::pac::pads::{regs::GpioCtrl as Pad, vals::Drive};
#[cfg(all(test, not(target_arch = "arm")))]
use rp_pac::io::{
    regs::GpioCtrl,
    vals::{Oeover, Outover},
};
#[cfg(all(test, not(target_arch = "arm")))]
use rp_pac::pads::{regs::GpioCtrl as Pad, vals::Drive};

#[cfg(any(target_arch = "arm", test))]
pub fn drive_pad(original: u32, ma: u8) -> Result<u32, crate::Error> {
    let code = match ma {
        2 => 0,
        4 => 1,
        8 => 2,
        12 => 3,
        _ => return Err(crate::Error::Protocol),
    };
    let mut pad = Pad(original);
    pad.set_drive(Drive::from_bits(code));
    Ok(pad.0)
}

#[cfg(any(target_arch = "arm", test))]
pub fn configure_pad(original: u32, ma: u8, fast: bool) -> Result<u32, crate::Error> {
    let mut pad = Pad(drive_pad(original, ma)?);
    pad.set_slewfast(fast);
    Ok(pad.0)
}

#[cfg(any(target_arch = "arm", test))]
pub fn tx_control(original: u32, inverted: bool) -> u32 {
    let mut control = GpioCtrl(original);
    control.set_funcsel(9);
    control.set_outover(if inverted {
        Outover::INVERT
    } else {
        Outover::NORMAL
    });
    control.set_oeover(Oeover::NORMAL);
    control.0
}

#[cfg(any(target_arch = "arm", test))]
pub fn rx_control(original: u32) -> u32 {
    let mut control = GpioCtrl(original);
    control.set_funcsel(5);
    control.set_outover(Outover::NORMAL);
    control.set_oeover(Oeover::NORMAL);
    control.0
}

/// A live polarity edit must not drive an idle/inactive GP13 high.
#[cfg(any(target_arch = "arm", test))]
pub fn polarity_control(original: u32, differential: bool, clock_enabled: bool) -> u32 {
    let mut control = GpioCtrl(original);
    if clock_enabled && control.funcsel() == 9 {
        control.set_outover(if differential {
            Outover::INVERT
        } else {
            Outover::NORMAL
        });
    }
    control.0
}

pub const fn output_pad(original: u32) -> u32 {
    original & !((1 << 2) | (1 << 3) | (1 << 7) | (1 << 8))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_polarity_only_changes_outover_and_never_activates_idle_output() {
        for original in [0, u32::MAX, 5, 9, 0xf009, 0x0800_0009] {
            for differential in [false, true] {
                for enabled in [false, true] {
                    let result = polarity_control(original, differential, enabled);
                    if enabled && GpioCtrl(original).funcsel() == 9 {
                        assert_eq!(
                            GpioCtrl(result).outover(),
                            if differential {
                                Outover::INVERT
                            } else {
                                Outover::NORMAL
                            }
                        );
                        assert_eq!(result & !(3 << 12), original & !(3 << 12));
                    } else {
                        assert_eq!(result, original);
                    }
                }
            }
        }
        for differential in [false, true] {
            let tx13 = tx_control(0, differential);
            let tx21 = tx_control(0, false);
            assert_eq!(GpioCtrl(tx13).outover() == Outover::INVERT, differential);
            assert_eq!(GpioCtrl(tx21).outover(), Outover::NORMAL);
            assert_eq!(rx_control(tx13), 5);
            assert_eq!(rx_control(tx21), 5);
        }
    }
    #[test]
    fn pad_config_preserves_polarity_and_slew_through_tx_rx() {
        for original in [0, u32::MAX, 0x116, 0x43] {
            for ma in [2, 4, 8, 12] {
                for fast in [false, true] {
                    let pad = configure_pad(original, ma, fast).unwrap();
                    assert_eq!(Pad(pad).slewfast(), fast);
                    assert_eq!(pad & !0x31, original & !0x31);
                    assert_eq!(output_pad(pad) & 0x31, pad & 0x31);
                    assert_eq!(configure_pad(pad, ma, fast).unwrap(), pad);
                }
            }
        }
    }
    #[test]
    fn drive_strength_changes_only_the_two_drive_bits() {
        for original in [0, u32::MAX, 0x116, 0x43] {
            for (ma, code) in [(2, 0), (4, 1), (8, 2), (12, 3)] {
                let pad = drive_pad(original, ma).unwrap();
                assert_eq!(Pad(pad).drive(), Drive::from_bits(code));
                assert_eq!(pad & !0x30, original & !0x30);
                assert_eq!(output_pad(pad) & 0x30, pad & 0x30);
            }
        }
        for ma in [0, 1, 3, 5, 16, 255] {
            assert!(drive_pad(0, ma).is_err());
        }
    }
    #[test]
    fn tx_polarity_is_per_pin_and_rx_clears_both_overrides() {
        // RP2350 OUTOVER=13:12, OEOVER=15:14, unlike RP2040's 9:8/13:12.
        const CONTROL_MASK: u32 = 0x1f | (3 << 12) | (3 << 14);
        assert_eq!(tx_control(0, false), 0x0009);
        assert_eq!(tx_control(0, true), 0x1009);
        for original in [0, u32::MAX, 5 | (3 << 12) | (3 << 14)] {
            for inverted in [false, true] {
                let tx = tx_control(original, inverted);
                assert_eq!(tx & 0x1f, 9);
                let decoded = GpioCtrl(tx);
                assert_eq!(
                    decoded.outover(),
                    if inverted {
                        Outover::INVERT
                    } else {
                        Outover::NORMAL
                    }
                );
                assert_eq!(decoded.oeover(), Oeover::NORMAL);
                assert_eq!(tx & !CONTROL_MASK, original & !CONTROL_MASK);
                let rx = rx_control(tx);
                assert_eq!(rx & CONTROL_MASK, 5);
                assert_eq!(GpioCtrl(rx).outover(), Outover::NORMAL);
                assert_eq!(GpioCtrl(rx).oeover(), Oeover::NORMAL);
            }
            let pad = output_pad(original);
            assert_eq!(pad & ((1 << 2) | (1 << 3) | (1 << 7) | (1 << 8)), 0);
            assert_eq!(pad & 1, original & 1); // Preserve selected slew rate.
            assert_eq!(pad & 0x30, original & 0x30); // Drive strength unchanged.
        }
        assert_eq!(OUTPUT_MASK, (1 << 13) | (1 << 21));
    }
}
