//! Bounded I2C bus clear: NXP UM10204 rev. 7.0, section 3.1.16, p.19.

use embedded_hal_async::{delay::DelayNs, i2c::I2c};

/// Recover an exclusively owned bus after a failed/cancelled transaction.
#[allow(async_fn_in_trait)]
pub trait Recover: I2c {
    async fn recover(&mut self) -> Result<(), Self::Error>;
}

/// Open-drain only: `false` releases the line, never drives it high.
pub trait ClearPins {
    fn scl_low(&mut self, low: bool);
    fn sda_low(&mut self, low: bool);
    fn scl_high(&self) -> bool;
    fn sda_high(&self) -> bool;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClearError {
    ClockHeldLow,
    DataHeldLow,
}

async fn clock_high(pins: &impl ClearPins, delay: &mut impl DelayNs) -> Result<(), ClearError> {
    for _ in 0..100 {
        if pins.scl_high() {
            return Ok(());
        }
        delay.delay_us(10).await;
    }
    Err(ClearError::ClockHeldLow)
}

/// Up to nine clocks, then STOP. The caller releases pins on error/cancellation.
pub async fn clear(pins: &mut impl ClearPins, delay: &mut impl DelayNs) -> Result<u8, ClearError> {
    pins.sda_low(false);
    pins.scl_low(false);
    clock_high(pins, delay).await?;
    let mut pulses = 0;
    while !pins.sda_high() && pulses < 9 {
        pins.scl_low(true);
        delay.delay_us(10).await;
        pins.scl_low(false);
        clock_high(pins, delay).await?;
        delay.delay_us(10).await;
        pulses += 1;
    }
    if !pins.sda_high() {
        return Err(ClearError::DataHeldLow);
    }
    // Bring SDA low while SCL is low, then release SCL and finally SDA.
    pins.scl_low(true);
    pins.sda_low(true);
    delay.delay_us(10).await;
    pins.scl_low(false);
    clock_high(pins, delay).await?;
    delay.delay_us(10).await;
    pins.sda_low(false);
    delay.delay_us(10).await;
    if !pins.sda_high() {
        return Err(ClearError::DataHeldLow);
    }
    Ok(pulses)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Pins {
        clock_low: bool,
        data_low: bool,
        stuck_clock: bool,
        release_after: u8,
        edges: u8,
        stop: bool,
    }
    impl Pins {
        fn new(release_after: u8) -> Self {
            Self {
                clock_low: false,
                data_low: false,
                stuck_clock: false,
                release_after,
                edges: 0,
                stop: false,
            }
        }
    }
    impl ClearPins for Pins {
        fn scl_low(&mut self, low: bool) {
            if self.clock_low && !low {
                self.edges += 1;
            }
            self.clock_low = low;
        }
        fn sda_low(&mut self, low: bool) {
            if self.data_low && !low && self.scl_high() {
                self.stop = true;
            }
            self.data_low = low;
        }
        fn scl_high(&self) -> bool {
            !self.clock_low && !self.stuck_clock
        }
        fn sda_high(&self) -> bool {
            !self.data_low && self.edges >= self.release_after
        }
    }
    #[derive(Default)]
    struct Delay(u64);
    impl DelayNs for Delay {
        async fn delay_ns(&mut self, ns: u32) {
            self.0 += ns as u64;
        }
    }
    #[test]
    fn idle_and_stuck_data_recover_with_stop() {
        futures::executor::block_on(async {
            for n in 0..=9 {
                let mut pins = Pins::new(n);
                assert_eq!(clear(&mut pins, &mut Delay::default()).await, Ok(n));
                assert!(pins.stop && pins.scl_high() && pins.sda_high());
            }
        });
    }
    #[test]
    fn permanent_data_fault_has_only_nine_clocks() {
        futures::executor::block_on(async {
            let mut pins = Pins::new(10);
            assert_eq!(
                clear(&mut pins, &mut Delay::default()).await,
                Err(ClearError::DataHeldLow)
            );
            assert_eq!(pins.edges, 9);
            assert!(!pins.stop);
        });
    }
    #[test]
    fn permanent_clock_fault_is_bounded_and_does_not_drive_data() {
        futures::executor::block_on(async {
            let mut pins = Pins::new(0);
            pins.stuck_clock = true;
            let mut delay = Delay::default();
            assert_eq!(
                clear(&mut pins, &mut delay).await,
                Err(ClearError::ClockHeldLow)
            );
            assert_eq!(delay.0, 1_000_000);
            assert_eq!(pins.edges, 0);
            assert!(!pins.data_low);
        });
    }
}
