//! Fixed-size control records permit bounded recovery independent of USB packets.
use crate::{
    Config,
    controls::*,
    protocol::{get_u32, put_u32},
};
pub const SIZE: usize = 256;
pub const MAGIC: [u8; 4] = *b"C9V2";
pub const VERSION: u8 = 2;
pub const VID: u16 = 0xc0de;
pub const PID: u16 = 0x0918;
pub const INTERFACE: u8 = 0;
pub const COMMAND_EP: u8 = 0x01;
pub const REPLY_EP: u8 = 0x81;
pub const IQ_EP: u8 = 0x82;
pub const MS_VENDOR_CODE: u8 = 0x20;
pub const DEVICE_GUID: &str = "{6968CA61-7B2B-4C57-A713-842D2F96C918}";
pub const VERSION_GET: u8 = 0x10;
pub const INFO: u8 = 0x11;
pub const CAPS: u8 = 0x12;
pub const STATUS: u8 = 0x13;
pub const FREQUENCY_SET: u8 = 0x20;
pub const FREQUENCY_GET: u8 = 0x21;
pub const RATE_SET: u8 = 0x22;
pub const RATE_GET: u8 = 0x23;
pub const INPUT_SET: u8 = 0x24;
pub const INPUT_GET: u8 = 0x25;
pub const GAIN_MODE: u8 = 0x26;
pub const GAIN_SET: u8 = 0x27;
pub const GAIN_GET: u8 = 0x28;
pub const FILTER_SET: u8 = 0x29;
pub const FILTER_GET: u8 = 0x2a;
pub const FIR_UPLOAD: u8 = 0x2b;
pub const OPTIONS: u8 = 0x2c;
pub const LF_MF_CAPACITOR_SET: u8 = 0x2d;
pub const LF_MF_CAPACITOR_GET: u8 = 0x2e;
pub const START: u8 = 0x30;
pub const STOP: u8 = 0x31;
pub const BOOTSEL: u8 = 0x32;
pub const REGISTER_READ: u8 = 0x40;
pub const REGISTER_WRITE: u8 = 0x41;

