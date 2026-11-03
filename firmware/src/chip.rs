//! CMX918 I2C control. Sources: D/918/2.0 pp.20–31,56–60; UM918/2.0
//! registers 03,08–0B,28–2A,5A–5C,B7–BA,D0–D1,D7–D9.
//! Uses documented defaults rather than replaying captured readbacks/poll counts.

use crate::{Config, Error};
use crate::{
    astra::ReferenceClock,
    controls::Controls,
    iq_cal::{self, Context, Correction, Pilot},
};
use embedded_hal_async::{delay::DelayNs, i2c::I2c};

pub const ADDRESS: u8 = 0x55;
// D/918/2.0 Table 5: first 16 FIR1 and first 40 FIR2 coefficients.
const FIR: [i16; 56] = [
    -13, -137, -56, 573, -60, -1824, 1284, 8378, -61, -173, 249, 562, -1113, -1190, 4998, 9744, 28,
    44, 54, 38, -11, -82, -146, -163, -107, 16, 157, 244, 211, 43, -201, -402, -435, -236, 142,
    531, 716, 545, 28, -629, -1092, -1057, -424, 602, 1551, 1876, 1229, -310, -2162, -3410, -3145,
    -870, 3214, 8185, 12674, 15332,
];

pub struct Chip<I, D> {
    bus: I,
    delay: D,
    iq: iq_cal::State,
}

impl<I: I2c, D: DelayNs> Chip<I, D> {
    pub fn new(bus: I, delay: D) -> Self {
        Self {
            bus,
            delay,
            iq: iq_cal::State::default(),
        }
    }

    /// Caller must stop capture first and bound the entire operation. Retry
    /// once after bus clear and CMX918 software reset (UM918/2.0 register 04).
    /// Validate before touching hardware; never report partially applied state.
    pub async fn configure_recovering(
        &mut self,
        config: Config,
        controls: Controls,
    ) -> Result<(), Error>
    where
        I: crate::recovery::Recover,
    {
        self.configure_recovering_clock(config, controls, ReferenceClock::Internal)
            .await
    }

    pub async fn configure_recovering_clock(
        &mut self,
        config: Config,
        controls: Controls,
        reference: ReferenceClock,
    ) -> Result<(), Error>
    where
        I: crate::recovery::Recover,
    {
        self.configure_recovering_clock_with_if(
            config,
            controls,
            reference,
            config.frequency >= 2_000_000,
            96_000,
        )
        .await
    }

    pub async fn configure_recovering_clock_with_if(
        &mut self,
        config: Config,
        controls: Controls,
        reference: ReferenceClock,
        lo_below: bool,
        if_hz: u32,
    ) -> Result<(), Error>
    where
        I: crate::recovery::Recover,
    {
        self.configure_recovering_iq(
            config, controls, reference, lo_below, if_hz, true, false, false,
        )
        .await
    }

    pub fn iq_cal_status(&self) -> u8 {
        self.iq.status
    }

    /// Use the internal pilot once without changing the automatic retune policy.
    /// The requested carrier is restored before this returns successfully.
    #[allow(clippy::too_many_arguments)]
    pub async fn calibrate_iq_now(
        &mut self,
        config: Config,
        controls: Controls,
        reference: ReferenceClock,
        lo_below: bool,
        if_hz: u32,
        running: bool,
    ) -> Result<(), Error> {
        config.validate()?;
        controls.validate(config)?;
        self.finish_iq(
            config, controls, reference, lo_below, if_hz, running, true, true,
        )
        .await
    }

    /// The pilot request is optional; its lock/ready failure never rejects a valid RF tune.
    #[allow(clippy::too_many_arguments)]
    pub async fn configure_recovering_iq(
        &mut self,
        config: Config,
        controls: Controls,
        reference: ReferenceClock,
        lo_below: bool,
        if_hz: u32,
        running: bool,
        pilot_on_retune: bool,
        force_pilot: bool,
    ) -> Result<(), Error>
    where
        I: crate::recovery::Recover,
    {
        config.validate()?;
        controls.validate(config)?;
        let attempt = async {
            self.configure_controls_clock_with_if(config, controls, reference, lo_below, if_hz)
                .await?;
            self.finish_iq(
                config,
                controls,
                reference,
                lo_below,
                if_hz,
                running,
                pilot_on_retune,
                force_pilot,
            )
            .await
        };
        match attempt.await {
            Ok(()) => Ok(()),
            Err(error @ (Error::I2c | Error::Timeout | Error::Readback)) => {
                #[cfg(target_arch = "arm")]
                defmt::warn!(
                    "CMX918 configuration failed error={}; recovering once",
                    error as u32
                );
                #[cfg(not(target_arch = "arm"))]
                let _ = error;
                self.bus.recover().await.map_err(|_| Error::I2c)?;
                self.reset().await?;
                self.iq.disable();
                let result = self
                    .configure_controls_clock_with_if(config, controls, reference, lo_below, if_hz)
                    .await;
                let result = match result {
                    Ok(()) => {
                        self.finish_iq(
                            config,
                            controls,
                            reference,
                            lo_below,
                            if_hz,
                            running,
                            pilot_on_retune,
                            force_pilot,
                        )
                        .await
                    }
                    Err(e) => Err(e),
                };
                #[cfg(target_arch = "arm")]
                defmt::info!(
                    "CMX918 configuration recovery result={}",
                    result.err().map_or(0, |e| e as u32)
                );
                result
            }
            Err(error) => Err(error),
        }
    }

    /// Keyed TX update: never reset, calibrate, switch routes or touch L/R.
    /// Validate the locked plan before issuing even the first register write.
    pub async fn retune_tx(
        &mut self,
        millihz: u64,
        previous: crate::tx_pll::Plan,
    ) -> Result<crate::tx_pll::Plan, Error> {
        let next = crate::tx_pll::retune_millihz(millihz, previous)?;
        for (reg, old, value) in [
            (0x2b, (previous.n >> 8) as u8, (next.n >> 8) as u8),
            (0x2c, previous.n as u8, next.n as u8),
            (
                0x2d,
                (previous.fraction >> 16) as u8,
                (next.fraction >> 16) as u8,
            ),
            (
                0x2e,
                (previous.fraction >> 8) as u8,
                (next.fraction >> 8) as u8,
            ),
            (0x2f, previous.fraction as u8, next.fraction as u8),
        ] {
            if old != value {
                self.write(reg, value).await?;
            }
        }
        Ok(next)
    }

    pub async fn read(&mut self, register: u8) -> Result<u8, Error> {
        let mut value = [0];
        self.bus
            .write_read(ADDRESS, &[register], &mut value)
            .await
            .map_err(|_error| {
                #[cfg(target_arch = "arm")]
                defmt::warn!(
                    "I2C read failed reg={=u8:#x}: {}",
                    register,
                    defmt::Debug2Format(&_error)
                );
                Error::I2c
            })?;
        #[cfg(target_arch = "arm")]
        defmt::trace!("I2C read reg={=u8:#x} value={=u8:#x}", register, value[0]);
        Ok(value[0])
    }

    pub async fn write(&mut self, register: u8, value: u8) -> Result<(), Error> {
        #[cfg(target_arch = "arm")]
        defmt::trace!("I2C write reg={=u8:#x} value={=u8:#x}", register, value);
        self.bus
            .write(ADDRESS, &[register, value])
            .await
            .map_err(|_error| {
                #[cfg(target_arch = "arm")]
                defmt::warn!(
                    "I2C write failed reg={=u8:#x}: {}",
                    register,
                    defmt::Debug2Format(&_error)
                );
                Error::I2c
            })
    }

    async fn update(&mut self, register: u8, mask: u8, value: u8) -> Result<(), Error> {
        let previous = self.read(register).await?;
        self.write(register, (previous & !mask) | (value & mask))
            .await
    }

