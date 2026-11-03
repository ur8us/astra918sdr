//! Astra control protocol and authoritative, host-testable receiver state.
use crate::{
    Config, EXPERIMENTAL_MAX_HZ, EXPERIMENTAL_MIN_HZ,
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
pub const REFERENCE_SET: u8 = 0x3a;
pub const GPIO_UPDATE: u8 = 0x3b;
pub const VFO_SIGN_SET: u8 = 0x3c;
pub const IF_FREQUENCY_SET: u8 = 0x3d;
pub const TX_CONFIG: u8 = 0x4a;
/// Explicitly erase both saved-settings slots and restore default settings.
pub const FACTORY_RESET: u8 = 0x3f;
pub const FEATURE_FACTORY_RESET: u8 = 2;
pub const TX_CARRIER: u8 = 0x43;
pub const TX_MEASURE: u8 = 0x44;
/// Nominal USB audio channel tuning error (synthesized minus requested), i64 µHz.
pub const RX_TUNING_ERROR: u8 = 0x45;
pub const FEATURE_RX_TUNING_ERROR: u8 = 4; // status byte 127
/// Empty query -> one byte mA; one-byte setter -> ordinary status snapshot.
pub const TX_DRIVE: u8 = 0x46;
pub const FEATURE_TX_DRIVE: u8 = 8; // status byte 127
pub const TX_SLEW: u8 = 0x47;
pub const FEATURE_TX_SLEW: u8 = 16; // status byte 127
pub const TX_SLEW_FAST: u8 = 32; // status byte 127, valid with FEATURE_TX_SLEW
pub const TX_OUTPUTS: u8 = 0x48;
pub const TX_RELAY_DELAY: u8 = 0x49;
pub const FEATURE_TX_RELAY_DELAY: u8 = 8; // status byte 120
pub const FEATURE_COMBINED_SETTINGS: u8 = 16; // status byte 120
pub const MAX_TX_RELAY_DELAY_MS: u16 = 1000;
pub const FEATURE_TX_OUTPUTS: u8 = 64; // status byte 127
pub const TX_OUTPUTS_IN_PHASE: u8 = 128; // status byte 127, valid with FEATURE_TX_OUTPUTS
/// Mask/value update: bit 0 runs continuous I/Q correction; bit 1 enables pilot calibration.
pub const IQ_COMPENSATION_SET: u8 = 0x3e;
/// Erase both saved-settings sectors and apply defaults immediately.
/// Run one internal-pilot I/Q calibration at the current RF center.
pub const IQ_CALIBRATE_NOW: u8 = 0x42;
pub const IQ_RUNNING: u8 = 1;
pub const IQ_LARGE_RETUNE: u8 = 2;
pub const FEATURE_IQ_COMPENSATION: u8 = 1;
pub const FEATURE_IQ_CALIBRATE_NOW: u8 = 4;
pub const STATE_SIZE: usize = 128;
pub const IQ_HEADER: usize = 64;
pub const IQ_SAMPLES: usize = 512;
pub const IQ_FRAME: usize = IQ_HEADER + IQ_SAMPLES * 4;
pub const RATE: u32 = 120_000;
pub const OPTIONS: u32 = 0x18b;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum ReferenceClock {
    #[default]
    Internal = 0,
    External = 1,
}
impl ReferenceClock {
    pub fn parse(raw: u8) -> Result<Self, Status> {
        match raw {
            0 => Ok(Self::Internal),
            1 => Ok(Self::External),
            _ => Err(Status::Argument),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum VfoSign {
    #[default]
    Auto = 0,
    LoAbove = 1,
    LoBelow = 2,
}
impl VfoSign {
    pub fn parse(raw: u8) -> Result<Self, Status> {
        match raw {
            0 => Ok(Self::Auto),
            1 => Ok(Self::LoAbove),
            2 => Ok(Self::LoBelow),
            _ => Err(Status::Argument),
        }
    }
    pub fn lo_below(self, frequency: u32) -> bool {
        match self {
            Self::Auto => frequency >= 2_000_000,
            Self::LoAbove => false,
            Self::LoBelow => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum IfFrequency {
    #[default]
    Auto = 0,
    Khz96 = 1,
    Khz120 = 2,
}
impl IfFrequency {
    pub fn parse(raw: u8) -> Result<Self, Status> {
        match raw {
            0 => Ok(Self::Auto),
            1 => Ok(Self::Khz96),
            2 => Ok(Self::Khz120),
            _ => Err(Status::Argument),
        }
    }
    pub fn hz(self) -> u32 {
        match self {
            Self::Auto | Self::Khz96 => 96_000,
            Self::Khz120 => 120_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub dial: u64,
    pub offset: i32,
    pub mode: Mode,
    pub low: u16,
    pub high: u16,
    pub controls: Controls,
    pub reference: ReferenceClock,
    pub gpio: u8,
    pub vfo_sign: VfoSign,
    pub if_frequency: IfFrequency,
    pub tx_enabled: bool,
    pub tx_threshold_dbfs: i8,
    pub tx_key_ms: u8,
    pub tx_unkey_ms: u8,
    pub tx_relay_delay_ms: u16,
    pub tx_drive_ma: u8,
    pub tx_slew_fast: bool,
    pub tx_differential: bool,
    pub running_iq: bool,
    pub iq_on_large_retune: bool,
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
            reference: ReferenceClock::Internal,
            gpio: 0,
            vfo_sign: VfoSign::Auto,
            if_frequency: IfFrequency::Auto,
            tx_enabled: true,
            tx_threshold_dbfs: -40,
            tx_key_ms: 10,
            tx_unkey_ms: 100,
            tx_relay_delay_ms: 50,
            tx_drive_ma: 12,
            tx_slew_fast: true,
            tx_differential: true,
            running_iq: true,
            iq_on_large_retune: false,
        }
    }
}
impl Settings {
    pub fn center(self) -> i64 {
        self.dial as i64 - i64::from(self.offset)
    }
    pub fn hardware(self) -> Config {
        Config {
            frequency: self.center() as u32,
            rate: RATE,
            bandwidth: 100_000,
            flags: OPTIONS,
        }
    }
    pub fn validate(self) -> Result<Self, Status> {
        if self.tx_relay_delay_ms > MAX_TX_RELAY_DELAY_MS {
            return Err(Status::Argument);
        }
        if !matches!(self.tx_drive_ma, 2 | 4 | 8 | 12) {
            return Err(Status::Argument);
        }
        if !(-60..=-6).contains(&self.tx_threshold_dbfs) {
            return Err(Status::Argument);
        }
        if !(u64::from(EXPERIMENTAL_MIN_HZ)..=u64::from(EXPERIMENTAL_MAX_HZ)).contains(&self.dial)
            || !(i64::from(EXPERIMENTAL_MIN_HZ)..=i64::from(EXPERIMENTAL_MAX_HZ))
                .contains(&self.center())
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
        // 500 Hz guard includes the audio filter transition.
        if bottom < -59_500 || top > 59_500 {
            return Err(Status::Bandwidth);
        }
        self.controls
            .validate(self.hardware())
            .map_err(|_| Status::Argument)?;
        let frequency = self.hardware().frequency;
        if self.vfo_sign.lo_below(frequency) && frequency <= self.if_frequency.hz() {
            return Err(Status::Argument);
        }
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
    RegisterRead(u8),
    RegisterWrite(u8, u8),
    Configure,
    Channel,
    Capacitor,
    Logical,
    IqSettings,
    CalibrateNow,
    FactoryReset,
    Clock,
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
    pub tx_active: bool,
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
    /// 0 never attempted, 1 successful, 2 pilot unavailable or incomplete.
    pub iq_cal_status: u8,
}
impl Default for Receiver {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            revision: 0,
            generation: 0,
            streaming: false,
            tx_active: false,
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
            iq_cal_status: 0,
        }
    }
}
impl Receiver {
    pub fn prepare(&self, cmd: u8, p: &[u8]) -> Result<(Settings, Action), Status> {
        if self.tx_active && matches!(cmd, IQ_COMPENSATION_SET | IQ_CALIBRATE_NOW) {
            return Err(Status::Busy);
        }
        let mut s = self.settings;
        if cmd == TX_RELAY_DELAY {
            if p.is_empty() {
                return Ok((s, Action::Query));
            }
            if p.len() != 2 {
                return Err(Status::Length);
            }
            s.tx_relay_delay_ms = u16::from_le_bytes([p[0], p[1]]);
            return Ok((s.validate()?, Action::Logical));
        }
        if cmd == TX_OUTPUTS {
            if p.is_empty() {
                return Ok((s, Action::Query));
            }
            if p.len() != 1 {
                return Err(Status::Length);
            }
            if p[0] > 1 {
                return Err(Status::Argument);
            }
            s.tx_differential = p[0] == 0;
            return Ok((s.validate()?, Action::Logical));
        }
        if cmd == TX_SLEW {
            if p.is_empty() {
                return Ok((s, Action::Query));
            }
            if p.len() != 1 {
                return Err(Status::Length);
            }
            if p[0] > 1 {
                return Err(Status::Argument);
            }
            s.tx_slew_fast = p[0] == 1;
            return Ok((s.validate()?, Action::Logical));
        }
        if cmd == TX_DRIVE {
            if p.is_empty() {
                return Ok((s, Action::Query));
            }
            if p.len() != 1 {
                return Err(Status::Length);
            }
            s.tx_drive_ma = p[0];
            return Ok((s.validate()?, Action::Logical));
        }
        if cmd == TX_CONFIG {
            if p.is_empty() {
                return Ok((s, Action::Query));
            }
            if p.len() != 4 {
                return Err(Status::Length);
            }
            if p[0] > 1 {
                return Err(Status::Argument);
            }
            s.tx_enabled = p[0] != 0;
            s.tx_threshold_dbfs = p[1] as i8;
            s.tx_key_ms = p[2];
            s.tx_unkey_ms = p[3];
            return Ok((s.validate()?, Action::Logical));
        }
        let n = match cmd {
            v2::VERSION_GET
            | v2::INFO
            | v2::CAPS
            | v2::STATUS
            | RX_TUNING_ERROR
            | v2::FREQUENCY_GET
            | v2::RATE_GET
            | v2::INPUT_GET
            | v2::GAIN_GET
            | v2::FILTER_GET
            | v2::LF_MF_CAPACITOR_GET
            | v2::START
            | v2::STOP
            | SAVE
            | FACTORY_RESET
            | IQ_CALIBRATE_NOW
            | RETRY => 0,
            v2::FREQUENCY_SET | CHANNEL_TUNE | CENTER_TUNE => 8,
            OFFSET | AUDIO_FILTER | v2::RATE_SET | v2::OPTIONS => 4,
            MODE
            | v2::INPUT_SET
            | REFERENCE_SET
            | VFO_SIGN_SET
            | IF_FREQUENCY_SET
            | v2::REGISTER_READ => 1,
            v2::GAIN_MODE
            | v2::GAIN_SET
            | v2::LF_MF_CAPACITOR_SET
            | GPIO_UPDATE
            | IQ_COMPENSATION_SET
            | v2::REGISTER_WRITE => 2,
            _ => return Err(Status::Command),
        };
        if p.len() != n {
            return Err(Status::Length);
        }
        if self.streaming && matches!(cmd, v2::REGISTER_READ | v2::REGISTER_WRITE) {
            return Err(Status::Busy);
        }
        let action = match cmd {
            v2::REGISTER_READ => Action::RegisterRead(p[0]),
            v2::REGISTER_WRITE => Action::RegisterWrite(p[0], p[1]),
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
            REFERENCE_SET => {
                s.reference = ReferenceClock::parse(p[0])?;
                Action::Clock
            }
            VFO_SIGN_SET => {
                s.vfo_sign = VfoSign::parse(p[0])?;
                Action::Configure
            }
            IF_FREQUENCY_SET => {
                s.if_frequency = IfFrequency::parse(p[0])?;
                Action::Configure
            }
            GPIO_UPDATE => {
                s.gpio = (s.gpio & !p[0]) | (p[1] & p[0]);
                Action::Logical
            }
            IQ_COMPENSATION_SET => {
                if p[0] & !3 != 0 || p[1] & !3 != 0 || p[1] & !p[0] != 0 {
                    return Err(Status::Argument);
                }
                if p[0] & IQ_RUNNING != 0 {
                    s.running_iq = p[1] & IQ_RUNNING != 0;
                }
                if p[0] & IQ_LARGE_RETUNE != 0 {
                    s.iq_on_large_retune = p[1] & IQ_LARGE_RETUNE != 0;
                }
                Action::IqSettings
            }
            FACTORY_RESET => {
                if self.tx_active {
                    return Err(Status::Busy);
                }
                s = Settings::default();
                Action::FactoryReset
            }
            IQ_CALIBRATE_NOW => {
                if !self.configured {
                    return Err(Status::Io);
                }
                Action::CalibrateNow
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
            Action::Configure
            | Action::Clock
            | Action::Retry
            | Action::Channel
            | Action::IqSettings
            | Action::CalibrateNow => {
                self.configured = true;
                self.error = 0;
                self.generation = self.generation.wrapping_add(1);
            }
            Action::FactoryReset => {
                self.configured = true;
                self.error = 0;
                self.saved_revision = u32::MAX;
                self.iq_cal_status = 0;
                self.generation = self.generation.wrapping_add(1);
            }
            Action::Capacitor => {
                self.error = 0;
            }
            Action::Logical => {}
            Action::Start => {
                self.streaming = true;
                self.generation = self.generation.wrapping_add(1);
            }
            Action::Stop => self.streaming = false,
            Action::Save => self.saved_revision = self.revision,
            Action::Query | Action::RegisterRead(_) | Action::RegisterWrite(_, _) => {}
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
        p[60..62].copy_from_slice(&s.tx_relay_delay_ms.to_le_bytes());
        p[62] = u8::from(s.tx_enabled);
        p[63] = s.tx_threshold_dbfs as u8;
        p[64..76].fill(0xff);
        p[80..88].copy_from_slice(&(s.center() as u64).to_le_bytes());
        p[88..92].copy_from_slice(&s.offset.to_le_bytes());
        p[92] = s.mode.digit() - b'0';
        p[93] = s.tx_drive_ma;
        p[94..96].copy_from_slice(&s.low.to_le_bytes());
        p[116..118].copy_from_slice(&s.high.to_le_bytes());
        p[118] = s.reference as u8;
        p[119] = s.gpio;
        p[120] = 0xe0
            | FEATURE_FACTORY_RESET
            | FEATURE_TX_RELAY_DELAY
            | FEATURE_COMBINED_SETTINGS
            | FEATURE_IQ_COMPENSATION
            | FEATURE_IQ_CALIBRATE_NOW;
        p[121] = s.vfo_sign as u8;
        p[122] = s.if_frequency as u8;
        p[123] = u8::from(s.running_iq) | (u8::from(s.iq_on_large_retune) << 1);
        p[124] = self.iq_cal_status;
        p[125] = s.tx_key_ms;
        p[126] = s.tx_unkey_ms;
        p[127] = 1
            | (u8::from(self.tx_active) << 1)
            | FEATURE_RX_TUNING_ERROR
            | FEATURE_TX_DRIVE
            | FEATURE_TX_SLEW
            | if s.tx_slew_fast { TX_SLEW_FAST } else { 0 }
            | FEATURE_TX_OUTPUTS
            | if s.tx_differential {
                0
            } else {
                TX_OUTPUTS_IN_PHASE
            };
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
            Command::FrequencyB => Reply::frequency_b(t),
            Command::Mode(None) => Reply::mode(t),
            Command::FilterWidth => Reply::filter_width(self.settings.high - self.settings.low),
            Command::LowCutoff => Reply::low_cutoff(self.settings.low),
            Command::If => Reply::information(t),
            Command::Id => Reply::literal(b"ID020;"),
            Command::Ai => Reply::literal(b"AI0;"),
            Command::Fr => Reply::literal(b"FR0;"),
            Command::Ft => Reply::literal(b"FT0;"),
            Command::Power => Reply::literal(b"PS1;"),
            Command::KeyerSpeed => Reply::literal(b"KS020;"),
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
            TX_RELAY_DELAY if raw[12..14] == [0, 0] => {
                reply(raw, status, &self.settings.tx_relay_delay_ms.to_le_bytes())
            }
            TX_DRIVE if raw[12..14] == [0, 0] => reply(raw, status, &[self.settings.tx_drive_ma]),
            TX_SLEW if raw[12..14] == [0, 0] => {
                reply(raw, status, &[u8::from(self.settings.tx_slew_fast)])
            }
            TX_OUTPUTS if raw[12..14] == [0, 0] => {
                reply(raw, status, &[u8::from(!self.settings.tx_differential)])
            }
            RX_TUNING_ERROR => match self.rx_tuning_error() {
                Ok(value) => reply(raw, status, &value.to_le_bytes()),
                Err(status) => reply(raw, status, &[]),
            },
            v2::VERSION_GET | v2::INFO => reply(raw, status, b"Astra918 0.1.0;control=1;stream=1"),
            v2::CAPS => {
                let mut p = v2::capabilities();
                p[12..16].copy_from_slice(&1u32.to_le_bytes());
                p[16..20].copy_from_slice(&1u32.to_le_bytes());
                p[24..28].fill(0);
                p[32..36].copy_from_slice(&RATE.to_le_bytes());
                reply(raw, status, &p)
            }
            _ => reply(raw, status, &self.encode()),
        }
    }
    pub fn rx_tuning_error(&self) -> Result<i64, Status> {
        if !self.configured || self.tx_active {
            return Err(Status::Busy);
        }
        let pll_error = crate::rx_pll::error_micro_hz(
            self.settings.hardware().frequency,
            self.settings
                .vfo_sign
                .lo_below(self.settings.hardware().frequency),
            self.settings.if_frequency.hz(),
        )
        .map_err(|_| Status::Argument)?;
        Ok(pll_error + crate::dsp::channel_error_micro_hz(self.settings.offset))
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
    #[test]
    fn iq_and_tx_configuration_coexist_and_iq_operations_reject_active_tx() {
        let mut r = Receiver::default();
        let (s, action) = r.prepare(IQ_COMPENSATION_SET, &[3, 2]).unwrap();
        r.commit(s, action);
        let (s, action) = r.prepare(TX_CONFIG, &[0, (-52i8) as u8, 17, 83]).unwrap();
        r.commit(s, action);
        let p = r.encode();
        assert_eq!(p[123], 2);
        assert_eq!(&p[62..64], &[0, (-52i8) as u8]);
        assert_eq!(&p[125..127], &[17, 83]);
        assert!(!r.settings.running_iq && r.settings.iq_on_large_retune);
        r.tx_active = true;
        assert_eq!(r.prepare(IQ_COMPENSATION_SET, &[3, 0]), Err(Status::Busy));
        assert_eq!(r.prepare(IQ_CALIBRATE_NOW, &[]), Err(Status::Busy));
    }
    #[test]
    fn relay_delay_is_independent_volatile_and_reported() {
        use super::*;
        let mut r = Receiver::default();
        let original = r.settings;
        assert_eq!(original.tx_relay_delay_ms, 50);
        for ms in [0u16, 50, 1000] {
            let (s, action) = r.prepare(TX_RELAY_DELAY, &ms.to_le_bytes()).unwrap();
            assert_eq!(action, Action::Logical);
            r.commit(s, action);
            assert_eq!(
                r.settings,
                Settings {
                    tx_relay_delay_ms: ms,
                    ..original
                }
            );
            assert_eq!(r.saved_revision, u32::MAX);
            assert_eq!(&r.encode()[60..62], &ms.to_le_bytes());
            assert_ne!(r.encode()[120] & FEATURE_TX_RELAY_DELAY, 0);
            let reply = r.response(&request(TX_RELAY_DELAY, 1, &[]), Status::Ok);
            assert_eq!(&reply[12..14], &2u16.to_le_bytes());
            assert_eq!(&reply[16..18], &ms.to_le_bytes());
            let (s, _) = r.prepare(TX_CONFIG, &[1, (-40i8) as u8, 20, 80]).unwrap();
            assert_eq!(s.tx_relay_delay_ms, ms);
        }
        assert_eq!(r.prepare(TX_RELAY_DELAY, &[0]), Err(Status::Length));
        assert_eq!(r.prepare(TX_RELAY_DELAY, &[0, 0, 0]), Err(Status::Length));
        assert_eq!(
            r.prepare(TX_RELAY_DELAY, &1001u16.to_le_bytes()),
            Err(Status::Argument)
        );
        assert_eq!(r.prepare(TX_RELAY_DELAY, &[]).unwrap().1, Action::Query);
        let (s, _) = r.prepare(FACTORY_RESET, &[]).unwrap();
        assert_eq!(s.tx_relay_delay_ms, 50);
    }
    #[test]
    fn tx_output_mode_query_set_and_factory_defaults() {
        use super::*;
        let mut radio = Receiver {
            tx_active: true,
            configured: true,
            streaming: true,
            ..Default::default()
        };
        let original = radio.settings;
        assert!(original.tx_differential);
        for mode in [1, 0, 1] {
            let (settings, action) = radio.prepare(TX_OUTPUTS, &[mode]).unwrap();
            assert_eq!(action, Action::Logical);
            radio.commit(settings, action);
            assert_eq!(
                radio.settings,
                Settings {
                    tx_differential: mode == 0,
                    ..original
                }
            );
            assert!(radio.tx_active && radio.configured && radio.streaming);
            assert_eq!(radio.generation, 0);
            assert_eq!(radio.saved_revision, u32::MAX);
            assert_eq!(radio.prepare(TX_OUTPUTS, &[]).unwrap().1, Action::Query);
            let query = request(TX_OUTPUTS, 1, &[]);
            assert_eq!(
                parse(&radio.response(&query, Status::Ok)).unwrap().1,
                &[mode]
            );
            let status = radio.encode();
            assert_ne!(status[127] & FEATURE_TX_OUTPUTS, 0);
            assert_eq!(status[127] & TX_OUTPUTS_IN_PHASE != 0, mode == 1);
            let setter = request(TX_OUTPUTS, 2, &[mode]);
            assert_eq!(
                parse(&radio.response(&setter, Status::Ok)).unwrap().1.len(),
                STATE_SIZE
            );
        }
        assert_eq!(radio.prepare(TX_OUTPUTS, &[2]), Err(Status::Argument));
        assert_eq!(radio.prepare(TX_OUTPUTS, &[255]), Err(Status::Argument));
        assert_eq!(radio.prepare(TX_OUTPUTS, &[0, 1]), Err(Status::Length));
        radio.tx_active = false;
        let (settings, action) = radio.prepare(FACTORY_RESET, &[]).unwrap();
        assert!(settings.tx_differential);
        radio.commit(settings, action);
        assert!(radio.settings.tx_differential);
    }
    #[test]
    fn slew_and_drive_are_independent_live_settings_with_explicit_save() {
        use super::*;
        let mut radio = Receiver {
            tx_active: true,
            streaming: true,
            ..Default::default()
        };
        let initial = radio.settings;
        for fast in [false, true] {
            let (settings, action) = radio.prepare(TX_SLEW, &[u8::from(fast)]).unwrap();
            assert_eq!(action, Action::Logical);
            radio.commit(settings, action);
            let (settings, action) = radio.prepare(TX_DRIVE, &[12]).unwrap();
            radio.commit(settings, action);
            assert_eq!(
                radio.settings,
                Settings {
                    tx_drive_ma: 12,
                    tx_slew_fast: fast,
                    ..initial
                }
            );
            assert!(radio.tx_active && radio.streaming);
            assert_eq!(radio.generation, 0);
            assert_eq!(radio.saved_revision, u32::MAX);
            let query = request(TX_SLEW, 1, &[]);
            assert_eq!(radio.prepare(TX_SLEW, &[]).unwrap().1, Action::Query);
            assert_eq!(
                parse(&radio.response(&query, Status::Ok)).unwrap().1,
                &[u8::from(fast)]
            );
            let status = radio.encode();
            assert_ne!(status[127] & FEATURE_TX_SLEW, 0);
            assert_eq!(status[127] & TX_SLEW_FAST != 0, fast);
            let setter = request(TX_SLEW, 2, &[u8::from(fast)]);
            assert_eq!(
                parse(&radio.response(&setter, Status::Ok)).unwrap().1.len(),
                STATE_SIZE
            );
        }
        assert_eq!(radio.prepare(TX_SLEW, &[2]), Err(Status::Argument));
        assert_eq!(radio.prepare(TX_SLEW, &[0, 1]), Err(Status::Length));
        radio.tx_active = false;
        let (settings, action) = radio.prepare(SAVE, &[]).unwrap();
        radio.commit(settings, action);
        assert_eq!(radio.saved_revision, radio.revision);
    }
    #[test]
    fn shared_tx_drive_command_is_validated_live_and_never_saves_implicitly() {
        use super::*;
        let mut radio = Receiver {
            configured: true,
            streaming: true,
            tx_active: true,
            ..Default::default()
        };
        let initial = radio.settings;
        for ma in [2, 4, 8, 12] {
            let (settings, action) = radio.prepare(TX_DRIVE, &[ma]).unwrap();
            assert_eq!(action, Action::Logical);
            radio.commit(settings, action);
            assert_eq!(
                radio.settings,
                Settings {
                    tx_drive_ma: ma,
                    ..initial
                }
            );
            assert!(radio.tx_active && radio.streaming);
            assert_eq!(radio.generation, 0);
            assert_eq!(radio.saved_revision, u32::MAX);
            let query = request(TX_DRIVE, 1, &[]);
            assert_eq!(parse(&radio.response(&query, Status::Ok)).unwrap().1, &[ma]);
            assert_eq!(radio.encode()[93], ma);
            assert_ne!(radio.encode()[127] & FEATURE_TX_DRIVE, 0);
            let setter = request(TX_DRIVE, 2, &[ma]);
            assert_eq!(
                parse(&radio.response(&setter, Status::Ok)).unwrap().1.len(),
                STATE_SIZE
            );
        }
        for ma in [0, 1, 3, 5, 16, 255] {
            assert_eq!(radio.prepare(TX_DRIVE, &[ma]), Err(Status::Argument));
        }
        assert_eq!(radio.prepare(TX_DRIVE, &[4, 4]), Err(Status::Length));
    }
    #[test]
    fn tuning_error_command_reports_signed_microhertz_and_invalid_rx_state() {
        use super::*;
        let mut r = Receiver {
            configured: true,
            ..Receiver::default()
        };
        r.settings.vfo_sign = VfoSign::LoAbove;
        r.settings.dial = 95_904_001;
        assert_ne!(r.encode()[127] & FEATURE_RX_TUNING_ERROR, 0);
        assert_eq!(r.prepare(RX_TUNING_ERROR, &[1]), Err(Status::Length));
        assert_eq!(
            r.prepare(RX_TUNING_ERROR, &[]),
            Ok((r.settings, Action::Query))
        );
        let request = request(RX_TUNING_ERROR, 9, &[]);
        for (dial, expected) in [(95_904_000, 0), (95_904_001, 1_358), (95_903_999, -1_358)] {
            r.settings.dial = dial;
            let reply = r.response(&request, Status::Ok);
            let (_, payload) = parse(&reply).unwrap();
            assert_eq!(payload.len(), 8);
            assert_eq!(i64::from_le_bytes(payload.try_into().unwrap()), expected);
        }
        r.settings.dial = 95_914_001;
        r.settings.offset = 10_000;
        assert_eq!(r.rx_tuning_error(), Ok(1_349));
        r.tx_active = true;
        assert_eq!(r.response(&request, Status::Ok)[6], Status::Busy as u8);
        r.tx_active = false;
        r.configured = false;
        assert_eq!(r.rx_tuning_error(), Err(Status::Busy));
        assert_eq!(r.revision, 0);
        assert_eq!(r.saved_revision, u32::MAX);
    }
    #[test]
    fn factory_reset_restores_defaults_and_rejects_active_transmit() {
        use super::*;
        let mut r = Receiver::default();
        r.settings.tx_enabled = true;
        r.settings.dial = 28_075_000;
        r.saved_revision = 0;
        assert_eq!(r.prepare(FACTORY_RESET, &[1]), Err(Status::Length));
        r.tx_active = true;
        assert_eq!(r.prepare(FACTORY_RESET, &[]), Err(Status::Busy));
        r.tx_active = false;
        let (settings, action) = r.prepare(FACTORY_RESET, &[]).unwrap();
        assert_eq!(action, Action::FactoryReset);
        r.commit(settings, action);
        assert_eq!(r.settings, Settings::default());
        assert_eq!(r.saved_revision, u32::MAX);
        assert!(r.configured);
        assert_ne!(r.encode()[120] & FEATURE_FACTORY_RESET, 0);
        assert_eq!(r.encode()[127] & 1, 1);
    }
    #[test]
    fn transmit_configuration_is_explicit_validated_and_reported() {
        use super::*;
        let mut receiver = Receiver::default();
        assert!(receiver.settings.tx_enabled);
        let (settings, action) = receiver
            .prepare(TX_CONFIG, &[1, (-42i8) as u8, 12, 110])
            .unwrap();
        assert_eq!(action, Action::Logical);
        receiver.commit(settings, action);
        assert!(receiver.settings.tx_enabled);
        assert_eq!(&receiver.encode()[62..64], &[1, (-42i8) as u8]);
        assert_eq!(&receiver.encode()[125..127], &[12, 110]);
        assert_eq!(&receiver.encode()[123..125], &[1, 0]);
        assert_eq!(receiver.saved_revision, u32::MAX);
        assert!(
            receiver
                .prepare(TX_CONFIG, &[2, (-42i8) as u8, 12, 110])
                .is_err()
        );
        assert!(
            receiver
                .prepare(TX_CONFIG, &[1, (-61i8) as u8, 12, 110])
                .is_err()
        );
        assert!(receiver.prepare(TX_CONFIG, &[1, 0]).is_err());
    }
    use super::*;

    #[test]
    fn raw_register_access_requires_stopped_iq_and_does_not_change_receiver_state() {
        let mut receiver = Receiver::default();
        let initial = receiver.settings;
        assert_eq!(
            receiver.prepare(v2::REGISTER_READ, &[0x07]),
            Ok((initial, Action::RegisterRead(0x07)))
        );
        assert_eq!(
            receiver.prepare(v2::REGISTER_WRITE, &[0x07, 0x32]),
            Ok((initial, Action::RegisterWrite(0x07, 0x32)))
        );
        assert_eq!(
            receiver.prepare(v2::REGISTER_WRITE, &[0x07]),
            Err(Status::Length)
        );
        receiver.commit(initial, Action::Start);
        assert_eq!(
            receiver.prepare(v2::REGISTER_READ, &[0x07]),
            Err(Status::Busy)
        );
        assert_eq!(
            receiver.prepare(v2::REGISTER_WRITE, &[0x07, 0x32]),
            Err(Status::Busy)
        );
        receiver.commit(initial, Action::Stop);
        receiver.commit(initial, Action::RegisterWrite(0x07, 0x32));
        assert_eq!(receiver.settings, initial);
        assert_eq!(receiver.revision, 0);
        assert_eq!(receiver.saved_revision, u32::MAX);
    }

    #[test]
    fn reference_and_gpio_are_receiver_owned_and_do_not_restart_iq() {
        let mut r = Receiver::default();
        r.commit(r.settings, Action::Configure);
        r.commit(r.settings, Action::Start);
        let generation = r.generation;
        assert_eq!(r.prepare(REFERENCE_SET, &[2]), Err(Status::Argument));
        assert_eq!(r.prepare(GPIO_UPDATE, &[1]), Err(Status::Length));
        let (s, action) = r.prepare(GPIO_UPDATE, &[0x05, 0x05]).unwrap();
        assert_eq!(action, Action::Logical);
        r.commit(s, action);
        assert_eq!(r.settings.gpio, 5);
        let (s, action) = r.prepare(GPIO_UPDATE, &[0x01, 0]).unwrap();
        r.commit(s, action);
        assert_eq!(r.settings.gpio, 4);
        assert_eq!(r.generation, generation);
        assert!(r.streaming && r.configured);
        assert_eq!(r.encode()[119], 4);
        let (s, action) = r.prepare(REFERENCE_SET, &[1]).unwrap();
        assert_eq!(action, Action::Clock);
        r.fault(6);
        r.commit(s, action);
        assert_eq!(r.encode()[118], 1);
        assert_eq!(r.encode()[120], 0xff);
        assert!(r.configured);
        assert_eq!(r.error, 0);
        assert_eq!(r.generation, generation + 1);
        assert_eq!(r.saved_revision, u32::MAX);
    }

    #[test]
    fn iq_settings_are_masked_live_state_and_only_save_commits_flash_revision() {
        let mut r = Receiver::default();
        assert_eq!(r.encode()[123], IQ_RUNNING);
        assert_eq!(r.encode()[124], 0);
        for invalid in [[4, 0], [1, 2], [0, 1]] {
            assert_eq!(
                r.prepare(IQ_COMPENSATION_SET, &invalid),
                Err(Status::Argument)
            );
        }
        assert_eq!(r.prepare(IQ_COMPENSATION_SET, &[1]), Err(Status::Length));
        let (settings, action) = r
            .prepare(
                IQ_COMPENSATION_SET,
                &[IQ_RUNNING | IQ_LARGE_RETUNE, IQ_LARGE_RETUNE],
            )
            .unwrap();
        assert_eq!(action, Action::IqSettings);
        r.commit(settings, action);
        assert!(!r.settings.running_iq);
        assert!(r.settings.iq_on_large_retune);
        assert_eq!(r.encode()[123], IQ_LARGE_RETUNE);
        assert_eq!(r.revision, 1);
        assert_eq!(r.saved_revision, u32::MAX);
        r.iq_cal_status = 2;
        assert_eq!(r.encode()[124], 2);
        r.commit(settings, Action::Save);
        assert_eq!(r.saved_revision, 1);
    }

    #[test]
    fn factory_reset_restores_defaults_and_marks_flash_empty() {
        let mut r = Receiver::default();
        let (changed, action) = r.prepare(GPIO_UPDATE, &[0xff, 0xa5]).unwrap();
        r.commit(changed, action);
        r.commit(changed, Action::Save);
        r.iq_cal_status = 2;
        r.commit(changed, Action::Start);
        assert_eq!(r.prepare(FACTORY_RESET, &[1]), Err(Status::Length));
        let (defaults, action) = r.prepare(FACTORY_RESET, &[]).unwrap();
        assert_eq!(defaults, Settings::default());
        assert_eq!(action, Action::FactoryReset);
        r.commit(defaults, action);
        assert_eq!(r.settings, Settings::default());
        assert_eq!(r.saved_revision, u32::MAX);
        assert_eq!(r.iq_cal_status, 0);
        assert!(r.configured && r.streaming);
        assert_eq!(r.encode()[120], 0xff);
    }

    #[test]
    fn explicit_iq_calibration_is_one_shot_and_does_not_change_saved_settings() {
        let mut r = Receiver::default();
        assert_eq!(r.prepare(IQ_CALIBRATE_NOW, &[]), Err(Status::Io));
        r.commit(r.settings, Action::Configure);
        r.commit(r.settings, Action::Save);
        let old_settings = r.settings;
        let old_revision = r.revision;
        let saved_revision = r.saved_revision;
        let generation = r.generation;
        assert_eq!(r.prepare(IQ_CALIBRATE_NOW, &[0]), Err(Status::Length));
        let (settings, action) = r.prepare(IQ_CALIBRATE_NOW, &[]).unwrap();
        assert_eq!(action, Action::CalibrateNow);
        r.commit(settings, action);
        r.iq_cal_status = 2;
        assert_eq!(r.settings, old_settings);
        assert_eq!(r.revision, old_revision);
        assert_eq!(r.saved_revision, saved_revision);
        assert_eq!(r.generation, generation + 1);
        assert_eq!(r.encode()[124], 2);
        assert!(!r.settings.iq_on_large_retune);
    }

    #[test]
    fn experimental_tuning_bounds_preserve_audio_offset() {
        let mut r = Receiver::default();
        let (s, action) = r.prepare(OFFSET, &1000i32.to_le_bytes()).unwrap();
        r.commit(s, action);
        let (s, action) = r
            .prepare(v2::FREQUENCY_SET, &260_000_000u64.to_le_bytes())
            .unwrap();
        r.commit(s, action);
        assert_eq!(r.settings.center(), 259_999_000);
        assert!(
            r.prepare(v2::FREQUENCY_SET, &260_000_001u64.to_le_bytes())
                .is_err()
        );
    }
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
        assert_eq!(r.settings.hardware().frequency, 7_064_049);
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
    #[test]
    fn wsjtx_queries_report_live_dial_and_valid_read_only_values() {
        let mut r = Receiver::default();
        let (settings, action) = r.prepare(CENTER_TUNE, &7_074_000u64.to_le_bytes()).unwrap();
        r.commit(settings, action);
        for (command, expected) in [
            (Command::FrequencyB, b"FB00007074000;".as_slice()),
            (Command::Ft, b"FT0;"),
            (Command::Power, b"PS1;"),
            (Command::KeyerSpeed, b"KS020;"),
            (Command::LowCutoff, b"SL02;"),
        ] {
            let reply = r.cat_reply(command);
            assert_eq!(&reply.bytes[..reply.len], expected);
            assert!(r.cat_prepare(command).unwrap().is_none());
        }
    }
}
