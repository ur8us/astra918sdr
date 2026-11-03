//! Two independent flash sectors; CRC and last-written commit page reject torn saves.
use crate::{
    astra::{IfFrequency, Receiver, ReferenceClock, STATE_SIZE, Settings, VfoSign},
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
    b[4..8].copy_from_slice(&9u32.to_le_bytes());
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
        || !matches!(u32::from_le_bytes(b[4..8].try_into().ok()?), 1..=9)
        || &commit[..4] != b"DONE"
        || commit[4..8] != b[252..]
        || crc32(&b[..252]).to_le_bytes() != b[252..]
    {
        return None;
    }
    let version = u32::from_le_bytes(b[4..8].try_into().ok()?);
    let p = &b[16..];
    // The receiver and TX branches both used version 4 with different payloads.
    let has_tx = version >= 4 && p[127] & 1 != 0;
    let has_iq = version >= 4
        && p[120] & crate::astra::FEATURE_IQ_COMPENSATION != 0
        && (version >= 9 || !has_tx);
    let (enable, threshold) = if version >= 9 { (62, 63) } else { (123, 124) };
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
        vfo_sign: if version < 3 {
            VfoSign::Auto
        } else {
            VfoSign::parse(p[121]).ok()?
        },
        if_frequency: if version < 3 {
            IfFrequency::Auto
        } else {
            IfFrequency::parse(p[122]).ok()?
        },
        tx_enabled: if has_tx {
            p[enable] == 1
        } else {
            Settings::default().tx_enabled
        },
        tx_threshold_dbfs: if has_tx { p[threshold] as i8 } else { -40 },
        tx_key_ms: if has_tx { p[125] } else { 10 },
        tx_unkey_ms: if has_tx { p[126] } else { 100 },
        tx_relay_delay_ms: if version >= 8 {
            u16::from_le_bytes([p[60], p[61]])
        } else {
            50
        },
        tx_drive_ma: if version >= 5 {
            p[93]
        } else {
            Settings::default().tx_drive_ma
        },
        tx_slew_fast: version < 6 || p[127] & crate::astra::TX_SLEW_FAST != 0,
        tx_differential: version < 7 || p[127] & crate::astra::TX_OUTPUTS_IN_PHASE == 0,
        running_iq: !has_iq || p[123] & 1 != 0,
        iq_on_large_retune: has_iq && p[123] & 2 != 0,
    };
    if (has_iq && p[123] & !3 != 0) || (has_tx && p[enable] > 1) {
        return None;
    }
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
    fn iq_and_tx_version_four_records_migrate_independently() {
        let settings = Settings {
            running_iq: false,
            iq_on_large_retune: true,
            tx_enabled: false,
            tx_threshold_dbfs: -52,
            tx_drive_ma: 8,
            tx_key_ms: 27,
            tx_unkey_ms: 83,
            tx_relay_delay_ms: 91,
            tx_slew_fast: false,
            tx_differential: false,
            ..Settings::default()
        };
        let bytes = record(settings, 14);
        assert_eq!(decode(&bytes, &commit_page(&bytes)), Some((settings, 14)));
        let mut iq_v4 = bytes;
        iq_v4[4..8].copy_from_slice(&4u32.to_le_bytes());
        iq_v4[16 + 120] = 0xe7;
        iq_v4[16 + 127] = 0;
        let crc = crc32(&iq_v4[..252]);
        iq_v4[252..].copy_from_slice(&crc.to_le_bytes());
        let restored = decode(&iq_v4, &commit_page(&iq_v4)).unwrap().0;
        assert!(!restored.running_iq && restored.iq_on_large_retune);
        assert_eq!(restored.tx_threshold_dbfs, -40);
        assert_eq!(restored.tx_relay_delay_ms, 50);
        for version in 4..=8 {
            let old = legacy_tx_record(settings, version);
            let restored = decode(&old, &commit_page(&old)).unwrap().0;
            assert!(!restored.tx_enabled);
            assert_eq!(restored.tx_threshold_dbfs, -52);
            assert_eq!(restored.tx_key_ms, 27);
            assert_eq!(restored.tx_unkey_ms, 83);
            assert!(restored.running_iq && !restored.iq_on_large_retune);
            assert_eq!(
                restored.tx_relay_delay_ms,
                if version >= 8 { 91 } else { 50 }
            );
        }
    }
    fn legacy_tx_record(settings: Settings, version: u32) -> [u8; PAGE] {
        let mut bytes = record(settings, 14);
        bytes[4..8].copy_from_slice(&version.to_le_bytes());
        bytes[16 + 120] &= !(crate::astra::FEATURE_COMBINED_SETTINGS
            | crate::astra::FEATURE_IQ_COMPENSATION
            | crate::astra::FEATURE_IQ_CALIBRATE_NOW);
        bytes[16 + 123] = u8::from(settings.tx_enabled);
        bytes[16 + 124] = settings.tx_threshold_dbfs as u8;
        let crc = crc32(&bytes[..252]);
        bytes[252..].copy_from_slice(&crc.to_le_bytes());
        bytes
    }
    #[test]
    fn relay_delay_saved_and_old_records_default_to_fifty_ms() {
        let s = Settings {
            tx_relay_delay_ms: 137,
            ..Settings::default()
        };
        let bytes = record(s, 14);
        assert_eq!(decode(&bytes, &commit_page(&bytes)), Some((s, 14)));
        for version in 1u32..=7 {
            let mut old = legacy_tx_record(s, version);
            old[4..8].copy_from_slice(&version.to_le_bytes());
            old[16 + 60..16 + 62].fill(255);
            let crc = crc32(&old[..252]);
            old[252..].copy_from_slice(&crc.to_le_bytes());
            let (restored, _) = decode(&old, &commit_page(&old)).unwrap();
            assert_eq!(restored.tx_relay_delay_ms, 50);
        }
    }
    #[test]
    fn output_mode_round_trip_and_legacy_migration_preserve_other_tx_settings() {
        for differential in [false, true] {
            let settings = Settings {
                tx_differential: differential,
                tx_drive_ma: 12,
                tx_slew_fast: false,
                ..Default::default()
            };
            let bytes = record(settings, 13);
            assert_eq!(decode(&bytes, &commit_page(&bytes)), Some((settings, 13)));
            for version in 1u32..=6 {
                let mut old = legacy_tx_record(settings, version);
                old[4..8].copy_from_slice(&version.to_le_bytes());
                // This was previously unassigned and must not select In-phase.
                old[16 + 127] |= crate::astra::TX_OUTPUTS_IN_PHASE;
                let crc = crc32(&old[..252]);
                old[252..].copy_from_slice(&crc.to_le_bytes());
                let (restored, _) = decode(&old, &commit_page(&old)).unwrap();
                assert!(restored.tx_differential);
                assert_eq!(restored.tx_drive_ma, 12);
                assert_eq!(restored.tx_slew_fast, version < 6);
            }
        }
    }
    #[test]
    fn slew_round_trips_and_older_records_retain_fast_default() {
        for fast in [false, true] {
            for ma in [2, 4, 8, 12] {
                let settings = Settings {
                    tx_slew_fast: fast,
                    tx_drive_ma: ma,
                    ..Default::default()
                };
                let bytes = record(settings, 12);
                assert_eq!(decode(&bytes, &commit_page(&bytes)), Some((settings, 12)));
                for version in 1u32..=5 {
                    let mut old = legacy_tx_record(settings, version);
                    old[4..8].copy_from_slice(&version.to_le_bytes());
                    old[16 + 127] &= !crate::astra::TX_SLEW_FAST;
                    let crc = crc32(&old[..252]);
                    old[252..].copy_from_slice(&crc.to_le_bytes());
                    let (decoded, _) = decode(&old, &commit_page(&old)).unwrap();
                    assert!(decoded.tx_slew_fast);
                    assert_eq!(decoded.tx_drive_ma, if version >= 5 { ma } else { 12 });
                }
            }
        }
    }
    #[test]
    fn saved_drive_strength_round_trips_and_legacy_records_default_to_twelve_ma() {
        assert_eq!(Settings::default().tx_drive_ma, 12);
        for tx_drive_ma in [2, 4, 8, 12] {
            let settings = Settings {
                tx_drive_ma,
                ..Default::default()
            };
            let bytes = record(settings, 42);
            assert_eq!(decode(&bytes, &commit_page(&bytes)), Some((settings, 42)));
            for version in 1u32..=4 {
                let mut old = legacy_tx_record(settings, version);
                old[4..8].copy_from_slice(&version.to_le_bytes());
                old[16 + 93] = 0; // Was reserved, not a saved drive strength.
                let crc = crc32(&old[..252]);
                old[252..].copy_from_slice(&crc.to_le_bytes());
                assert_eq!(decode(&old, &commit_page(&old)).unwrap().0.tx_drive_ma, 12);
            }
        }
        let mut invalid = record(Settings::default(), 1);
        invalid[16 + 93] = 3;
        let crc = crc32(&invalid[..252]);
        invalid[252..].copy_from_slice(&crc.to_le_bytes());
        assert!(decode(&invalid, &commit_page(&invalid)).is_none());
    }
    #[test]
    fn legacy_record_defaults_new_settings_and_new_record_round_trips() {
        let settings = Settings {
            reference: ReferenceClock::External,
            gpio: 0xa5,
            vfo_sign: VfoSign::LoAbove,
            if_frequency: IfFrequency::Khz120,
            running_iq: false,
            iq_on_large_retune: true,
            ..Settings::default()
        };
        let next = record(settings, 7);
        assert_eq!(decode(&next, &commit_page(&next)), Some((settings, 7)));
        let mut v3 = next;
        v3[4..8].copy_from_slice(&3u32.to_le_bytes());
        let crc = crc32(&v3[..252]);
        v3[252..].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            decode(&v3, &commit_page(&v3)),
            Some((
                Settings {
                    running_iq: true,
                    iq_on_large_retune: false,
                    ..settings
                },
                7
            ))
        );
        let mut v2 = next;
        v2[4..8].copy_from_slice(&2u32.to_le_bytes());
        let crc = crc32(&v2[..252]);
        v2[252..].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            decode(&v2, &commit_page(&v2)),
            Some((
                Settings {
                    vfo_sign: VfoSign::Auto,
                    if_frequency: IfFrequency::Auto,
                    running_iq: true,
                    iq_on_large_retune: false,
                    ..settings
                },
                7
            ))
        );
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
        let mut bad_flags = next;
        bad_flags[16 + 123] = 4;
        let crc = crc32(&bad_flags[..252]);
        bad_flags[252..].copy_from_slice(&crc.to_le_bytes());
        assert!(decode(&bad_flags, &commit_page(&bad_flags)).is_none());
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