    pub async fn mute(&mut self, mute: bool) -> Result<(), Error> {
        self.update(0x28, 8, if mute { 8 } else { 0 }).await?;
        for _ in 0..20 {
            if self.read(0x60).await? & 1 == u8::from(mute) {
                return Ok(());
            }
            self.delay.delay_ms(1).await;
        }
        Err(Error::Timeout)
    }

    pub async fn configure(&mut self, config: Config) -> Result<(), Error> {
        self.configure_controls(config, Controls::default()).await
    }

    pub async fn configure_controls(
        &mut self,
        config: Config,
        controls: Controls,
    ) -> Result<(), Error> {
        self.configure_controls_clock(config, controls, ReferenceClock::Internal)
            .await
    }

    pub async fn configure_controls_clock(
        &mut self,
        config: Config,
        controls: Controls,
        reference: ReferenceClock,
    ) -> Result<(), Error> {
        self.configure_controls_clock_with_if(
            config,
            controls,
            reference,
            config.frequency >= 2_000_000,
            96_000,
        )
        .await?;
        self.finish_iq(
            config,
            controls,
            reference,
            config.frequency >= 2_000_000,
            96_000,
            true,
            false,
            false,
        )
        .await
    }

    async fn configure_controls_clock_with_if(
        &mut self,
        config: Config,
        controls: Controls,
        reference: ReferenceClock,
        lo_below: bool,
        if_hz: u32,
    ) -> Result<(), Error> {
        let config = config.validate()?;
        let controls = controls.validate(config)?;
        let plan = crate::rx_pll::plan(config.frequency, lo_below, if_hz)?;
        let pll_registers = [
            (0x2a, 1), // internal VCO, manual L, fractional DSM
            (0x2b, (plan.n >> 8) as u8),
            (0x2c, plan.n as u8),
            (0x2d, (plan.fraction >> 16) as u8),
            (0x2e, (plan.fraction >> 8) as u8),
            (0x2f, plan.fraction as u8),
            (0x30, 0), // R=1 with the 38.4 MHz reference
            (0x31, (plan.l >> 8) as u8),
            (0x32, plan.l as u8),
            (0x33, (plan.l >> 8) as u8), // active L status, read-only
            (0x34, plan.l as u8),
        ];
        // Stop capture before entering this routine. Standby quiesces processing
        // even if a pre-existing stream's frame-synchronous mute cannot finish.
        self.write(0x03, 1).await?;
        // CLK_CTL bit 0 selects crystal/TCXO; preserve the clock output settings.
        self.update(0x07, 1, reference as u8).await?;
        self.write(0x97, config.xtal_control()).await?;
        self.delay.delay_ms(10).await;
        self.write(0x28, config.output_control(true)).await?;
        for &(register, value) in &pll_registers[..9] {
            self.write(register, value).await?;
        }
        // Reset manual output-clock overrides; choose one frame per sample.
        self.write(0x5a, 0).await?; // documented 20 clocks/channel, 40/frame
        self.write(0x5b, 0x19).await?; // single frame, manual ratio x1
        self.write(0x5c, 0).await?; // no manual master-clock division
        self.update(0xb7, 0x80, 0).await?; // direct SCLK, sample on rising edge
        self.write(0xb8, 0x60).await?; // keep running compensation off until PLL locks
        self.write(0x51, 0x70).await?; // no stale one-shot correction during a retune
        self.write(0xba, 0x0a).await?; // demodulation/DC correction, manual digital gain
        // Digital AGC is bypassed above; restore documented manual defaults.
        for (r, v) in [
            (0xbb, 0x3e),
            (0xbc, 0),
            (0xbd, 0x80),
            (0xbe, 0),
            (0xbf, 0x80),
            // $0C defaults to LF/MF and HF/VHF automatic RF gain. UM918/2.0
            // p.15: bit4 pauses AGC; it must never be set for normal setup.
            (0x0c, 0),
        ] {
            self.write(r, v).await?;
        }
        // Fc carries rounded metadata, IF/sign and RF mixer selection only.
        // Exact-Hz PLL coefficients above never depend on this 100 Hz field.
        let fc = config.carrier_with(lo_below, if_hz);
        for (i, v) in fc.iter().enumerate() {
            self.write(0x08 + i as u8, *v).await?;
        }
        self.write(0x0b, config.bandwidth_code()).await?;
        let fir2 = if config.flags & crate::WIDE_FIR != 0 {
            crate::wide_fir::coefficients(config)
        } else {
            &crate::controls::FIR2_DEFAULT
        };
        #[cfg(target_arch = "arm")]
        defmt::info!(
            "FIR2 wide={} rate={} half_sum={}",
            config.flags & crate::WIDE_FIR != 0,
            config.rate,
            fir2.iter().map(|&v| i32::from(v)).sum::<i32>()
        );
        for (index, coefficient) in FIR[..16].iter().chain(fir2.iter()).enumerate() {
            let [msb, lsb] = coefficient.to_be_bytes();
            self.write(0xd7, index as u8).await?;
            self.write(0xd8, msb).await?;
            self.write(0xd9, lsb).await?; // LSB commits coefficient, no auto-increment
        }
        self.write(0x03, 3).await?;
        // IF filter calibration + VCO sub-band/SAS only. Never request PLL
        // calculation (bit0), which would replace our exact N/F from Fc.
        self.write(0x29, 6).await?;
        for attempt in 0..200 {
            if self.read(0x29).await? & 7 == 0 {
                break;
            }
            if attempt == 199 {
                return Err(Error::Timeout);
            }
            self.delay.delay_ms(5).await;
        }
        // Check the physical lock indicator separately from calibration completion.
        let mut locked = false;
        for _ in 0..100 {
            if self.read(0x06).await? & 8 != 0 {
                locked = true;
                break;
            }
            self.delay.delay_ms(5).await;
        }
        if !locked {
            return Err(Error::Timeout);
        }
        // Check programmed N/F/R/L and active L after SAS, not only lock.
        for &(register, expected) in &pll_registers {
            if self.read(register).await? != expected {
                return Err(Error::Readback);
            }
        }
        // Ported from cmx918audiocat 5e4d7d9 (MIT). DS p.28: writing Fc
        // alone does not execute PLL calculation. Preserve the calibrated PLL.
        let routed_fc = controls.input.routing_carrier_with(config, fc);
        if routed_fc != fc {
            for (i, value) in routed_fc.iter().enumerate() {
                self.write(8 + i as u8, *value).await?;
            }
        }
        // UM p.65 $A2: LF/MF bit2, HF bit1, VHF bit0. Explicitly restore AUTO
        // selection too, so previous manual selections cannot survive retunes.
        let input_mask = controls.input.mask(config.frequency);
        self.update(0xa2, 7, input_mask).await?;
        let actual_input = controls.input.actual(config.frequency);
        let rf_gc_ctl = if controls.rf_auto {
            0
        } else if actual_input == crate::controls::RfInput::Lf {
            // $0C bits3:1: manual LF attenuator, mixer and LNA.
            0x0e
        } else {
            // $0C bit0: manual HF/VHF LNA.
            0x01
        };
        self.write(0x0c, rf_gc_ctl).await?;
        if actual_input == crate::controls::RfInput::Lf {
            // $13 high nibble is mixer, low nibble LNA. The UI deliberately
            // drives both with one code; see controls::LF_*_GAIN_DB10.
            self.write(0x13, controls.lf_gain << 4 | controls.lf_gain)
                .await?;
            self.write(0x16, controls.lf_attenuator).await?;
        } else {
            self.write(0x0f, controls.rf_gain).await?;
        }
        self.write(0x20, controls.if_gain).await?;
        self.write(0x21, controls.if_gain).await?;
        self.write(0x1a, u8::from(!controls.if_auto)).await?;
        self.set_lf_mf_capacitor(controls.lf_mf_capacitor).await?;
        if self.read(0xa2).await? & 7 != input_mask || self.read(0x06).await? & 8 == 0 {
            return Err(Error::Readback);
        }
        // Verify again after forced LF/HF routing and all control writes.
        for &(register, expected) in &pll_registers {
            if self.read(register).await? != expected {
                return Err(Error::Readback);
            }
        }
        for (r, v) in [
            (8, routed_fc[0]),
            (9, routed_fc[1]),
            (10, routed_fc[2]),
            (11, config.bandwidth_code()),
            (0x28, config.output_control(true)),
            (0x5a, 0),
            (0x5b, 0x19),
            (0x5c, 0),
            (0x97, config.xtal_control()),
        ] {
            let actual = self.read(r).await?;
            if actual != v {
                #[cfg(target_arch = "arm")]
                defmt::warn!(
                    "readback reg={=u8:#x} expected={=u8:#x} actual={=u8:#x}",
                    r,
                    v,
                    actual
                );
                return Err(Error::Readback);
            }
        }
        Ok(())
    }

