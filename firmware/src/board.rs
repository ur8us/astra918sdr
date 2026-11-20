//! Compile-time board wiring and capabilities.

pub const LIONEL: bool = cfg!(feature = "board-lionel");
pub const TX_SUPPORTED: bool = !LIONEL;
pub const FLASH_BYTES: usize = if LIONEL { 16 } else { 4 } * 1024 * 1024;
pub const I2C_SDA: usize = if LIONEL { 6 } else { 0 };
pub const I2C_SCL: usize = if LIONEL { 7 } else { 1 };
pub const RESETN: usize = if LIONEL { 43 } else { 2 };
pub const IQ_DATA: u8 = if LIONEL { 44 } else { 8 };
pub const IQ_FRAME: u8 = if LIONEL { 45 } else { 9 };
pub const IQ_CLOCK: u8 = if LIONEL { 46 } else { 10 };
pub const PIO_GPIO_BASE: u8 = if LIONEL { 16 } else { 0 };

#[cfg(all(target_arch = "arm", not(feature = "board-lionel")))]
pub use embassy_rp::peripherals::{
    I2C0 as ControlI2c, PIN_0 as ControlSda, PIN_1 as ControlScl, PIN_8 as IqData,
    PIN_9 as IqFrame, PIN_10 as IqClock,
};
#[cfg(all(target_arch = "arm", feature = "board-lionel"))]
pub use embassy_rp::peripherals::{
    I2C1 as ControlI2c, PIN_6 as ControlSda, PIN_7 as ControlScl, PIN_44 as IqData,
    PIN_45 as IqFrame, PIN_46 as IqClock,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::astra::{self as a, Receiver, ReferenceClock, Settings};

    #[test]
    fn selected_board_wiring_defaults_and_capabilities() {
        let defaults = Settings::default();
        let radio = Receiver::default();
        let status = radio.encode();
        if LIONEL {
            assert_eq!((I2C_SDA, I2C_SCL, RESETN), (6, 7, 43));
            assert_eq!(
                (IQ_DATA, IQ_FRAME, IQ_CLOCK, PIO_GPIO_BASE),
                (44, 45, 46, 16)
            );
            assert_eq!(defaults.reference, ReferenceClock::External);
            assert!(!defaults.tx_enabled);
            assert_eq!(status[127], a::FEATURE_RX_TUNING_ERROR);
            assert_eq!(status[120] & a::FEATURE_TX_RELAY_DELAY, 0);
            assert_ne!(&status[64..68], a::TX_IQ_STATUS_TAG);
            for command in [
                a::TX_CONFIG,
                a::TX_DRIVE,
                a::TX_SLEW,
                a::TX_OUTPUTS,
                a::TX_RELAY_DELAY,
                a::TX_IQ_MONITOR,
                a::TX_CARRIER,
                a::TX_MEASURE,
            ] {
                assert_eq!(radio.prepare(command, &[]), Err(a::Status::Unsupported));
            }
        } else {
            assert_eq!((I2C_SDA, I2C_SCL, RESETN), (0, 1, 2));
            assert_eq!((IQ_DATA, IQ_FRAME, IQ_CLOCK, PIO_GPIO_BASE), (8, 9, 10, 0));
            assert_eq!(defaults.reference, ReferenceClock::Internal);
            assert!(defaults.tx_enabled);
            assert_ne!(status[127] & 1, 0);
        }
        let record = crate::persistence::record(defaults, 7);
        assert_eq!(
            crate::persistence::decode(&record, &crate::persistence::commit_page(&record)),
            Some((defaults, 7))
        );
        let mut changed = radio;
        changed.settings.reference = ReferenceClock::Internal;
        let (settings, _) = changed.prepare(a::FACTORY_RESET, &[]).unwrap();
        assert_eq!(settings, defaults);
    }
}
