//! USB manual-carrier requests and nominal PLL readback, independent of I/O.
use crate::{astra::Status, tx_pll};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carrier {
    Stop,
    Rf(u64),
    Ldiv(u64),
}
impl Carrier {
    pub fn parse(payload: &[u8]) -> Result<Self, Status> {
        if payload.len() != 9 {
            return Err(Status::Length);
        }
        let hz = u64::from_le_bytes(payload[1..9].try_into().unwrap());
        match (payload[0], hz) {
            (0, 0) => Ok(Self::Stop),
            (1, 1..=316_000_000) => Ok(Self::Rf(hz)),
            (2, 240_000_000..=316_000_000) => Ok(Self::Ldiv(hz)),
            _ => Err(Status::Argument),
        }
    }
}

pub fn report(mode: u8, plan: tx_pll::Plan) -> [u8; 40] {
    let mut bytes = [0; 40];
    bytes[0] = 1;
    bytes[1] = mode;
    bytes[2..4].copy_from_slice(&plan.divider.to_le_bytes());
    bytes[4..6].copy_from_slice(&u16::from(plan.l).to_le_bytes());
    bytes[6..8].copy_from_slice(&plan.n.to_le_bytes());
    bytes[8..12].copy_from_slice(&plan.fraction.to_le_bytes());
    let scaled = (u128::from(plan.n) << 24) + u128::from(plan.fraction);
    let vco = (38_400_000_000u128 * scaled + (1 << 23)) >> 24;
    let ldiv = vco / u128::from(plan.l);
    let rf = ldiv / u128::from(plan.divider);
    for (offset, value) in [(16, rf), (24, ldiv), (32, vco)] {
        bytes[offset..offset + 8].copy_from_slice(&(value as u64).to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_manual_requests_and_encodes_pll_report() {
        assert_eq!(Carrier::parse(&[]), Err(Status::Length));
        assert_eq!(Carrier::parse(&[0; 9]), Ok(Carrier::Stop));
        for (mode, hz, valid) in [
            (1, 14_070_000u64, true),
            (2, 250_000_000, true),
            (2, 270_000_000, true),
            (2, 300_000_000, true),
            (2, 1, false),
            (0, 1, false),
            (3, 1, false),
        ] {
            let mut payload = [0; 9];
            payload[0] = mode;
            payload[1..].copy_from_slice(&hz.to_le_bytes());
            assert_eq!(Carrier::parse(&payload).is_ok(), valid);
        }
        let p = tx_pll::plan(270_000_000).unwrap();
        let b = report(2, p);
        assert_eq!(&b[..4], &[1, 2, 1, 0]);
        assert_eq!(
            u64::from_le_bytes(b[24..32].try_into().unwrap()),
            270_000_000_000
        );
        assert_eq!(
            u64::from_le_bytes(b[32..40].try_into().unwrap()),
            1_620_000_000_000
        );
    }
}
