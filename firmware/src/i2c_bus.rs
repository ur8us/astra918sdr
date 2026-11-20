//! Cancellation-aware board-selected I2C ownership. Embassy RP 0.10.0 I2c contains only
//! PhantomData and has no cancellation cleanup. Reset its peripheral before
//! reusing it after an unfinished transaction; retain the original HAL owner.

use astra918_firmware::{
    board::{self, ControlI2c, ControlScl, ControlSda},
    recovery::{self, ClearPins, Recover},
};
use embassy_embedded_hal::SetConfig;
use embassy_rp::{Peri, bind_interrupts, i2c, pac};
use embassy_time::{Duration, Timer, with_timeout};
use embedded_hal::i2c::{ErrorKind, ErrorType, Operation};
use embedded_hal_async::i2c::I2c;
use pac::io::vals::{Oeover, Outover};

#[cfg(feature = "board-lionel")]
bind_interrupts!(struct Irqs {
    I2C1_IRQ => i2c::InterruptHandler<ControlI2c>;
});
#[cfg(not(feature = "board-lionel"))]
bind_interrupts!(struct Irqs {
    I2C0_IRQ => i2c::InterruptHandler<ControlI2c>;
});

#[cfg(feature = "board-lionel")]
const I2C: pac::i2c::I2c = pac::I2C1;
#[cfg(not(feature = "board-lionel"))]
const I2C: pac::i2c::I2c = pac::I2C0;
#[cfg(feature = "board-lionel")]
const I2C_INDEX: usize = 1;
#[cfg(not(feature = "board-lionel"))]
const I2C_INDEX: usize = 0;

pub struct Bus {
    inner: i2c::I2c<'static, ControlI2c, i2c::Async>,
    dirty: bool,
}

impl Bus {
    pub fn new(
        peri: Peri<'static, ControlI2c>,
        scl: Peri<'static, ControlScl>,
        sda: Peri<'static, ControlSda>,
    ) -> Self {
        Self {
            inner: i2c::I2c::new_async(peri, scl, sda, Irqs, config()),
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
// Overrides retain the board's I2C function/pullups and only drive low or release.
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
        Self::low(board::I2C_SCL, low);
    }
    fn sda_low(&mut self, low: bool) {
        Self::low(board::I2C_SDA, low);
    }
    fn scl_high(&self) -> bool {
        pac::IO_BANK0
            .gpio(board::I2C_SCL)
            .status()
            .read()
            .infrompad()
    }
    fn sda_high(&self) -> bool {
        pac::IO_BANK0
            .gpio(board::I2C_SDA)
            .status()
            .read()
            .infrompad()
    }
}
impl Drop for Pins {
    fn drop(&mut self) {
        for pin in [board::I2C_SDA, board::I2C_SCL] {
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
        I2C.ic_intr_mask().write(|_| {});
        // A hardware reset also discards FIFOs/abort state left by cancellation.
        pac::RESETS.reset().modify(|w| {
            if I2C_INDEX == 1 {
                w.set_i2c1(true);
            } else {
                w.set_i2c0(true);
            }
        });
        pac::RESETS.reset().modify(|w| {
            if I2C_INDEX == 1 {
                w.set_i2c1(false);
            } else {
                w.set_i2c0(false);
            }
        });
        let result = with_timeout(Duration::from_millis(20), async {
            while !if I2C_INDEX == 1 {
                pac::RESETS.reset_done().read().i2c1()
            } else {
                pac::RESETS.reset_done().read().i2c0()
            } {
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