#[test]
fn bootsel_accepts_only_empty_payload_even_without_configuration() {
    let s = Settings::default(); // unqualified settings cannot START, but can reboot
    for running in [false, true] {
        assert_eq!(s.prepare(BOOTSEL, &[], running), Ok((s, Action::Bootsel)));
        assert_eq!(s.prepare(BOOTSEL, &[0], running), Err(Status::Length));
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Ok = 0,
    Command = 1,
    Version = 2,
    Length = 3,
    Argument = 4,
    Unsupported = 5,
    Busy = 6,
    Bandwidth = 7,
    Io = 8,
    Pll = 9,
    Internal = 10,
}
pub fn reply(raw: &[u8; SIZE], status: Status, payload: &[u8]) -> [u8; SIZE] {
    assert!(payload.len() <= SIZE - 16);
    let mut b = [0; SIZE];
    b[..4].copy_from_slice(&MAGIC);
    b[4] = VERSION;
    b[5] = raw[5];
    b[6] = status as u8;
    b[8..12].copy_from_slice(&raw[8..12]);
    b[12..14].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    b[16..16 + payload.len()].copy_from_slice(payload);
    b
}
pub fn parse(raw: &[u8; SIZE]) -> Result<(u8, &[u8]), Status> {
    if raw[..4] != MAGIC || raw[4] != VERSION {
        return Err(Status::Version);
    }
    let n = u16::from_le_bytes([raw[12], raw[13]]) as usize;
    if n > SIZE - 16
        || raw[6..8] != [0, 0]
        || raw[14..16] != [0, 0]
        || raw[16 + n..].iter().any(|v| *v != 0)
    {
        return Err(Status::Length);
    }
    Ok((raw[5], &raw[16..16 + n]))
}
pub fn max_bandwidth(rate: u32) -> Option<u32> {
    match rate {
        12000 => Some(5000),
        24000 => Some(10000),
        48000 | 96000 => Some(20000),
        120000 | 240000 => Some(100000),
        _ => None,
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub requested: u64,
    pub config: Config,
    pub controls: Controls,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            requested: 14_200_000,
            config: Config {
                frequency: 14_200_000,
                rate: 24_000,
                bandwidth: 10_000,
                flags: crate::EXPERIMENTAL_RF | crate::XTAL_TRIM | 0x80,
            },
            controls: Controls::default(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Query,
    Options,
    Configure,
    Capacitor,
    Upload,
    Start,
    Stop,
    Bootsel,
    RegisterRead,
    RegisterWrite,
}
impl Settings {
    /// Returns a proposed state. Hardware owner commits it only after success.
    pub fn prepare(self, cmd: u8, p: &[u8], running: bool) -> Result<(Self, Action), Status> {
        let expected = match cmd {
            VERSION_GET | INFO | CAPS | STATUS | FREQUENCY_GET | RATE_GET | INPUT_GET
            | GAIN_GET | FILTER_GET | LF_MF_CAPACITOR_GET | START | STOP | BOOTSEL => 0,
            FREQUENCY_SET => 8,
            RATE_SET | FILTER_SET | OPTIONS => 4,
            INPUT_SET | REGISTER_READ => 1,
            GAIN_MODE | GAIN_SET | LF_MF_CAPACITOR_SET | REGISTER_WRITE => 2,
            FIR_UPLOAD => 80,
            _ => return Err(Status::Command),
        };
        if p.len() != expected {
            return Err(Status::Length);
        }
        let mut s = self;
        let action = match cmd {
            START => Action::Start,
            STOP => Action::Stop,
            BOOTSEL => Action::Bootsel,
            REGISTER_READ => Action::RegisterRead,
            REGISTER_WRITE => Action::RegisterWrite,
            FREQUENCY_SET => {
                s.requested = u64::from_le_bytes(p.try_into().unwrap());
                s.config.frequency = round_frequency(s.requested).map_err(|_| Status::Argument)?;
                Action::Configure
            }
            RATE_SET => {
                s.config.rate = get_u32(p, 0);
                s.config.bandwidth = max_bandwidth(s.config.rate).ok_or(Status::Bandwidth)?;
                Action::Configure
            }
            INPUT_SET => {
                s.controls.input = RfInput::parse(p[0]).map_err(|_| Status::Argument)?;
                Action::Configure
            }
            GAIN_MODE => {
                if p[0] > 1 || p[1] > 1 {
                    return Err(Status::Argument);
                }
                if p[0] == 0 {
                    s.controls.rf_auto = p[1] == 0;
                } else {
                    s.controls.if_auto = p[1] == 0;
                }
                Action::Configure
            }
            GAIN_SET => {
                if p[0] > 3 {
                    return Err(Status::Argument);
                }
                if p[0] >= 2 && p[1] >= 16 {
                    return Err(Status::Argument);
                }
                if s.controls.rf_auto && p[0] != 1 {
                    return Err(Status::Argument);
                }
                if p[0] == 0 {
                    s.controls.rf_gain = p[1];
                } else if p[0] == 1 {
                    if s.controls.if_auto {
                        return Err(Status::Argument);
                    }
                    s.controls.if_gain = p[1];
                } else {
                    if s.controls.input.actual(s.config.frequency) != RfInput::Lf {
                        return Err(Status::Unsupported);
                    }
                    if p[0] == 2 {
                        s.controls.lf_gain = p[1];
                    } else {
                        s.controls.lf_attenuator = p[1];
                    }
                }
                Action::Configure
            }
            FILTER_SET => {
                s.config.bandwidth = get_u32(p, 0);
                Action::Configure
            }
            LF_MF_CAPACITOR_SET => {
                let code = u16::from_le_bytes(p.try_into().unwrap());
                if code > 4095 {
                    return Err(Status::Argument);
                }
                s.controls.lf_mf_capacitor = code;
                // $14/$15 are independent of the PLL and output framing.  This
                // bounded register update is safe while capture is running;
                // do not turn every slider tick into a full calibration cycle.
                Action::Capacitor
            }
            OPTIONS => {
                s.config.flags = get_u32(p, 0);
                Action::Options
            }
            FIR_UPLOAD => {
                let mut c = [0i16; 40];
                for (i, v) in c.iter_mut().enumerate() {
                    *v = i16::from_le_bytes([p[2 * i], p[2 * i + 1]]);
                }
                validate_fir(&c).map_err(|_| Status::Argument)?;
                if c != FIR2_DEFAULT || s.config.flags & crate::WIDE_FIR != 0 {
                    return Err(Status::Unsupported);
                }
                Action::Upload
            }
            _ => Action::Query,
        };
        if running
            && matches!(
                action,
                Action::Configure
                    | Action::Upload
                    | Action::Options
                    | Action::RegisterRead
                    | Action::RegisterWrite
            )
        {
            return Err(Status::Busy);
        }
        if !matches!(
            action,
            Action::Query
                | Action::Stop
                | Action::Bootsel
                | Action::RegisterRead
                | Action::RegisterWrite
                | Action::Capacitor
        ) {
            s.controls
                .validate(s.config)
                .map_err(|_| Status::Unsupported)?;
            let validation_config = if action == Action::Options {
                Config {
                    flags: s.config.flags | crate::UNQUALIFIED_RATE,
                    ..s.config
                }
            } else {
                s.config
            };
            validation_config.validate().map_err(|e| match e {
                crate::Error::Unqualified => Status::Unsupported,
                crate::Error::Frequency => Status::Argument,
                _ => Status::Argument,
            })?;
        }
        Ok((s, action))
    }
    /// State layout is documented in docs/PROTOCOL-V2.md; runtime fills tail.
    pub fn encode(self) -> [u8; 80] {
        let mut p = [0; 80];
        p[..8].copy_from_slice(&self.requested.to_le_bytes());
        p[8..16].copy_from_slice(&u64::from(self.config.frequency).to_le_bytes());
        put_u32(&mut p, 16, self.config.rate);
        put_u32(&mut p, 20, self.config.bandwidth);
        put_u32(&mut p, 24, self.config.flags);
        p[28] = self.controls.input as u8;
        p[29] = self.controls.input.actual(self.config.frequency) as u8;
        p[30] = u8::from(!self.controls.rf_auto);
        p[31] = u8::from(!self.controls.if_auto);
        p[32] = self.controls.rf_gain;
        p[33] = self.controls.if_gain;
        p[76..78].copy_from_slice(&self.controls.lf_mf_capacitor.to_le_bytes());
        p[78] = self.controls.lf_gain;
        p[79] = self.controls.lf_attenuator;
        p
    }
}
/// Capabilities version 1: bounds, rate masks, hardware gain tables and FIR mode.
pub fn capabilities() -> [u8; 204] {
    let mut p = [0; 204];
    put_u32(&mut p, 0, 2);
    put_u32(&mut p, 4, 70_000);
    put_u32(&mut p, 8, 130_000_000);
    put_u32(&mut p, 12, 100);
    put_u32(&mut p, 16, 0x1f);
    put_u32(&mut p, 20, 0);
    put_u32(&mut p, 24, 0x10); // candidates, qualified, previously failed
    p[28] = 15;
    p[29] = 39;
    p[30] = 32;
    // bit0: preset upload only; bit1: USB120 decimator; bit2: wide FIR option.
    // bit3: LF/MF capacitor setter/readback in state bytes 76..78.
    // bit4: raw single-register I2C access (stopped only).
    // bit5: LF/MF manual LNA/mixer and attenuator gain controls.
    p[31] = 63;
    for (i, r) in crate::RATES.iter().enumerate() {
        put_u32(&mut p, 32 + 4 * i, *r);
    }
    for (i, g) in RF_GAIN_DB10.iter().chain(IF_GAIN_DB10.iter()).enumerate() {
        p[52 + i * 2..54 + i * 2].copy_from_slice(&g.to_le_bytes());
    }
    put_u32(&mut p, 194, 40);
    p[198] = 0x0e; // manual RF applies LF, HF and VHF
    p[199] = 1; // framed I/Q format
    put_u32(&mut p, 200, 512);
    p
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capacitor_commands_validate_and_report_without_mutating_previous_state() {
        let (s, _) = Settings::default()
            .prepare(OPTIONS, &0x18bu32.to_le_bytes(), false)
            .unwrap();
        assert_eq!(capabilities()[31] & 0x18, 0x18);
        for code in 0..=4095u16 {
            let (proposed, action) = s
                .prepare(LF_MF_CAPACITOR_SET, &code.to_le_bytes(), false)
                .unwrap();
            assert_eq!(action, Action::Capacitor);
            assert_eq!(
                &proposed.encode()[76..80],
                &[code as u8, (code >> 8) as u8, 15, 15]
            );
            assert_eq!(
                proposed.prepare(LF_MF_CAPACITOR_GET, &[], true),
                Ok((proposed, Action::Query))
            );
            assert_eq!(s.controls.lf_mf_capacitor, 0);
        }
        for code in [4096u16, u16::MAX] {
            assert_eq!(
                s.prepare(LF_MF_CAPACITOR_SET, &code.to_le_bytes(), false),
                Err(Status::Argument)
            );
        }
        for p in [&[][..], &[0][..], &[0, 0, 0][..]] {
            assert_eq!(
                s.prepare(LF_MF_CAPACITOR_SET, p, false),
                Err(Status::Length)
            );
        }
        let (live, action) = s.prepare(LF_MF_CAPACITOR_SET, &[0, 0], true).unwrap();
        assert_eq!(action, Action::Capacitor);
        assert_eq!(live.controls.lf_mf_capacitor, 0);
        assert_eq!(live.config, s.config);
        assert_eq!(
            s.prepare(LF_MF_CAPACITOR_GET, &[0], false),
            Err(Status::Length)
        );
    }
    #[test]
    fn raw_commands_accept_all_addresses_without_valid_profile_only_when_stopped() {
        let s = Settings::default(); // Missing rate consent must not block diagnostics.
        for address in 0..=255 {
            assert_eq!(
                s.prepare(REGISTER_READ, &[address], false),
                Ok((s, Action::RegisterRead))
            );
            for value in [0, 0x80, 0xff] {
                assert_eq!(
                    s.prepare(REGISTER_WRITE, &[address, value], false),
                    Ok((s, Action::RegisterWrite))
                );
                assert_eq!(
                    s.prepare(REGISTER_WRITE, &[address, value], true),
                    Err(Status::Busy)
                );
            }
            assert_eq!(
                s.prepare(REGISTER_READ, &[address], true),
                Err(Status::Busy)
            );
        }
        for (cmd, n) in [(REGISTER_READ, 1), (REGISTER_WRITE, 2)] {
            for size in 0..=3 {
                if size != n {
                    assert_eq!(s.prepare(cmd, &[0; 3][..size], false), Err(Status::Length));
                }
            }
        }
    }
    #[test]
    fn malformed_and_transactional() {
        let mut b = reply(&[0; SIZE], Status::Ok, &[]);
        b[5] = STATUS;
        assert!(parse(&b).is_ok());
        b[12..14].copy_from_slice(&241u16.to_le_bytes());
        assert_eq!(parse(&b), Err(Status::Length));
        let s = Settings::default();
        assert_eq!(s.prepare(0xff, &[], false), Err(Status::Command));
        assert_eq!(s.prepare(START, &[], false), Err(Status::Unsupported));
        let (s, _) = s
            .prepare(OPTIONS, &(s.config.flags | 2).to_le_bytes(), false)
            .unwrap();
        assert_eq!(
            s.prepare(FREQUENCY_SET, &70_049u64.to_le_bytes(), true),
            Err(Status::Busy)
        );
        let (t, _) = s
            .prepare(FREQUENCY_SET, &70_049u64.to_le_bytes(), false)
            .unwrap();
        assert_eq!(t.config.frequency, 70_000);
        assert_eq!(s.requested, 14_200_000);
        assert_eq!(
            s.prepare(RATE_SET, &480_000u32.to_le_bytes(), false),
            Err(Status::Bandwidth)
        );
        assert_eq!(s.prepare(GAIN_SET, &[0, 10], false), Err(Status::Argument));
        assert_eq!(
            s.prepare(FIR_UPLOAD, &[0; 80], false),
            Err(Status::Unsupported)
        );
    }
    #[test]
    fn frequency_rate_input_matrix_and_opt_out() {
        let base = Settings {
            config: Config {
                flags: 0x8f,
                ..Settings::default().config
            },
            ..Settings::default()
        };
        for hz in [
            70_000u64,
            77_000,
            100_000,
            150_000,
            474_200,
            1_900_000,
            2_000_000,
            2_100_000,
            14_074_000,
            39_999_900,
            40_000_000,
            70_000_000,
            130_000_000,
        ] {
            let (t, _) = base
                .prepare(FREQUENCY_SET, &hz.to_le_bytes(), false)
                .unwrap();
            for input in 0..4 {
                let (t, _) = t.prepare(INPUT_SET, &[input], false).unwrap();
                for (bw, rate) in crate::PROFILES {
                    let (r, _) = t.prepare(RATE_SET, &rate.to_le_bytes(), false).unwrap();
                    let (r, _) = r.prepare(FILTER_SET, &bw.to_le_bytes(), false).unwrap();
                    assert_eq!(r.config.rate, rate);
                    assert_eq!(r.config.bandwidth, bw);
                    assert_eq!(r.prepare(START, &[], false).unwrap().1, Action::Start);
                }
                let (r, _) = t
                    .prepare(RATE_SET, &crate::USB_DECIMATED_RATE.to_le_bytes(), false)
                    .unwrap();
                assert_eq!(r.config.rate, crate::USB_DECIMATED_RATE);
                assert_eq!(r.config.bandwidth, 100_000);
                assert_eq!(r.prepare(START, &[], false).unwrap().1, Action::Start);
            }
        }
        let (disabled, action) = base
            .prepare(OPTIONS, &0x89u32.to_le_bytes(), false)
            .unwrap();
        assert_eq!(action, Action::Options);
        assert_eq!(
            disabled.prepare(START, &[], false),
            Err(Status::Unsupported)
        );
        for cmd in [
            INPUT_SET,
            GAIN_MODE,
            GAIN_SET,
            RATE_SET,
            FREQUENCY_SET,
            FIR_UPLOAD,
        ] {
            assert_eq!(base.prepare(cmd, &[], false), Err(Status::Length));
        }
    }
    #[test]
    fn manual_rf_settings_survive_lf_selection_and_tuning() {
        let manual = Settings {
            config: Config {
                flags: 0x8f,
                ..Settings::default().config
            },
            controls: Controls {
                input: RfInput::Hf,
                rf_auto: false,
                ..Controls::default()
            },
            ..Settings::default()
        };
        let (lf, action) = manual
            .prepare(INPUT_SET, &[RfInput::Lf as u8], false)
            .unwrap();
        assert_eq!(action, Action::Configure);
        assert_eq!(lf.controls.input, RfInput::Lf);
        assert!(!lf.controls.rf_auto);
        let auto_manual = Settings {
            config: Config {
                flags: 0x8f,
                ..Settings::default().config
            },
            controls: Controls {
                input: RfInput::Auto,
                rf_auto: false,
                ..Controls::default()
            },
            ..Settings::default()
        };
        let (tuned, action) = auto_manual
            .prepare(FREQUENCY_SET, &982_300u64.to_le_bytes(), false)
            .unwrap();
        assert_eq!(action, Action::Configure);
        assert!(!tuned.controls.rf_auto);
        assert_eq!(
            tuned.controls.input.actual(tuned.config.frequency),
            RfInput::Lf
        );
    }
}

#[cfg(test)]
#[test]
fn wide_fir_option_is_advertised_and_cannot_be_overridden_by_upload() {
    assert_eq!(capabilities()[31] & 4, 4);
    let (state, _) = Settings::default()
        .prepare(OPTIONS, &0x18bu32.to_le_bytes(), false)
        .unwrap();
    let mut payload = [0; 80];
    for (i, coefficient) in FIR2_DEFAULT.iter().enumerate() {
        payload[i * 2..i * 2 + 2].copy_from_slice(&coefficient.to_le_bytes());
    }
    assert_eq!(
        state.prepare(FIR_UPLOAD, &payload, false),
        Err(Status::Unsupported)
    );
    assert!(
        state
            .prepare(OPTIONS, &0x38bu32.to_le_bytes(), false)
            .is_err()
    );
}
