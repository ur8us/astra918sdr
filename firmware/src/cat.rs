//! Bounded Kenwood-style ASCII CAT subset. Frames end with ';', not USB packets.
//! Wire reference: Kenwood TS-480 PC Control Command reference, FA/MD/IF.
use crate::{
    Config, EXPERIMENTAL_MAX_HZ, EXPERIMENTAL_MIN_HZ, EXPERIMENTAL_RF, Error, UNQUALIFIED_RATE,
    XTAL_TRIM,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Lsb,
    Usb,
}
impl Mode {
    pub fn digit(self) -> u8 {
        if self == Self::Lsb { b'1' } else { b'2' }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tuning {
    pub frequency: u32,
    pub mode: Mode,
}
impl Default for Tuning {
    fn default() -> Self {
        Self {
            frequency: 14_200_000,
            mode: Mode::Usb,
        }
    }
}
impl Tuning {
    pub fn coarse_frequency(self) -> u32 {
        ((self.frequency + 50) / 100) * 100
    }
    pub fn config(self) -> Config {
        Config {
            frequency: self.coarse_frequency(),
            rate: 24_000,
            bandwidth: 10_000,
            flags: EXPERIMENTAL_RF | UNQUALIFIED_RATE | XTAL_TRIM | (8 << 4),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Frequency(Option<u32>),
    Mode(Option<Mode>),
    If,
    Id,
    Ai,
    Fr,
    Ack,
    Status,
    Retry,
}

pub fn parse(bytes: &[u8]) -> Result<Command, Error> {
    match bytes {
        b"FA" => Ok(Command::Frequency(None)),
        b"MD" => Ok(Command::Mode(None)),
        b"MD1" => Ok(Command::Mode(Some(Mode::Lsb))),
        b"MD2" => Ok(Command::Mode(Some(Mode::Usb))),
        b"IF" => Ok(Command::If),
        b"ID" => Ok(Command::Id),
        b"AI" => Ok(Command::Ai),
        b"FR" => Ok(Command::Fr),
        b"AI0" | b"FR0" | b"RX" => Ok(Command::Ack),
        b"ZZST" => Ok(Command::Status),
        b"ZZRX" => Ok(Command::Retry),
        _ if bytes.len() == 13 && bytes.starts_with(b"FA") => {
            let mut value = 0u32;
            for &digit in &bytes[2..] {
                if !digit.is_ascii_digit() {
                    return Err(Error::Protocol);
                }
                value = value
                    .checked_mul(10)
                    .and_then(|v| v.checked_add(u32::from(digit - b'0')))
                    .ok_or(Error::Frequency)?;
            }
            if !(EXPERIMENTAL_MIN_HZ..=EXPERIMENTAL_MAX_HZ).contains(&value) {
                return Err(Error::Frequency);
            }
            Ok(Command::Frequency(Some(value)))
        }
        _ => Err(Error::Protocol),
    }
}

#[derive(Default)]
pub struct Parser {
    bytes: [u8; 32],
    used: usize,
    discard: bool,
}
impl Parser {
    /// Timeout poisons an incomplete frame until its delimiter, so a valid-looking
    /// suffix cannot accidentally tune the receiver. Reconnect uses a new parser.
    pub fn expire(&mut self) {
        if self.used != 0 {
            self.discard = true;
            self.used = 0;
        }
    }
    pub fn push(&mut self, byte: u8) -> Option<Result<Command, Error>> {
        if byte == b';' {
            let result = if self.discard {
                Err(Error::Protocol)
            } else {
                parse(&self.bytes[..self.used])
            };
            self.used = 0;
            self.discard = false;
            return Some(result);
        }
        if self.discard {
            return None;
        }
        if self.used == 0 && matches!(byte, b'\r' | b'\n') {
            return None;
        }
        if !byte.is_ascii_graphic() || self.used == self.bytes.len() {
            self.discard = true;
            self.used = 0;
            return None;
        }
        self.bytes[self.used] = byte.to_ascii_uppercase();
        self.used += 1;
        None
    }
}

#[derive(Clone, Copy)]
pub struct Reply {
    pub bytes: [u8; 64],
    pub len: usize,
}
impl Reply {
    pub fn literal(bytes: &[u8]) -> Self {
        let mut reply = Self {
            bytes: [0; 64],
            len: bytes.len(),
        };
        reply.bytes[..bytes.len()].copy_from_slice(bytes);
        reply
    }
    fn decimal(&mut self, offset: usize, width: usize, mut value: u32) {
        for i in (offset..offset + width).rev() {
            self.bytes[i] = b'0' + (value % 10) as u8;
            value /= 10;
        }
    }
    pub fn frequency(tuning: Tuning) -> Self {
        let mut r = Self::literal(b"FA00000000000;");
        r.decimal(2, 11, tuning.frequency);
        r
    }
    pub fn mode(tuning: Tuning) -> Self {
        let mut r = Self::literal(b"MD0;");
        r.bytes[2] = tuning.mode.digit();
        r
    }
    pub fn information(tuning: Tuning) -> Self {
        // TS-480 IF: 11-digit Hz, five spaces, +0000 RIT, RIT/XIT off,
        // bank/channel zero, RX, mode, VFO A, scan/split/tone off, trailing space.
        let mut r = Self::literal(b"IF00000000000     +00000000000000000 ;");
        r.decimal(2, 11, tuning.frequency);
        r.bytes[29] = tuning.mode.digit();
        r
    }
    pub fn status(
        ready: bool,
        error: u32,
        faults: u32,
        under: u32,
        over: u32,
        usb_stalls: u32,
    ) -> Self {
        let mut r = Self::literal(b"ZZST0,00,0000000000,0000000000,0000000000,0000000000;");
        r.bytes[4] = b'0' + u8::from(ready);
        r.decimal(6, 2, error);
        r.decimal(9, 10, faults);
        r.decimal(20, 10, under);
        r.decimal(31, 10, over);
        r.decimal(42, 10, usb_stalls);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_coalesced_and_lowercase() {
        let mut p = Parser::default();
        let mut n = 0;
        for b in b"fa00014200049;md1;FA;\r\nmd;" {
            if let Some(c) = p.push(*b) {
                let expected = [
                    Command::Frequency(Some(14_200_049)),
                    Command::Mode(Some(Mode::Lsb)),
                    Command::Frequency(None),
                    Command::Mode(None),
                ];
                assert_eq!(c, Ok(expected[n]));
                n += 1;
            }
        }
        assert_eq!(n, 4);
    }
    #[test]
    fn rejects_unsupported_malformed_and_overflow() {
        for b in [
            b"MD3".as_slice(),
            b"MD4",
            b"MD5",
            b"TX",
            b"FA99999999999",
            b"FA01300000001",
            b"FA00000069999",
            b"FA00014200a00",
            b"FA14200000",
            b"AI1",
        ] {
            assert!(parse(b).is_err(), "{b:?}");
        }
        for value in [70_000, 14_200_049, 130_000_000] {
            let t = Tuning {
                frequency: value,
                ..Tuning::default()
            };
            let r = Reply::frequency(t);
            assert_eq!(
                parse(&r.bytes[..r.len - 1]),
                Ok(Command::Frequency(Some(value)))
            );
            assert!(t.config().validate().is_ok());
        }
    }
    #[test]
    fn resynchronizes_only_at_delimiter() {
        let mut p = Parser::default();
        for _ in 0..100 {
            assert!(p.push(b'X').is_none());
        }
        for b in b"FA00014200000" {
            assert!(p.push(*b).is_none());
        }
        assert!(p.push(b';').unwrap().is_err());
        p.push(b'F');
        p.expire();
        for b in b"MD1" {
            p.push(*b);
        }
        assert!(p.push(b';').unwrap().is_err());
        for b in b"MD" {
            p.push(*b);
        }
        assert_eq!(p.push(b';'), Some(Ok(Command::Mode(None))));
    }
    #[test]
    fn fixed_width_replies() {
        let t = Tuning::default();
        let r = Reply::information(t);
        assert_eq!(r.len, 38);
        assert_eq!(&r.bytes[..r.len], b"IF00014200000     +00000000002000000 ;");
        let r = Reply::status(true, 9, 123, 456, 789, 101);
        assert_eq!(
            &r.bytes[..r.len],
            b"ZZST1,09,0000000123,0000000456,0000000789,0000000101;"
        );
    }
}
