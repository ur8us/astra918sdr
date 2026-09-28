//! CMX918 I2C control. Sources: D/918/2.0 pp.20–31,56–60; UM918/2.0
//! registers 03,08–0B,28–2A,5A–5C,B7–BA,D0–D1,D7–D9.
//! Uses documented defaults rather than replaying captured readbacks/poll counts.

use crate::{Config, Error};
use crate::{astra::ReferenceClock, controls::Controls};
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
}

impl<I: I2c, D: DelayNs> Chip<I, D> {
    pub fn new(bus: I, delay: D) -> Self {
        Self { bus, delay }
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
        config.validate()?;
        controls.validate(config)?;
        match self
            .configure_controls_clock(config, controls, reference)
            .await
        {
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
                let result = self
                    .configure_controls_clock(config, controls, reference)
                    .await;
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
        let config = config.validate()?;
        let controls = controls.validate(config)?;
        // Stop capture before entering this routine. Standby quiesces processing
        // even if a pre-existing stream's frame-synchronous mute cannot finish.
        self.write(0x03, 1).await?;
        // CLK_CTL bit 0 selects crystal/TCXO; preserve the clock output settings.
        self.update(0x07, 1, reference as u8).await?;
        self.write(0x97, config.xtal_control()).await?;
        self.delay.delay_ms(10).await;
        self.write(0x28, config.output_control(true)).await?;
        self.write(0x2a, 3).await?; // internal PLL, automatic divider, fractional DSM
        self.write(0x30, 0).await?; // reference divider /1; known baseline
        // Reset manual output-clock overrides; choose one frame per sample.
        self.write(0x5a, 0).await?; // documented 20 clocks/channel, 40/frame
        self.write(0x5b, 0x19).await?; // single frame, manual ratio x1
        self.write(0x5c, 0).await?; // no manual master-clock division
        self.update(0xb7, 0x80, 0).await?; // direct SCLK, sample on rising edge
        self.write(0xb8, 0x64).await?; // documented running I/Q compensation
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
        let fc = config.carrier();
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
        let high_band = config.frequency > 130_000_000;
        if high_band {
            // Above the normal range, auto calculation can select L=3. This
            // silicon cannot use L=3, so program the documented manual PLL
            // formula with L=4 and a 96 kHz low-side IF instead.
            let lo = u64::from(config.frequency - 96_000);
            let scaled = (lo * 16 * (1 << 24) + 19_200_000) / 38_400_000;
            let n = (scaled >> 24) as u16;
            let f = (scaled & 0x00ff_ffff) as u32;
            self.write(0x2a, 1).await?; // manual L, fractional DSM
            for (reg, value) in [
                (0x2b, (n >> 8) as u8),
                (0x2c, n as u8),
                (0x2d, (f >> 16) as u8),
                (0x2e, (f >> 8) as u8),
                (0x2f, f as u8),
                (0x31, 0),
                (0x32, 4),
            ] {
                self.write(reg, value).await?;
            }
        }
        self.write(0x03, 3).await?;
        self.write(0x29, if high_band { 6 } else { 5 }).await?;
        // Normal tuning: IF filter and automatic PLL/VCO calibration.
        // High band: IF filter and VCO sub-band selection for manual L=4.
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
        if high_band {
            let divider =
                u16::from(self.read(0x33).await?) << 8 | u16::from(self.read(0x34).await?);
            if divider != 4 {
                return Err(Error::Timeout);
            }
        }
        // Ported from cmx918audiocat 5e4d7d9 (MIT). DS p.28: writing Fc
        // alone does not execute PLL calculation. Preserve the calibrated PLL.
        let routed_fc = controls.input.routing_carrier(config);
        if routed_fc != fc {
            let mut pll = [0; 11];
            for (i, value) in pll.iter_mut().enumerate() {
                *value = self.read(0x2a + i as u8).await?;
            }
            for (i, value) in routed_fc.iter().enumerate() {
                self.write(8 + i as u8, *value).await?;
            }
            for (i, value) in pll.iter().enumerate() {
                if self.read(0x2a + i as u8).await? != *value {
                    return Err(Error::Readback);
                }
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
    // The normal configuration path and its automatic calibration/SAS
    // algorithm must remain the production path; do not enable this while
    // streaming. UM918/2.0 pp.58–60: clear PLL_SAS_MD in $95, then select
    // minimum VCO capacitance / band cell 0 in $92. This manually sets PLL L,
    // R, N and F without writing AUTO_EXE, which would recalculate them. A
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
                            }
                            if reg == 0x28 {
                                self.regs[0x60] = u8::from(value & 8 != 0);
                            }
                            if reg == 0x32 && self.regs[0x2a] & 2 == 0 {
                                self.regs[0x33] = self.regs[0x31];
                                self.regs[0x34] = value;
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
    fn high_band_programs_manual_l_four() {
        futures::executor::block_on(async {
            let mut chip = Chip::new(Bus::new(), Delay);
            let high = Config {
                frequency: 170_000_000,
                flags: config().flags | crate::EXPERIMENTAL_RF,
                ..config()
            };
            chip.configure(high).await.unwrap();
            assert_eq!(chip.bus.regs[0x2a], 1);
            assert_eq!(chip.bus.regs[0x32], 4);
            assert_eq!(chip.bus.regs[0x34], 4);
            assert!(chip.bus.writes.contains(&(0x29, 6)));
            let scaled =
                (u64::from(high.frequency - 96_000) * 16 * (1 << 24) + 19_200_000) / 38_400_000;
            assert_eq!(chip.bus.regs[0x2c], (scaled >> 24) as u8);
            assert_eq!(chip.bus.regs[0x2f], scaled as u8);
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
                chip.bus.writes.iter().filter(|w| **w == (0x29, 5)).count(),
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
            let cal = w.iter().position(|v| *v == (0x29, 5)).unwrap();
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
