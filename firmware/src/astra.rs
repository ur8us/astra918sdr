//! Astra control protocol and authoritative, host-testable receiver state.
use crate::{
    Config,
    cat::{Command, Mode, Reply, Tuning},
    control_v2 as v2,
    controls::Controls,
};
pub use v2::{SIZE, Status};
pub const VID: u16 = 0xc0de;
pub const PID: u16 = 0x091a;
pub const INTERFACE: u8 = 4;
pub const COMMAND_EP: u8 = 0x03;
pub const REPLY_EP: u8 = 0x84;
pub const IQ_EP: u8 = 0x85;
pub const OFFSET: u8 = 0x33;
pub const MODE: u8 = 0x34;
pub const AUDIO_FILTER: u8 = 0x35;
pub const SAVE: u8 = 0x36;
pub const RETRY: u8 = 0x37;
/// Move the shared audio/CAT channel while retaining the current RF center.
pub const CHANNEL_TUNE: u8 = 0x38;
/// Tune the RF center and audio/CAT dial together, with zero channel offset.
pub const CENTER_TUNE: u8 = 0x39;
pub const STATE_SIZE: usize = 128;
pub const IQ_HEADER: usize = 64;
pub const IQ_SAMPLES: usize = 512;
pub const IQ_FRAME: usize = IQ_HEADER + IQ_SAMPLES * 4;
pub const RATE: u32 = 120_000;
pub const OPTIONS: u32 = 0x18b;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub dial: u64,
    pub offset: i32,
    pub mode: Mode,
    pub low: u16,
    pub high: u16,
    pub controls: Controls,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            dial: 14_200_000,
            offset: 0,
            mode: Mode::Usb,
            low: 100,
            high: 3500,
            controls: Controls::default(),
        }
    }
}
impl Settings {
    pub fn center(self) -> i64 {
        self.dial as i64 - i64::from(self.offset)
    }
    pub fn hardware(self) -> Config {
        Config {
            frequency: ((self.center() + 50) / 100 * 100) as u32,
            rate: RATE,
            bandwidth: 100_000,
            flags: OPTIONS,
        }
    }
    pub fn validate(self) -> Result<Self, Status> {
        if !(70_000..=130_000_000).contains(&self.dial)
            || !(70_000..=130_000_000).contains(&self.center())
            || self.low >= self.high
            || self.high > 5000
        {
            return Err(Status::Argument);
        }
        let (bottom, top) = match self.mode {
            Mode::Usb => (
                i64::from(self.offset) + i64::from(self.low),
                i64::from(self.offset) + i64::from(self.high),
            ),
            Mode::Lsb => (
                i64::from(self.offset) - i64::from(self.high),
                i64::from(self.offset) - i64::from(self.low),
            ),
        };
        // 500 Hz guard includes the audio transition and coarse-center correction.
        if bottom < -59_500 || top > 59_500 {
            return Err(Status::Bandwidth);
        }
        self.controls
            .validate(self.hardware())
            .map_err(|_| Status::Argument)?;
        Ok(self)
    }
    pub fn tuning(self) -> Tuning {
        Tuning {
            frequency: self.dial as u32,
            mode: self.mode,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Query,
    Configure,
    Channel,
    Capacitor,
    Start,
    Stop,
    Save,
    Retry,
}
#[derive(Clone, Copy, Debug)]
pub struct Receiver {
    pub settings: Settings,
    pub revision: u32,
    pub generation: u32,
    pub streaming: bool,
    pub configured: bool,
    pub error: u32,
    pub dropped: u32,
    pub capture_faults: u32,
    pub usb_faults: u32,
    pub underruns: u32,
    pub overruns: u32,
    pub audio_stalls: u32,
    pub saved_revision: u32,
    pub chip_status: u8,
    pub rssi: i16,
}
impl Default for Receiver {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            revision: 0,
            generation: 0,
            streaming: false,
            configured: false,
            error: 0,
            dropped: 0,
            capture_faults: 0,
            usb_faults: 0,
            underruns: 0,
            overruns: 0,
            audio_stalls: 0,
            saved_revision: u32::MAX,
            chip_status: 0,
            rssi: 0,
        }
    }
}
impl Receiver {
    pub fn prepare(&self, cmd: u8, p: &[u8]) -> Result<(Settings, Action), Status> {
        let mut s = self.settings;
        let n = match cmd {
            v2::VERSION_GET
            | v2::INFO
            | v2::CAPS
            | v2::STATUS
            | v2::FREQUENCY_GET
            | v2::RATE_GET
            | v2::INPUT_GET
            | v2::GAIN_GET
            | v2::FILTER_GET
            | v2::LF_MF_CAPACITOR_GET
            | v2::START
            | v2::STOP
            | SAVE
            | RETRY => 0,
            v2::FREQUENCY_SET | CHANNEL_TUNE | CENTER_TUNE => 8,
            OFFSET | AUDIO_FILTER | v2::RATE_SET | v2::OPTIONS => 4,
            MODE | v2::INPUT_SET => 1,
            v2::GAIN_MODE | v2::GAIN_SET | v2::LF_MF_CAPACITOR_SET => 2,
            _ => return Err(Status::Command),
        };
        if p.len() != n {
            return Err(Status::Length);
        }
        let action = match cmd {
            v2::FREQUENCY_SET => {
                s.dial = u64::from_le_bytes(p.try_into().unwrap());
                Action::Configure
            }
            CHANNEL_TUNE => {
                if !self.configured {
                    return Err(Status::Io);
                }
                let dial = u64::from_le_bytes(p.try_into().unwrap());
                s.offset = i32::try_from(i128::from(dial) - i128::from(s.center()))
                    .map_err(|_| Status::Bandwidth)?;
                s.dial = dial;
                Action::Channel
            }
            CENTER_TUNE => {
                s.dial = u64::from_le_bytes(p.try_into().unwrap());
                s.offset = 0;
                Action::Configure
            }
            OFFSET => {
                s.offset = i32::from_le_bytes(p.try_into().unwrap());
                Action::Configure
            }
            MODE => {
                s.mode = match p[0] {
                    1 => Mode::Lsb,
                    2 => Mode::Usb,
                    _ => return Err(Status::Argument),
                };
                Action::Configure
            }
            AUDIO_FILTER => {
                s.low = u16::from_le_bytes([p[0], p[1]]);
                s.high = u16::from_le_bytes([p[2], p[3]]);
                Action::Configure
            }
            v2::INPUT_SET | v2::GAIN_MODE | v2::GAIN_SET | v2::LF_MF_CAPACITOR_SET => {
                let legacy = v2::Settings {
                    requested: s.center() as u64,
                    config: s.hardware(),
                    controls: s.controls,
                };
                s.controls = legacy.prepare(cmd, p, false)?.0.controls;
                if cmd == v2::LF_MF_CAPACITOR_SET {
                    Action::Capacitor
                } else {
                    Action::Configure
                }
            }
            v2::RATE_SET | v2::OPTIONS => {
                let expected = if cmd == v2::RATE_SET { RATE } else { OPTIONS };
                if u32::from_le_bytes(p.try_into().unwrap()) != expected {
                    return Err(Status::Unsupported);
                }
                Action::Query
            }
            v2::START => Action::Start,
            v2::STOP => Action::Stop,
            SAVE => Action::Save,
            RETRY => Action::Retry,
            _ => Action::Query,
        };
        Ok((s.validate()?, action))
    }
    /// Commit only after the hardware operation succeeds.
    pub fn commit(&mut self, s: Settings, action: Action) {
        if s != self.settings {
            self.settings = s;
            self.revision = self.revision.wrapping_add(1);
        }
        match action {
            Action::Configure | Action::Retry | Action::Channel => {
                self.configured = true;
                self.error = 0;
                self.generation = self.generation.wrapping_add(1);
            }
            Action::Capacitor => {
                self.error = 0;
            }
            Action::Start => {
                self.streaming = true;
                self.generation = self.generation.wrapping_add(1);
            }
            Action::Stop => self.streaming = false,
            Action::Save => self.saved_revision = self.revision,
            Action::Query => {}
        }
    }
    pub fn fault(&mut self, error: u32) {
        self.configured = false;
        self.error = error;
        self.capture_faults = self.capture_faults.saturating_add(1);
    }
    pub fn encode(&self) -> [u8; STATE_SIZE] {
        let s = self.settings;
        let mut p = [0; STATE_SIZE];
        p[..80].copy_from_slice(
            &v2::Settings {
                requested: s.dial,
                config: s.hardware(),
                controls: s.controls,
            }
            .encode(),
        );
        p[34] = self.streaming as u8;
        p[35] = self.configured as u8;
        for (o, v) in [
            (36, self.generation),
            (40, self.dropped),
            (44, self.capture_faults),
            (48, self.usb_faults),
            (52, self.error),
            (96, self.revision),
            (100, self.underruns),
            (104, self.overruns),
            (108, self.audio_stalls),
            (112, self.saved_revision),
        ] {
            p[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        p[56] = self.chip_status;
        p[57] = self.configured as u8;
        p[58..60].copy_from_slice(&self.rssi.to_le_bytes());
        p[64..76].fill(0xff);
        p[80..88].copy_from_slice(&(s.center() as u64).to_le_bytes());
        p[88..92].copy_from_slice(&s.offset.to_le_bytes());
        p[92] = s.mode.digit() - b'0';
        p[94..96].copy_from_slice(&s.low.to_le_bytes());
        p[116..118].copy_from_slice(&s.high.to_le_bytes());
        p
    }
    pub fn cat_prepare(&self, command: Command) -> Result<Option<(Settings, Action)>, Status> {
        match command {
            Command::Frequency(Some(hz)) => self
                .prepare(v2::FREQUENCY_SET, &u64::from(hz).to_le_bytes())
                .map(Some),
            Command::Mode(Some(mode)) => self.prepare(MODE, &[mode.digit() - b'0']).map(Some),
            Command::Retry => self.prepare(RETRY, &[]).map(Some),
            _ => Ok(None),
        }
    }
    pub fn cat_reply(&self, command: Command) -> Reply {
        let t = self.settings.tuning();
        match command {
            Command::Frequency(None) => Reply::frequency(t),
            Command::Mode(None) => Reply::mode(t),
            Command::FilterWidth => Reply::filter_width(self.settings.high - self.settings.low),
            Command::If => Reply::information(t),
            Command::Id => Reply::literal(b"ID020;"),
            Command::Ai => Reply::literal(b"AI0;"),
            Command::Fr => Reply::literal(b"FR0;"),
            Command::Status => Reply::status(
                self.configured,
                self.error,
                self.capture_faults,
                self.underruns,
                self.overruns,
                self.audio_stalls,
            ),
            _ => Reply::literal(b""),
        }
    }
    pub fn response(&self, raw: &[u8; SIZE], status: Status) -> [u8; SIZE] {
        if status != Status::Ok {
            return reply(raw, status, &[]);
        }
        match raw[5] {
            v2::VERSION_GET | v2::INFO => reply(raw, status, b"Astra918 0.1.0;control=1;stream=1"),
            v2::CAPS => {
                let mut p = v2::capabilities();
                p[12..16].copy_from_slice(&1u32.to_le_bytes());
                p[16..20].copy_from_slice(&1u32.to_le_bytes());
                p[24..28].fill(0);
                p[32..36].copy_from_slice(&RATE.to_le_bytes());
                p[31] &= !16;
                reply(raw, status, &p)
            }
            _ => reply(raw, status, &self.encode()),
        }
    }
    pub fn iq_header(&self, sequence: u32, first: u64) -> [u8; IQ_HEADER] {
        let mut b = [0; IQ_HEADER];
        b[..4].copy_from_slice(b"ASIQ");
        b[4] = 1;
        b[5] = 0x80;
        b[6..8].copy_from_slice(&(IQ_HEADER as u16).to_le_bytes());
        b[8..12].copy_from_slice(&self.generation.to_le_bytes());
        b[12..16].copy_from_slice(&sequence.to_le_bytes());
        b[16..24].copy_from_slice(&first.to_le_bytes());
        b[24..28].copy_from_slice(&RATE.to_le_bytes());
        b[28..30].copy_from_slice(&(IQ_SAMPLES as u16).to_le_bytes());
        b[32..40].copy_from_slice(&(self.settings.center() as u64).to_le_bytes());
        b[40..48].copy_from_slice(&self.settings.dial.to_le_bytes());
        b[48..52].copy_from_slice(&self.revision.to_le_bytes());
        b
    }
}
pub fn parse(raw: &[u8; SIZE]) -> Result<(u8, &[u8]), Status> {
    if &raw[..4] != b"AST1" || raw[4] != 1 {
        return Err(Status::Version);
    }
    let n = u16::from_le_bytes([raw[12], raw[13]]) as usize;
    if n > 240
        || raw[6..8] != [0, 0]
        || raw[14..16] != [0, 0]
        || raw[16 + n..].iter().any(|b| *b != 0)
    {
        return Err(Status::Length);
    }
    Ok((raw[5], &raw[16..16 + n]))
}
pub fn reply(raw: &[u8; SIZE], status: Status, payload: &[u8]) -> [u8; SIZE] {
    let mut b = v2::reply(raw, status, payload);
    b[..4].copy_from_slice(b"AST1");
    b[4] = 1;
    b
}
pub fn request(cmd: u8, seq: u32, payload: &[u8]) -> [u8; SIZE] {
    let mut raw = [0; SIZE];
    raw[5] = cmd;
    raw[8..12].copy_from_slice(&seq.to_le_bytes());
    reply(&raw, Status::Ok, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn channel_tuning_keeps_rf_center_and_updates_cat_atomically() {
        let mut r = Receiver::default();
        assert!(
            r.prepare(CHANNEL_TUNE, &14_201_000u64.to_le_bytes())
                .is_err()
        );
        r.commit(r.settings, Action::Configure);
        r.commit(r.settings, Action::Start);
        for mode in [Mode::Usb, Mode::Lsb] {
            r.settings.mode = mode;
            for offset in [-45_000i64, 0, 45_000] {
                let center = r.settings.center();
                let hardware = r.settings.hardware();
                let dial = (center + offset) as u64;
                let (s, action) = r.prepare(CHANNEL_TUNE, &dial.to_le_bytes()).unwrap();
                assert_eq!(action, Action::Channel);
                assert_eq!(r.settings.center(), center);
                let generation = r.generation;
                r.commit(s, action);
                assert_eq!(r.settings.center(), center);
                assert_eq!(r.settings.hardware(), hardware);
                assert_eq!(r.settings.dial, dial);
                assert_eq!(r.settings.offset, offset as i32);
                assert_eq!(r.generation, generation + 1);
                assert!(r.streaming && r.configured);
                assert_eq!(r.saved_revision, u32::MAX);
                assert_eq!(r.settings.tuning().frequency, dial as u32);
            }
        }
        let original = r.settings;
        for dial in [0, u64::MAX, 14_800_000] {
            assert!(r.prepare(CHANNEL_TUNE, &dial.to_le_bytes()).is_err());
            assert_eq!(r.settings, original);
        }
        let (s, action) = r.prepare(CENTER_TUNE, &7_074_049u64.to_le_bytes()).unwrap();
        assert_eq!(action, Action::Configure);
        r.commit(s, action);
        assert_eq!(r.settings.dial, 7_074_049);
        assert_eq!(r.settings.center(), 7_074_049);
        assert_eq!(r.settings.offset, 0);
    }
    #[test]
    fn shared_tuning_and_offset_semantics() {
        let mut r = Receiver::default();
        let (s, a) = r.prepare(OFFSET, &10_000i32.to_le_bytes()).unwrap();
        r.commit(s, a);
        assert_eq!(r.settings.dial, 14_200_000);
        assert_eq!(r.settings.center(), 14_190_000);
        let (s, a) = r
            .cat_prepare(Command::Frequency(Some(7_074_049)))
            .unwrap()
            .unwrap();
        r.commit(s, a);
        assert_eq!(r.settings.offset, 10_000);
        assert_eq!(r.settings.center(), 7_064_049);
        assert_eq!(r.settings.hardware().frequency, 7_064_000);
        assert_eq!(
            &r.cat_reply(Command::Frequency(None)).bytes[..14],
            b"FA00007074049;"
        );
        let (s, a) = r.prepare(v2::INPUT_SET, &[2]).unwrap();
        r.commit(s, a);
        assert_eq!(r.settings.dial, 7_074_049);
    }
    #[test]
    fn malformed_and_out_of_band_do_not_commit() {
        let r = Receiver::default();
        for v in [i32::MIN, i32::MAX, 59_000] {
            assert!(r.prepare(OFFSET, &v.to_le_bytes()).is_err());
        }
        assert!(r.prepare(MODE, &[3]).is_err());
        assert!(r.prepare(AUDIO_FILTER, &[0, 0, 0, 0]).is_err());
        let mut b = request(OFFSET, 1, &0i32.to_le_bytes());
        b[255] = 1;
        assert_eq!(parse(&b), Err(Status::Length));
    }
    #[test]
    fn stopping_iq_keeps_receiver_and_cat() {
        let mut r = Receiver::default();
        r.commit(r.settings, Action::Configure);
        r.commit(r.settings, Action::Start);
        r.commit(r.settings, Action::Stop);
        assert!(r.configured);
        assert!(!r.streaming);
        assert_eq!(r.cat_reply(Command::Id).len, 6);
    }
}
