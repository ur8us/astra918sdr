//! Two independent flash sectors; CRC and last-written commit page reject torn saves.
use crate::{
    astra::{Receiver, ReferenceClock, STATE_SIZE, Settings},
    cat::Mode,
    controls::{Controls, RfInput},
};
pub const SECTOR: usize = 4096;
pub const PAGE: usize = 256;
pub const FLASH_SIZE: usize = 4 * 1024 * 1024;
pub const BASE: u32 = (FLASH_SIZE - 2 * SECTOR) as u32;
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for b in data {
        c ^= u32::from(*b);
        for _ in 0..8 {
            c = (c >> 1) ^ (0xedb88320u32 & (0u32.wrapping_sub(c & 1)));
        }
    }
    !c
}
pub fn record(settings: Settings, sequence: u32) -> [u8; PAGE] {
    let mut b = [0xff; PAGE];
    b[..4].copy_from_slice(b"ASNV");
    b[4..8].copy_from_slice(&2u32.to_le_bytes());
    b[8..12].copy_from_slice(&sequence.to_le_bytes());
    let r = Receiver {
        settings,
        ..Receiver::default()
    };
    b[16..16 + STATE_SIZE].copy_from_slice(&r.encode());
    let crc = crc32(&b[..252]);
    b[252..].copy_from_slice(&crc.to_le_bytes());
    b
}
pub fn commit_page(record: &[u8; PAGE]) -> [u8; PAGE] {
    let mut b = [0xff; PAGE];
    b[..4].copy_from_slice(b"DONE");
    b[4..8].copy_from_slice(&record[252..]);
    b
}
pub fn decode(b: &[u8; PAGE], commit: &[u8; PAGE]) -> Option<(Settings, u32)> {
    if &b[..4] != b"ASNV"
        || !matches!(u32::from_le_bytes(b[4..8].try_into().ok()?), 1 | 2)
        || &commit[..4] != b"DONE"
        || commit[4..8] != b[252..]
        || crc32(&b[..252]).to_le_bytes() != b[252..]
    {
        return None;
    }
    let version = u32::from_le_bytes(b[4..8].try_into().ok()?);
    let p = &b[16..];
    if p[30] > 1 || p[31] > 1 {
        return None;
    }
    let s = Settings {
        dial: u64::from_le_bytes(p[..8].try_into().ok()?),
        offset: i32::from_le_bytes(p[88..92].try_into().ok()?),
        mode: match p[92] {
            1 => Mode::Lsb,
            2 => Mode::Usb,
            _ => return None,
        },
        low: u16::from_le_bytes(p[94..96].try_into().ok()?),
        high: u16::from_le_bytes(p[116..118].try_into().ok()?),
        controls: Controls {
            input: RfInput::parse(p[28]).ok()?,
            rf_auto: p[30] == 0,
            if_auto: p[31] == 0,
            rf_gain: p[32],
            if_gain: p[33],
            lf_mf_capacitor: u16::from_le_bytes(p[76..78].try_into().ok()?),
            lf_gain: p[78],
            lf_attenuator: p[79],
        },
        reference: if version == 1 {
            ReferenceClock::Internal
        } else {
            ReferenceClock::parse(p[118]).ok()?
        },
        gpio: if version == 1 { 0 } else { p[119] },
    };
    Some((
        s.validate().ok()?,
        u32::from_le_bytes(b[8..12].try_into().ok()?),
    ))
}
pub fn newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_record_defaults_new_settings_and_new_record_round_trips() {
        let settings = Settings {
            reference: ReferenceClock::External,
            gpio: 0xa5,
            ..Settings::default()
        };
        let next = record(settings, 7);
        assert_eq!(decode(&next, &commit_page(&next)), Some((settings, 7)));
        let mut old = record(Settings::default(), 6);
        old[4..8].copy_from_slice(&1u32.to_le_bytes());
        old[118 + 16] = 0xff;
        old[119 + 16] = 0xff;
        let crc = crc32(&old[..252]);
        old[252..].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            decode(&old, &commit_page(&old)),
            Some((Settings::default(), 6))
        );
        assert!(newer(7, 6));
    }
    #[test]
    fn torn_corrupt_and_valid_saves() {
        let s = Settings::default();
        let b = record(s, 42);
        let c = commit_page(&b);
        assert_eq!(decode(&b, &c), Some((s, 42)));
        for n in 0..PAGE {
            let mut torn = [0xff; PAGE];
            torn[..n].copy_from_slice(&b[..n]);
            assert!(decode(&torn, &c).is_none());
        }
        assert!(decode(&b, &[0xff; PAGE]).is_none());
        let mut bad = b;
        bad[30] ^= 1;
        assert!(decode(&bad, &c).is_none());
        assert!(newer(0, u32::MAX));
    }
}