    /// UM918/2.0 pp.20–21: $14 upper four bits (reserved nibble zero),
    /// $15 lower eight bits. Caller stops capture and bounds the operation.
    pub async fn set_lf_mf_capacitor(&mut self, code: u16) -> Result<(), Error> {
        if code > 4095 {
            return Err(Error::Profile);
        }
        let [hi, lo] = code.to_be_bytes();
        self.write(0x14, hi).await?;
        self.write(0x15, lo).await?;
        if self.read(0x14).await? != hi || self.read(0x15).await? != lo {
            return Err(Error::Readback);
        }
        #[cfg(target_arch = "arm")]
        defmt::info!("LF/MF capacitor applied code={}", code);
        Ok(())
    }

    async fn wait_auto(&mut self, mask: u8, polls: usize) -> Result<bool, Error> {
        for _ in 0..polls {
            if self.read(0x29).await? & mask == 0 {
                return Ok(true);
            }
            self.delay.delay_ms(10).await;
        }
        Ok(false)
    }

    async fn write_carrier(
        &mut self,
        config: Config,
        lo_below: bool,
        if_hz: u32,
    ) -> Result<(), Error> {
        for (index, value) in config.carrier_with(lo_below, if_hz).into_iter().enumerate() {
            self.write(8 + index as u8, value).await?;
        }
        let plan = crate::rx_pll::plan(config.frequency, lo_below, if_hz)?;
        for (reg, value) in [
            (0x2a, 1),
            (0x2b, (plan.n >> 8) as u8),
            (0x2c, plan.n as u8),
            (0x2d, (plan.fraction >> 16) as u8),
            (0x2e, (plan.fraction >> 8) as u8),
            (0x2f, plan.fraction as u8),
            (0x30, 0),
            (0x31, (plan.l >> 8) as u8),
            (0x32, plan.l as u8),
        ] {
            self.write(reg, value).await?;
        }
        self.write(0x29, 6).await?;
        if !self.wait_auto(7, 100).await? || self.read(0x06).await? & 8 == 0 {
            return Err(Error::Timeout);
        }
        Ok(())
    }

