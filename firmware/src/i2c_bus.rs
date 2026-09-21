//! Cancellation-aware I2C0 ownership. Embassy RP 0.10.0 I2c contains only
//! PhantomData and has no cancellation cleanup. Reset its peripheral before
//! reusing it after an unfinished transaction; retain the original HAL owner.

use astra918_firmware::recovery::{self, ClearPins, Recover};
use embassy_embedded_hal::SetConfig;
use embassy_rp::{
    Peri, i2c, pac,
    peripherals::{I2C0, PIN_0, PIN_1},
};
use embassy_time::{Duration, Timer, with_timeout};
use embedded_hal::i2c::{ErrorKind, ErrorType, Operation};
use embedded_hal_async::i2c::I2c;
use pac::io::vals::{Oeover, Outover};

pub struct Bus {
    inner: i2c::I2c<'static, I2C0, i2c::Async>,
    dirty: bool,
}

impl Bus {
    pub fn new(
        peri: Peri<'static, I2C0>,
        scl: Peri<'static, PIN_1>,
        sda: Peri<'static, PIN_0>,
    ) -> Self {
        Self {
            inner: i2c::I2c::new_async(peri, scl, sda, crate::Irqs, config()),
            // Also clear a slave left mid-byte by an MCU-only reset/reflash.
            dirty: true,
        }
    }
    async fn prepare(&mut self) -> Result<(), ErrorKind> {
        if self.dirty {
            self.recover().await?;
        }
        // Set BEFORE awaiting. Dropping this future at any outer timeout leaves
        // it dirty; only a successful complete transaction clears the flag.
        self.dirty = true;
        Ok(())
    }
}

fn config() -> i2c::Config {
    let mut config = i2c::Config::default();
    config.frequency = 400_000;
    config
}

// Constructed only while Bus holds &mut self, with no HAL transaction alive.
// Overrides retain GPIO0/1's I2C function/pullups and only drive low or release.
// Drop removes overrides even if an outer control deadline cancels recovery.
struct Pins;
impl Pins {
    fn low(pin: usize, low: bool) {
        pac::IO_BANK0.gpio(pin).ctrl().modify(|w| {
            w.set_outover(Outover::LOW);
            w.set_oeover(if low { Oeover::ENABLE } else { Oeover::DISABLE });
        });
    }
}
impl ClearPins for Pins {
    fn scl_low(&mut self, low: bool) {
        Self::low(1, low);
    }
    fn sda_low(&mut self, low: bool) {
        Self::low(0, low);
    }
    fn scl_high(&self) -> bool {
        pac::IO_BANK0.gpio(1).status().read().infrompad()
    }
    fn sda_high(&self) -> bool {
        pac::IO_BANK0.gpio(0).status().read().infrompad()
    }
}
impl Drop for Pins {
    fn drop(&mut self) {
        for pin in 0..2 {
            pac::IO_BANK0.gpio(pin).ctrl().modify(|w| {
                w.set_outover(Outover::NORMAL);
                w.set_oeover(Oeover::NORMAL);
            });
        }
    }
}

impl ErrorType for Bus {
    type Error = ErrorKind;
}
impl Recover for Bus {
    async fn recover(&mut self) -> Result<(), ErrorKind> {
        self.dirty = true;
        pac::I2C0.ic_intr_mask().write(|_| {});
        // A hardware reset also discards FIFOs/abort state left by cancellation.
        pac::RESETS.reset().modify(|w| w.set_i2c0(true));
        pac::RESETS.reset().modify(|w| w.set_i2c0(false));
        let result = with_timeout(Duration::from_millis(20), async {
            while !pac::RESETS.reset_done().read().i2c0() {
                Timer::after_micros(1).await;
            }
            let mut pins = Pins;
            defmt::info!(
                "I2C recovery: SDA={} SCL={}",
                pins.sda_high(),
                pins.scl_high()
            );
            recovery::clear(&mut pins, &mut embassy_time::Delay).await
        })
        .await;
        match result {
            Ok(Ok(pulses)) => {
                // Same timing setup used by I2c::new_inner in the pinned HAL;
                // its remaining IC_CON configuration uses hardware defaults.
                self.inner
                    .set_config(&config())
                    .map_err(|_| ErrorKind::Bus)?;
                self.dirty = false;
                defmt::info!("I2C recovery complete: clocks={}", pulses);
                Ok(())
            }
            error => {
                defmt::warn!("I2C recovery failed: {}", defmt::Debug2Format(&error));
                Err(ErrorKind::Bus)
            }
        }
    }
}

macro_rules! transfer {
    ($self:ident, $operation:expr) => {{
        $self.prepare().await?;
        let result = match with_timeout(Duration::from_millis(10), $operation).await {
            Ok(result) => result.map_err(|e| embedded_hal::i2c::Error::kind(&e)),
            Err(_) => {
                defmt::warn!("I2C transaction exceeded 10 ms; recovery required");
                Err(ErrorKind::Bus)
            }
        };
        $self.dirty = result.is_err();
        result
    }};
}
impl I2c for Bus {
    async fn read(&mut self, address: u8, read: &mut [u8]) -> Result<(), ErrorKind> {
        transfer!(self, self.inner.read(address, read))
    }
    async fn write(&mut self, address: u8, write: &[u8]) -> Result<(), ErrorKind> {
        transfer!(self, self.inner.write(address, write))
    }
    async fn write_read(
        &mut self,
        address: u8,
        write: &[u8],
        read: &mut [u8],
    ) -> Result<(), ErrorKind> {
        transfer!(self, self.inner.write_read(address, write, read))
    }
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), ErrorKind> {
        transfer!(self, self.inner.transaction(address, operations))
    }
}