    async fn pilot_measure(
        &mut self,
        context: Context,
        pilot: Pilot,
        config: Config,
    ) -> Result<Option<Correction>, Error> {
        if pilot.calibration_center != context.center {
            self.write_carrier(
                Config {
                    frequency: pilot.calibration_center,
                    ..config
                },
                context.lo_below,
                context.if_hz,
            )
            .await?;
        }
        // The pilot measurement must not compete with continuous correction.
        // A one-shot command can arrive while running mode is active.
        self.write(0xb8, 0x60).await?;
        self.write(0x51, 0x70).await?;
        // A previous completed calibration must no longer assert READY;
        // otherwise an old coefficient pair could be mistaken for this run.
        let mut cleared = false;
        for _ in 0..20 {
            if self.read(0xd4).await? & 2 == 0 {
                cleared = true;
                break;
            }
            self.delay.delay_ms(10).await;
        }
        if !cleared {
            #[cfg(target_arch = "arm")]
            defmt::warn!("I/Q pilot: previous READY did not clear");
            return Ok(None);
        }
        self.write(0x9e, 0).await?; // never route pilot onto the pin-8 test output
        for (reg, value) in [
            (0x99, (pilot.n >> 8) as u8),
            (0x9a, pilot.n as u8),
            (0x9b, (pilot.p >> 8) as u8),
            (0x9c, pilot.p as u8),
            (0x9d, 3), // documented 64 mV default; avoid the highest injection gain
        ] {
            self.write(reg, value).await?;
        }
        self.write(0x03, 5).await?; // IQ calibration mode
        self.write(
            0x98,
            if context.input == crate::controls::RfInput::Lf {
                3
            } else {
                5
            },
        )
        .await?;
        self.write(0x29, 8).await?; // PILOT_EXE
        let pilot_done = self.wait_auto(8, 50).await?;
        let pilot_lock = self.read(0x06).await? & 0x40 != 0;
        if !pilot_done || !pilot_lock {
            #[cfg(target_arch = "arm")]
            defmt::warn!(
                "I/Q pilot: PLL execute done={} lock={}",
                pilot_done,
                pilot_lock
            );
            return Ok(None);
        }
        // $51 automatic mode + ON starts the digital calibration. On the
        // connected CMX918, an additional $29 IQ_COMP_EXE write after this
        // prevents READY from asserting, despite updating coefficient bytes.
        self.write(0x51, 0x73).await?;
        let mut ready = false;
        for _ in 0..200 {
            if self.read(0xd4).await? & 2 != 0 {
                ready = true;
                break;
            }
            self.delay.delay_ms(10).await;
        }
        if !ready {
            #[cfg(target_arch = "arm")]
            defmt::warn!(
                "I/Q pilot: READY did not assert 03={=u8:#x} 06={=u8:#x} 29={=u8:#x} 51={=u8:#x} 98={=u8:#x} D4={=u8:#x}",
                self.read(0x03).await.unwrap_or(0xff),
                self.read(0x06).await.unwrap_or(0xff),
                self.read(0x29).await.unwrap_or(0xff),
                self.read(0x51).await.unwrap_or(0xff),
                self.read(0x98).await.unwrap_or(0xff),
                self.read(0xd4).await.unwrap_or(0xff)
            );
            return Ok(None);
        }
        let i = i16::from_be_bytes([self.read(0x56).await?, self.read(0x57).await?]);
        let q = i16::from_be_bytes([self.read(0x58).await?, self.read(0x59).await?]);
        #[cfg(target_arch = "arm")]
        defmt::info!("I/Q pilot: correction ready i={} q={}", i, q);
        Ok(Some(Correction { i, q }))
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_iq(
        &mut self,
        config: Config,
        controls: Controls,
        reference: ReferenceClock,
        lo_below: bool,
        if_hz: u32,
        running: bool,
        pilot_on_retune: bool,
        force_pilot: bool,
    ) -> Result<(), Error> {
        let context = Context {
            center: config.frequency,
            input: controls.input.actual(config.frequency),
            reference,
            lo_below,
            if_hz,
        };
        if !pilot_on_retune {
            self.iq.disable();
        } else if self.iq.needed(context, true, force_pilot) {
            let normal_fc = config.carrier_with(lo_below, if_hz);
            let routed_fc = controls.input.routing_carrier_with(config, normal_fc);
            // Forced LF/HF routing across 2 MHz rewrites Fc without moving
            // the calibrated LO. A temporary pilot retune cannot safely
            // reproduce that routing, so leave this unvalidated case alone.
            let result = if routed_fc == normal_fc
                && let Some(pilot) = Pilot::for_context(context)
            {
                let measured = self.pilot_measure(context, pilot, config).await;
                // Always remove the injected signal and restore the requested RF tune.
                self.write(0x98, 0).await?;
                self.write(0x03, 3).await?;
                if pilot.calibration_center != context.center {
                    self.write_carrier(config, lo_below, if_hz).await?;
                }
                match measured {
                    Ok(result) => result,
                    // A pilot-only lock failure must not reject the already
                    // successful requested RF tune. Restoring that tune above
                    // remains mandatory and still propagates its error.
                    Err(Error::Timeout) => None,
                    Err(error) => return Err(error),
                }
            } else {
                None
            };
            match result {
                Some(correction) => self.iq.succeed(context, correction),
                None => self.iq.fail(context),
            }
        }
        if let Some(correction) = self
            .iq
            .compatible_correction(context)
            .filter(|_| pilot_on_retune)
        {
            for (reg, value) in [
                (0x52, correction.i.to_be_bytes()[0]),
                (0x53, correction.i.to_be_bytes()[1]),
                (0x54, correction.q.to_be_bytes()[0]),
                (0x55, correction.q.to_be_bytes()[1]),
            ] {
                self.write(reg, value).await?;
            }
            // Cached coefficients are in the manual registers. Running
            // compensation is an independent $B8 control.
            self.write(0x51, 0x71).await?;
        } else {
            self.write(0x51, 0x70).await?;
        }
        self.write(0xb8, if running { 0x64 } else { 0x60 }).await?;
        Ok(())
    }

    /// Streaming path for the LF/MF slider. A consecutive write to $14/$15
    /// avoids four separate I2C address phases while the CMX918 is producing
    /// samples. Stopped configuration still uses `set_lf_mf_capacitor` above
    /// for readback verification.
    pub async fn set_lf_mf_capacitor_live(&mut self, code: u16) -> Result<(), Error> {
        if code > 4095 {
            return Err(Error::Profile);
        }
        let [hi, lo] = code.to_be_bytes();
        self.bus
            .write(ADDRESS, &[0x14, hi, lo])
            .await
            .map_err(|_error| {
                #[cfg(target_arch = "arm")]
                defmt::warn!(
                    "I2C live capacitor write failed: {}",
                    defmt::Debug2Format(&_error)
                );
                Error::I2c
            })?;
        #[cfg(target_arch = "arm")]
        defmt::debug!("LF/MF capacitor live code={}", code);
        Ok(())
    }

    /// A slider update must not reset or recalibrate a streaming receiver.
    /// Clear a stuck I2C bus and retry the one atomic bank write once; leave
    /// the caller's desired setting unchanged if this bounded recovery fails.
    pub async fn set_lf_mf_capacitor_live_recovering(&mut self, code: u16) -> Result<(), Error>
    where
        I: crate::recovery::Recover,
    {
        match self.set_lf_mf_capacitor_live(code).await {
            Ok(()) => Ok(()),
            Err(Error::I2c) => {
                #[cfg(target_arch = "arm")]
                defmt::warn!("LF/MF live capacitor write failed; clearing I2C and retrying once");
                self.bus.recover().await.map_err(|_| Error::I2c)?;
                self.set_lf_mf_capacitor_live(code).await
            }
            Err(error) => Err(error),
        }
    }

    /*
    // Manual VCO band-cell edge experiment — intentionally disabled.
    //
    // Performed on RP2350A / CMX918 on 2026-09-15 after a normal, stopped
    // configuration at Fc=118_846_000 Hz. AUTO_EXE produced N=49,
    // F=0x7A_AA_AA, R=1, L=4, i.e. FVCO=1_899_999_998 Hz. A subsequent raw,
    // stopped sweep set N=55, F=0x55_55_55, R=1, L=4: calculated FVCO is
    // 2_124_799_999 Hz. This point returned STATUS=0x3C for 300 reads / 30 s;
    // the high VCO-control-voltage comparator is asserted, so this is not an
    // operating-margin result. At 2_124_900_000 Hz one of ten reads unlocked;
    // 2_125_000_000 Hz was unlocked throughout.
    //
    // The normal configuration path and its automatic filter calibration/SAS
    // algorithm must remain the production path; do not enable this while
    // streaming. UM918/2.0 pp.58–60: clear PLL_SAS_MD in $95, then select
    // minimum VCO capacitance / band cell 0 in $92. This manually sets PLL L,
    // R, N and F without writing AUTO_EXE bit0, which would recalculate them. A
    // normal configure_controls() immediately follows any re-run to restore
    // automatic calibration.
    async fn manual_vco_high_edge_experiment(&mut self) -> Result<(), Error> {
        self.write(0x2a, 0x01).await?; // internal VCO, manual L, fractional DSM
        self.write(0x30, 0x00).await?; // R=1
        self.write(0x31, 0x00).await?;
        self.write(0x32, 0x04).await?; // L=4
        self.write(0x2b, 0x00).await?;
        self.write(0x2c, 0x37).await?; // N=55
        self.write(0x2d, 0x55).await?;
        self.write(0x2e, 0x55).await?;
        self.write(0x2f, 0x55).await?; // F=0x555555
        self.write(0x95, 0x08).await?; // SAS manual; DAC disabled, code 8 retained
        self.write(0x92, 0x00).await?; // minimum VCO capacitance / band cell 0
        self.delay.delay_ms(350).await;
        if self.read(0x06).await? & 0x08 == 0 || self.read(0x96).await? & 0x80 == 0 {
            return Err(Error::Timeout);
        }
        Ok(())
    }
    */

    /// Caller must stop capture and mute first. Only the documented FIR2 table
    /// is enabled until custom CIC-compensating responses are RF-validated.
    pub async fn upload_fir(&mut self, coefficients: &[i16]) -> Result<(), Error> {
        crate::controls::validate_fir(coefficients)?;
        if coefficients != crate::controls::FIR2_DEFAULT {
            return Err(Error::Profile);
        }
        self.write(0x03, 1).await?;
        for (i, c) in coefficients.iter().enumerate() {
            let [hi, lo] = c.to_be_bytes();
            self.write(0xd7, 16 + i as u8).await?;
            self.write(0xd8, hi).await?;
            self.write(0xd9, lo).await?;
            if self.read(0xd8).await? != hi || self.read(0xd9).await? != lo {
                return Err(Error::Readback);
            }
        }
        self.write(0x03, 3).await?;
        Ok(())
    }

    pub async fn reset(&mut self) -> Result<(), Error> {
        // UM918/2.0 p.11, $04: software reset is host-cleared, retains I2C.
        self.write(0x04, 1).await?;
        self.write(0x04, 0).await?;
        self.delay.delay_ms(10).await;
        if self.read(0x04).await? != 0 {
            return Err(Error::Readback);
        }
        Ok(())
    }

    /// Returns raw STATUS and signed RSSI in 1/16 dB units. Absolute RSSI
    /// calibration is board-dependent and is not established by this firmware.
    pub async fn status(&mut self) -> Result<(u8, i16), Error> {
        let status = self.read(0x06).await?;
        let mut raw = [0; 2];
        self.bus
            .write_read(ADDRESS, &[0xd0], &mut raw)
            .await
            .map_err(|_| Error::I2c)?;
        let value = u16::from_be_bytes(raw) & 0xfff;
        Ok((status, ((value << 4) as i16) >> 4))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use embedded_hal::i2c::{ErrorType, Operation};
    use std::vec::Vec;
    struct Bus {
        regs: [u8; 256],
        writes: Vec<(u8, u8)>,
        busy: bool,
        fail: bool,
        recoveries: u8,
        heal: bool,
        recovery_fails: bool,
        corrupt_read: Option<u8>,
        corrupt_on_route: Option<u8>,
        ready_on_iq: bool,
    }
    impl Bus {
        fn new() -> Self {
            let mut b = Self {
                regs: [0; 256],
                writes: Vec::new(),
                busy: false,
                fail: false,
                recoveries: 0,
                heal: true,
                recovery_fails: false,
                corrupt_read: None,
                corrupt_on_route: None,
                ready_on_iq: false,
            };
            b.regs[6] = 8;
            b
        }
    }
    impl ErrorType for Bus {
        type Error = embedded_hal::i2c::ErrorKind;
    }
    impl crate::recovery::Recover for Bus {
        async fn recover(&mut self) -> Result<(), Self::Error> {
            self.recoveries += 1;
            if self.recovery_fails {
                return Err(embedded_hal::i2c::ErrorKind::Bus);
            }
            if self.heal {
                self.busy = false;
                self.fail = false;
            }
            Ok(())
        }
    }
    impl I2c for Bus {
        async fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x55);
            if self.fail {
                return Err(embedded_hal::i2c::ErrorKind::Bus);
            }
            let mut reg = 0;
            for op in operations {
                match op {
                    Operation::Write(bytes) => {
                        reg = bytes[0] as usize;
                        for &value in &bytes[1..] {
                            self.regs[reg] = value;
                            self.writes.push((reg as u8, value));
                            if reg == 0x29 && !self.busy {
                                self.regs[reg] = 0;
                                if self.regs[0x2a] & 2 == 0 {
                                    self.regs[0x33] = self.regs[0x31];
                                    self.regs[0x34] = self.regs[0x32];
                                }
                            }
                            if reg == 0x51 && value & 1 == 0 {
                                self.regs[0xd4] &= !2;
                            }
                            if reg == 0x51 && value == 0x73 && self.ready_on_iq {
                                self.regs[0xd4] |= 2;
                            }
                            if reg == 0x28 {
                                self.regs[0x60] = u8::from(value & 8 != 0);
                            }
                            if reg == 0x0a
                                && self.regs[0x03] == 3
                                && let Some(register) = self.corrupt_on_route
                            {
                                self.regs[usize::from(register)] ^= 1;
                            }
                            reg += 1;
                        }
                    }
                    Operation::Read(bytes) => {
                        for value in bytes.iter_mut() {
                            *value = self.regs[reg];
                            if self.corrupt_read == Some(reg as u8) {
                                *value ^= 1;
                            }
                            reg += 1;
                        }
                    }
                }
            }
            Ok(())
        }
    }
    struct Delay;
    impl DelayNs for Delay {
        async fn delay_ns(&mut self, _: u32) {}
    }
    fn config() -> Config {
        Config {
            frequency: 14_200_000,
            rate: 48_000,
            bandwidth: 10_000,
            flags: 2,
        }
    }
    #[test]
    fn keyed_tx_retunes_only_write_changed_n_and_f_registers() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            let mut plan = crate::tx_pll::plan(50_000_000).unwrap();
            let original = plan;
            for millihz in [
                50_000_000_001,
                50_000_006_250,
                50_000_020_833,
                49_000_000_000,
                50_000_000_000,
            ] {
                chip.bus.writes.clear();
                let old = plan;
                plan = chip.retune_tx(millihz, plan).await.unwrap();
                assert_eq!((plan.l, plan.divider), (original.l, original.divider));
                assert!(
                    chip.bus
                        .writes
                        .iter()
                        .all(|(reg, _)| (0x2b..=0x2f).contains(reg))
                );
                for (reg, before, after) in [
                    (0x2b, (old.n >> 8) as u8, (plan.n >> 8) as u8),
                    (0x2c, old.n as u8, plan.n as u8),
                    (
                        0x2d,
                        (old.fraction >> 16) as u8,
                        (plan.fraction >> 16) as u8,
                    ),
                    (0x2e, (old.fraction >> 8) as u8, (plan.fraction >> 8) as u8),
                    (0x2f, old.fraction as u8, plan.fraction as u8),
                ] {
                    assert_eq!(chip.bus.writes.contains(&(reg, after)), before != after);
                }
            }
            chip.bus.writes.clear();
            assert_eq!(chip.retune_tx(50_000_000_000, plan).await.unwrap(), plan);
            assert!(chip.bus.writes.is_empty());
            assert_eq!(
                chip.retune_tx(60_000_000_000, plan).await,
                Err(Error::Frequency)
            );
            assert!(chip.bus.writes.is_empty());
        });
    }

    #[test]
    fn capacitor_full_range_reserved_bits_readback_and_errors() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            for code in 0..=4095 {
                chip.bus.writes.clear();
                chip.set_lf_mf_capacitor(code).await.unwrap();
                assert_eq!(
                    chip.bus.writes,
                    [(0x14, (code >> 8) as u8), (0x15, code as u8)]
                );
                assert_eq!(chip.bus.regs[0x14] & 0xf0, 0);
                assert_eq!(
                    u16::from_be_bytes([chip.bus.regs[0x14], chip.bus.regs[0x15]]),
                    code
                );
            }
            chip.bus.writes.clear();
            for code in [4096, u16::MAX] {
                assert_eq!(chip.set_lf_mf_capacitor(code).await, Err(Error::Profile));
            }
            assert!(chip.bus.writes.is_empty());
            for register in [0x14, 0x15] {
                chip.bus.corrupt_read = Some(register);
                assert_eq!(chip.set_lf_mf_capacitor(0xabc).await, Err(Error::Readback));
            }
            chip.bus.corrupt_read = None;
            chip.bus.fail = true;
            assert_eq!(chip.set_lf_mf_capacitor(1).await, Err(Error::I2c));
        });
    }
    #[test]
    fn live_capacitor_update_uses_one_write_and_bounded_bus_retry() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            chip.set_lf_mf_capacitor_live_recovering(0xabc)
                .await
                .unwrap();
            assert_eq!(chip.bus.writes, [(0x14, 0x0a), (0x15, 0xbc)]);
            assert_eq!(chip.bus.recoveries, 0);

            chip.bus.writes.clear();
            chip.bus.fail = true;
            chip.set_lf_mf_capacitor_live_recovering(0x123)
                .await
                .unwrap();
            assert_eq!(chip.bus.recoveries, 1);
            assert_eq!(chip.bus.writes, [(0x14, 1), (0x15, 0x23)]);

            chip.bus.fail = true;
            chip.bus.recovery_fails = true;
            assert_eq!(
                chip.set_lf_mf_capacitor_live_recovering(0x456).await,
                Err(Error::I2c)
            );
        });
    }
    #[test]
    fn raw_register_access_preserves_full_address_and_value() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            for (reg, value) in [(0, 0xff), (0x14, 0xab), (0x80, 0x12), (0xff, 0xfe)] {
                chip.write(reg, value).await.unwrap();
                assert_eq!(chip.read(reg).await.unwrap(), value);
            }
            assert_eq!(
                chip.bus.writes,
                [(0, 0xff), (0x14, 0xab), (0x80, 0x12), (0xff, 0xfe)]
            );
            chip.bus.fail = true;
            assert_eq!(chip.read(0xff).await, Err(Error::I2c));
            assert_eq!(chip.write(0xff, 0).await, Err(Error::I2c));
        });
    }
    #[test]
    fn initialization_orders_carrier_and_commits_every_coefficient() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            chip.configure(config()).await.unwrap();
            let writes = &chip.bus.writes;
            assert!(
                writes
                    .windows(3)
                    .any(|v| v == [(8, 0x82), (9, 0x2a), (10, 0xb0)])
            );
            assert_eq!(writes.iter().filter(|(r, _)| *r == 0xd9).count(), 56);
            assert_eq!(chip.bus.regs[0x28], 0x9b);
            chip.mute(false).await.unwrap();
            assert_eq!(chip.bus.regs[0x60], 0);
        });
    }
    #[test]
    fn pilot_calibration_restores_exact_hz_pll_coefficients() {
        futures::executor::block_on(async {
            let mut bus = Bus::new();
            bus.regs[6] = 0x48;
            bus.ready_on_iq = true;
            let mut chip = Chip::new(bus, Delay);
            let c = Config {
                frequency: 14_200_037,
                ..config()
            };
            chip.configure_recovering_iq(
                c,
                Controls::default(),
                ReferenceClock::Internal,
                true,
                96_000,
                true,
                false,
                false,
            )
            .await
            .unwrap();
            chip.calibrate_iq_now(
                c,
                Controls::default(),
                ReferenceClock::Internal,
                true,
                96_000,
                true,
            )
            .await
            .unwrap();
            let plan = crate::rx_pll::plan(c.frequency, true, 96_000).unwrap();
            assert_eq!(chip.iq_cal_status(), 1);
            assert_eq!(
                &chip.bus.regs[0x2b..0x30],
                &[
                    (plan.n >> 8) as u8,
                    plan.n as u8,
                    (plan.fraction >> 16) as u8,
                    (plan.fraction >> 8) as u8,
                    plan.fraction as u8
                ]
            );
            assert_eq!(
                &chip.bus.regs[0x31..0x33],
                &[(plan.l >> 8) as u8, plan.l as u8]
            );
            assert_eq!(chip.bus.regs[0x98], 0);
            assert!(!chip.bus.writes.contains(&(0x29, 1)));
        });
    }
    #[test]
    fn pilot_calibration_restores_requested_tune_and_reuses_coefficients() {
        futures::executor::block_on(async {
            let mut bus = Bus::new();
            bus.regs[6] = 0x48; // Main and pilot PLL locked.
            bus.ready_on_iq = true; // Digital correction completes on command.
            bus.regs[0x56..0x5a].copy_from_slice(&[0x12, 0x34, 0xff, 0xed]);
            let mut chip = Chip::new(bus, Delay);
            chip.configure_recovering_iq(
                config(),
                Controls::default(),
                ReferenceClock::Internal,
                true,
                96_000,
                false,
                true,
                false,
            )
            .await
            .unwrap();
            assert_eq!(chip.iq_cal_status(), 1);
            assert_eq!(chip.bus.regs[0x98], 0); // Pilot powered down.
            assert_eq!(chip.bus.regs[3], 3); // Normal mode restored.
            assert_eq!(chip.bus.regs[0xb8], 0x60); // Running mode remains off.
            assert_eq!(chip.bus.regs[0x51], 0x71); // Saved correction in manual mode.
            assert_eq!(&chip.bus.regs[0x52..0x56], &[0x12, 0x34, 0xff, 0xed]);
            assert_eq!(&chip.bus.regs[8..11], &config().carrier());
            let pilots = chip.bus.writes.iter().filter(|w| **w == (0x29, 8)).count();
            let small = Config {
                frequency: 14_300_000,
                ..config()
            };
            chip.configure_recovering_iq(
                small,
                Controls::default(),
                ReferenceClock::Internal,
                true,
                96_000,
                true,
                true,
                false,
            )
            .await
            .unwrap();
            assert_eq!(
                chip.bus.writes.iter().filter(|w| **w == (0x29, 8)).count(),
                pilots
            );
            assert_eq!(chip.bus.regs[0xb8], 0x64);
            assert_eq!(&chip.bus.regs[0x52..0x56], &[0x12, 0x34, 0xff, 0xed]);
            let large = Config {
                frequency: 14_700_000,
                ..config()
            };
            chip.configure_recovering_iq(
                large,
                Controls::default(),
                ReferenceClock::Internal,
                true,
                96_000,
                true,
                true,
                false,
            )
            .await
            .unwrap();
            assert_eq!(
                chip.bus.writes.iter().filter(|w| **w == (0x29, 8)).count(),
                pilots + 1
            );
        });
    }
    #[test]
    fn manual_pilot_calibration_runs_now_even_with_automatic_retune_disabled() {
        futures::executor::block_on(async {
            let mut bus = Bus::new();
            bus.regs[6] = 0x48;
            bus.ready_on_iq = true;
            bus.regs[0x56..0x5a].copy_from_slice(&[0x12, 0x34, 0xff, 0xed]);
            let mut chip = Chip::new(bus, Delay);
            chip.configure_recovering_iq(
                config(),
                Controls::default(),
                ReferenceClock::Internal,
                true,
                96_000,
                true,
                false,
                false,
            )
            .await
            .unwrap();
            assert_eq!(chip.iq_cal_status(), 0);
            for expected_runs in 1..=2 {
                let write_start = chip.bus.writes.len();
                chip.calibrate_iq_now(
                    config(),
                    Controls::default(),
                    ReferenceClock::Internal,
                    true,
                    96_000,
                    true,
                )
                .await
                .unwrap();
                assert_eq!(chip.iq_cal_status(), 1);
                assert_eq!(chip.bus.regs[0x98], 0);
                assert_eq!(chip.bus.regs[3], 3);
                assert_eq!(&chip.bus.regs[8..11], &config().carrier());
                assert_eq!(chip.bus.regs[0x51], 0x71);
                assert_eq!(chip.bus.regs[0xb8], 0x64);
                let writes = &chip.bus.writes[write_start..];
                let paused = writes
                    .iter()
                    .position(|write| *write == (0xb8, 0x60))
                    .unwrap();
                let started = writes
                    .iter()
                    .position(|write| *write == (0x51, 0x73))
                    .unwrap();
                let resumed = writes
                    .iter()
                    .rposition(|write| *write == (0xb8, 0x64))
                    .unwrap();
                assert!(paused < started && started < resumed);
                assert!(!writes.contains(&(0x29, 0x10)));
                assert_eq!(
                    chip.bus
                        .writes
                        .iter()
                        .filter(|write| **write == (0x29, 8))
                        .count(),
                    expected_runs
                );
            }
        });
    }
    #[test]
    fn pilot_lock_failure_is_reported_but_rf_tune_survives() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            chip.configure_recovering_iq(
                config(),
                Controls::default(),
                ReferenceClock::Internal,
                true,
                96_000,
                true,
                true,
                false,
            )
            .await
            .unwrap();
            assert_eq!(chip.iq_cal_status(), 2);
            assert_eq!(&chip.bus.regs[8..11], &config().carrier());
            assert_eq!(chip.bus.regs[0x98], 0);
            assert_eq!(chip.bus.regs[3], 3);
            assert_eq!(chip.bus.regs[0x51], 0x70);
            assert_eq!(chip.bus.regs[0xb8], 0x64);
        });
    }
    #[test]
    fn reference_selection_preserves_clock_output_bits_and_recovers_after_reset() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            chip.bus.regs[0x07] = 0x32;
            chip.configure_recovering_clock(
                config(),
                Controls::default(),
                ReferenceClock::External,
            )
            .await
            .unwrap();
            assert_eq!(chip.bus.regs[0x07], 0x33);
            assert_eq!(chip.bus.regs[0x97] & 1, 1);
            chip.configure_recovering_clock(
                config(),
                Controls::default(),
                ReferenceClock::Internal,
            )
            .await
            .unwrap();
            assert_eq!(chip.bus.regs[0x07], 0x32);
        });
    }
    #[test]
    fn high_band_selects_requested_manual_divider() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            let high = Config {
                frequency: 260_000_000,
                flags: config().flags | crate::EXPERIMENTAL_RF,
                ..config()
            };
            chip.configure(high).await.unwrap();
            assert_eq!(chip.bus.regs[0x2a], 1);
            assert_eq!(chip.bus.regs[0x32], 2);
            assert_eq!(chip.bus.regs[0x34], 2);
            assert!(chip.bus.writes.contains(&(0x29, 6)));
            let scaled =
                (u64::from(high.frequency - 96_000) * 8 * (1 << 24) + 19_200_000) / 38_400_000;
            assert_eq!(chip.bus.regs[0x2c], (scaled >> 24) as u8);
            assert_eq!(chip.bus.regs[0x2f], scaled as u8);
        });
    }
    #[test]
    fn precise_rx_programs_known_coefficients_with_both_if_signs_and_rates() {
        futures::executor::block_on(async {
            for (frequency, lo_below, if_hz) in [
                (96_096_001, true, 96_000),
                (95_904_001, false, 96_000),
                (96_120_001, true, 120_000),
                (95_880_001, false, 120_000),
            ] {
                for (bandwidth, rate) in crate::PROFILES
                    .into_iter()
                    .chain([(100_000, crate::USB_DECIMATED_RATE)])
                {
                    let c = Config {
                        frequency,
                        bandwidth,
                        rate,
                        ..config()
                    };
                    let mut chip = Chip::new(Bus::new(), Delay);
                    chip.configure_controls_clock_with_if(
                        c,
                        Controls::default(),
                        ReferenceClock::Internal,
                        lo_below,
                        if_hz,
                    )
                    .await
                    .unwrap();
                    // LO=96_000_001 Hz: N=40, F=7, R=1, L=4.
                    assert_eq!(
                        &chip.bus.regs[0x2a..=0x34],
                        &[1, 0, 40, 0, 0, 7, 0, 0, 4, 0, 4]
                    );
                    assert_eq!(&chip.bus.regs[8..11], &c.carrier_with(lo_below, if_hz));
                    let writes = &chip.bus.writes;
                    assert_eq!(
                        writes
                            .iter()
                            .filter(|(r, _)| *r == 0x29)
                            .copied()
                            .collect::<Vec<_>>(),
                        [(0x29, 6)]
                    );
                    assert!(writes.iter().all(|&(r, v)| r != 0x2a || v == 1));
                    let coefficients = writes.iter().position(|&v| v == (0x2f, 7)).unwrap();
                    let calibration = writes.iter().position(|&v| v == (0x29, 6)).unwrap();
                    assert!(coefficients < calibration);
                }
            }
        });
    }

    #[test]
    fn precise_rx_low_band_writes_and_checks_upper_l_bits() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            let c = Config {
                frequency: 70_001,
                flags: config().flags | crate::EXPERIMENTAL_RF,
                ..config()
            };
            chip.configure_controls_clock_with_if(
                c,
                Controls::default(),
                ReferenceClock::Internal,
                false,
                120_000,
            )
            .await
            .unwrap();
            let plan = crate::rx_pll::plan(c.frequency, false, 120_000).unwrap();
            assert!(plan.l > 255);
            assert_eq!(chip.bus.regs[0x31], (plan.l >> 8) as u8);
            assert_eq!(chip.bus.regs[0x32], plan.l as u8);
            assert_eq!(&chip.bus.regs[0x31..0x33], &chip.bus.regs[0x33..0x35]);
            assert!(
                !chip
                    .bus
                    .writes
                    .iter()
                    .any(|(r, _)| [0x33, 0x34].contains(r))
            );
        });
    }

    #[test]
    fn precise_rx_rejects_invalid_lo_and_if_before_chip_io() {
        futures::executor::block_on(async {
            for (frequency, lo_below, if_hz, expected) in [
                (70_000, false, 96_000, Error::Frequency),
                (96_000, true, 96_000, Error::Frequency),
                (120_000, true, 120_000, Error::Frequency),
                (70_000, true, 120_000, Error::Frequency),
                (14_200_037, true, 96_001, Error::Profile),
                (14_200_037, false, 0, Error::Profile),
            ] {
                let mut chip = Chip::new(Bus::new(), Delay);
                let result = chip
                    .configure_recovering_clock_with_if(
                        Config {
                            frequency,
                            flags: config().flags | crate::EXPERIMENTAL_RF,
                            ..config()
                        },
                        Controls::default(),
                        ReferenceClock::Internal,
                        lo_below,
                        if_hz,
                    )
                    .await;
                assert_eq!(result, Err(expected));
                assert!(chip.bus.writes.is_empty());
                assert_eq!(chip.bus.recoveries, 0);
            }
        });
    }

    #[test]
    fn precise_rx_checks_all_pll_registers_and_active_l_after_calibration() {
        futures::executor::block_on(async {
            for register in 0x2a..=0x34 {
                let mut bus = Bus::new();
                bus.corrupt_read = Some(register);
                let mut chip = Chip::new(bus, Delay);
                assert_eq!(chip.configure(config()).await, Err(Error::Readback));
                assert!(chip.bus.writes.contains(&(0x29, 6)));
            }
        });
    }

    #[test]
    fn precise_rx_forced_lf_and_hf_routing_preserve_exact_pll_and_detect_corruption() {
        futures::executor::block_on(async {
            for (frequency, input) in [
                (474_237, crate::controls::RfInput::Hf),
                (14_200_037, crate::controls::RfInput::Lf),
            ] {
                for lo_below in [false, true] {
                    for if_hz in [96_000, 120_000] {
                        let c = Config {
                            frequency,
                            ..config()
                        };
                        let controls = Controls {
                            input,
                            ..Default::default()
                        };
                        let mut chip = Chip::new(Bus::new(), Delay);
                        chip.configure_controls_clock_with_if(
                            c,
                            controls,
                            ReferenceClock::Internal,
                            lo_below,
                            if_hz,
                        )
                        .await
                        .unwrap();
                        let plan = crate::rx_pll::plan(frequency, lo_below, if_hz).unwrap();
                        let n = u16::from_be_bytes([chip.bus.regs[0x2b], chip.bus.regs[0x2c]]);
                        let fraction = u32::from_be_bytes([
                            0,
                            chip.bus.regs[0x2d],
                            chip.bus.regs[0x2e],
                            chip.bus.regs[0x2f],
                        ]);
                        assert_eq!((n, fraction), (plan.n, plan.fraction));
                        let fc = c.carrier_with(lo_below, if_hz);
                        let routed = input.routing_carrier_with(c, fc);
                        assert_ne!(fc, routed);
                        assert_eq!(&chip.bus.regs[8..11], &routed);
                        assert_eq!(chip.bus.regs[0xa2] & 7, input.mask(frequency));
                        let writes = &chip.bus.writes;
                        let calibration = writes.iter().position(|&v| v == (0x29, 6)).unwrap();
                        assert_eq!(
                            writes[calibration + 1..calibration + 4],
                            [(8, routed[0]), (9, routed[1]), (10, routed[2]),]
                        );
                        assert!(
                            writes[calibration + 1..]
                                .iter()
                                .all(|&(r, _)| { !(0x29..=0x34).contains(&r) })
                        );
                        // Corruption occurs only during the post-calibration Fc
                        // route write, so the final verification must catch it.
                        for register in 0x2a..=0x34 {
                            let mut bus = Bus::new();
                            bus.corrupt_on_route = Some(register);
                            let mut chip = Chip::new(bus, Delay);
                            assert_eq!(
                                chip.configure_controls_clock_with_if(
                                    c,
                                    controls,
                                    ReferenceClock::Internal,
                                    lo_below,
                                    if_hz,
                                )
                                .await,
                                Err(Error::Readback)
                            );
                        }
                    }
                }
            }
        });
    }
    #[test]
    fn wide_fir_is_committed_in_order_and_restored_after_reconfigure() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            for (rate, bandwidth) in [
                (24_000, 10_000),
                (48_000, 20_000),
                (96_000, 20_000),
                (120_000, 100_000),
            ] {
                let c = Config {
                    rate,
                    bandwidth,
                    flags: 0x18b,
                    ..config()
                };
                for frequency in [14_200_000, 14_201_000] {
                    chip.bus.writes.clear();
                    chip.configure(Config { frequency, ..c }).await.unwrap();
                    let writes = &chip.bus.writes;
                    for (i, coefficient) in crate::wide_fir::coefficients(c).iter().enumerate() {
                        let [hi, lo] = coefficient.to_be_bytes();
                        assert!(
                            writes
                                .windows(3)
                                .any(|w| w == [(0xd7, 16 + i as u8), (0xd8, hi), (0xd9, lo)])
                        );
                    }
                    assert_eq!(writes.iter().filter(|(r, _)| *r == 0xd9).count(), 56);
                }
            }
        });
    }
    #[test]
    fn calibration_and_bus_failures_propagate() {
        futures::executor::block_on(async {
            let mut bus = Bus::new();
            bus.busy = true;
            assert_eq!(
                Chip::new(bus, Delay).configure(config()).await,
                Err(Error::Timeout)
            );
            let mut bus = Bus::new();
            bus.fail = true;
            assert_eq!(
                Chip::new(bus, Delay).configure(config()).await,
                Err(Error::I2c)
            );
        });
    }
    #[test]
    fn transient_bus_and_calibration_failures_reset_and_restore_requested_controls() {
        futures::executor::block_on(async {
            for bus_fault in [false, true] {
                let mut bus = Bus::new();
                bus.fail = bus_fault;
                bus.busy = !bus_fault;
                let mut chip = Chip::new(bus, Delay);
                let controls = Controls {
                    input: crate::controls::RfInput::Hf,
                    lf_mf_capacitor: 0xabc,
                    ..Default::default()
                };
                chip.configure_recovering(config(), controls).await.unwrap();
                assert_eq!(chip.bus.recoveries, 1);
                let reset = chip
                    .bus
                    .writes
                    .windows(2)
                    .position(|w| w == [(4, 1), (4, 0)])
                    .unwrap();
                assert_eq!(chip.bus.writes[reset + 2], (3, 1));
                assert_eq!(&chip.bus.regs[8..11], &config().carrier());
                assert_eq!(chip.bus.regs[0xa2] & 7, 2);
                assert_eq!(&chip.bus.regs[0x14..0x16], &[0x0a, 0xbc]);
                chip.configure_controls(config(), controls).await.unwrap();
                assert_eq!(&chip.bus.regs[0x14..0x16], &[0x0a, 0xbc]);
            }
        });
    }
    #[test]
    fn persistent_calibration_fault_retries_only_once_and_next_request_can_recover() {
        futures::executor::block_on(async {
            let mut bus = Bus::new();
            bus.busy = true;
            bus.heal = false;
            let mut chip = Chip::new(bus, Delay);
            assert_eq!(
                chip.configure_recovering(config(), Controls::default())
                    .await,
                Err(Error::Timeout)
            );
            assert_eq!(chip.bus.recoveries, 1);
            assert_eq!(
                chip.bus.writes.iter().filter(|w| **w == (0x29, 6)).count(),
                2
            );
            chip.bus.heal = true;
            chip.configure_recovering(config(), Controls::default())
                .await
                .unwrap();
            assert_eq!(chip.bus.recoveries, 2);
        });
    }
    #[test]
    fn failed_bus_clear_does_not_attempt_chip_reset_or_retry() {
        futures::executor::block_on(async {
            let mut bus = Bus::new();
            bus.fail = true;
            bus.recovery_fails = true;
            let mut chip = Chip::new(bus, Delay);
            assert_eq!(
                chip.configure_recovering(config(), Controls::default())
                    .await,
                Err(Error::I2c)
            );
            assert_eq!(chip.bus.recoveries, 1);
            assert!(chip.bus.writes.is_empty());
        });
    }
    #[test]
    fn invalid_request_never_resets_and_success_never_retries() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            assert!(
                chip.configure_recovering(
                    Config {
                        rate: 960_000,
                        ..config()
                    },
                    Controls::default()
                )
                .await
                .is_err()
            );
            assert!(
                chip.configure_recovering(
                    config(),
                    Controls {
                        if_gain: 32,
                        ..Default::default()
                    }
                )
                .await
                .is_err()
            );
            assert_eq!(chip.bus.recoveries, 0);
            assert!(chip.bus.writes.is_empty());
            chip.configure_recovering(config(), Controls::default())
                .await
                .unwrap();
            assert_eq!(chip.bus.recoveries, 0);
            assert!(!chip.bus.writes.iter().any(|w| w.0 == 4));
        });
    }
    #[test]
    fn forced_hf_routes_after_calibration_and_auto_restores_lf() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            let c = Config {
                frequency: 474_200,
                ..config()
            };
            chip.configure_controls(
                c,
                Controls {
                    input: crate::controls::RfInput::Hf,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(chip.bus.regs[0xa2] & 7, 2);
            let w = &chip.bus.writes;
            let cal = w.iter().position(|v| *v == (0x29, 6)).unwrap();
            let route = w
                .windows(3)
                .position(|v| v == [(8, 0), (9, 0x4e), (10, 0x20)])
                .unwrap();
            assert!(route > cal);
            assert_eq!(w.iter().filter(|v| v.0 == 0x29).count(), 1);
            chip.configure(c).await.unwrap();
            assert_eq!(chip.bus.regs[0xa2] & 7, 4);
            assert_eq!(&chip.bus.regs[8..11], &c.carrier());
        });
    }
    #[test]
    fn invalid_manual_gain_and_fir_never_write() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            assert!(
                chip.configure_controls(
                    config(),
                    Controls {
                        if_gain: 32,
                        ..Default::default()
                    }
                )
                .await
                .is_err()
            );
            assert!(chip.upload_fir(&[0; 40]).await.is_err());
            assert!(chip.bus.writes.is_empty());
        });
    }
    #[test]
    fn lf_manual_gain_and_attenuator_program_documented_registers() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            chip.configure_controls(
                Config {
                    frequency: 474_200,
                    ..config()
                },
                Controls {
                    input: crate::controls::RfInput::Lf,
                    rf_auto: false,
                    lf_gain: 9,
                    lf_attenuator: 4,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(chip.bus.regs[0x0c], 0x0e);
            assert_eq!(chip.bus.regs[0x13], 0x99);
            assert_eq!(chip.bus.regs[0x16], 4);
        });
    }
    #[test]
    fn invalid_configuration_never_touches_bus() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            assert!(
                chip.configure(Config {
                    rate: 960_000,
                    ..config()
                })
                .await
                .is_err()
            );
            assert!(chip.bus.writes.is_empty());
        });
    }

    #[test]
    fn crystal_trim_is_explicit_and_reset_is_released() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            for code in 0..16 {
                let config = Config {
                    flags: 2 | 8 | (code << 4),
                    ..config()
                };
                chip.configure(config).await.unwrap();
                assert_eq!(chip.bus.regs[0x97], ((code << 4) | 1) as u8);
            }
            chip.configure(config()).await.unwrap();
            assert_eq!(chip.bus.regs[0x97], 0x61);
            chip.reset().await.unwrap();
            assert!(chip.bus.writes.ends_with(&[(4, 1), (4, 0)]));
        });
    }

    #[test]
    fn status_probe_reads_signed_rssi_without_programming_registers() {
        futures::executor::block_on(async {
            let mut bus = Bus::new();
            bus.regs[0xd0] = 0x0f;
            bus.regs[0xd1] = 0xf0;
            let mut chip = Chip::new(bus, Delay);
            assert_eq!(chip.status().await, Ok((8, -16)));
            assert!(chip.bus.writes.is_empty());
        });
    }
}
