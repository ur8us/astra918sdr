use crate::{Config, Error};

pub const MAGIC: [u8; 4] = *b"C918";
pub const VERSION: u8 = 1;
pub const REQUEST_LEN: usize = 32;
pub const REPLY_LEN: usize = 64;
pub const HEADER_LEN: usize = 32;
pub const SAMPLES: usize = 512;
pub const BLOCK_BYTES: usize = SAMPLES * 4;
pub const FLAG_USB_DECIMATED_BY_2: u32 = 1 << 1;
pub const FLAG_SYNTHETIC: u32 = 1 << 2;
pub const GET_STATUS: u8 = 1;
pub const CONFIGURE: u8 = 2;
pub const START: u8 = 3;
pub const STOP: u8 = 4;
pub const PROBE: u8 = 5;
pub const RESET: u8 = 6;

pub fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
pub fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub opcode: u8,
    pub id: u32,
    pub config: Config,
}
impl Request {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != REQUEST_LEN
            || bytes[..4] != MAGIC
            || bytes[4] != VERSION
            || !(1..=6).contains(&bytes[5])
            || bytes[6..8] != [0, 0]
            || bytes[28..32] != [0; 4]
        {
            return Err(Error::Protocol);
        }
        if bytes[5] != CONFIGURE && bytes[12..28] != [0; 16] {
            return Err(Error::Protocol);
        }
        Ok(Self {
            opcode: bytes[5],
            id: get_u32(bytes, 8),
            config: Config {
                frequency: get_u32(bytes, 12),
                rate: get_u32(bytes, 16),
                bandwidth: get_u32(bytes, 20),
                flags: get_u32(bytes, 24),
            },
        })
    }
}

pub struct Block {
    pub generation: u32,
    pub sequence: u32,
    pub first_sample: u64,
    pub flags: u32,
    pub data: [u8; BLOCK_BYTES],
}
impl Block {
    pub fn header(&self, rate: u32) -> [u8; HEADER_LEN] {
        let mut h = [0; HEADER_LEN];
        h[..4].copy_from_slice(&MAGIC);
        h[4] = VERSION;
        h[5] = 0x80;
        h[6] = HEADER_LEN as u8;
        put_u32(&mut h, 8, self.generation);
        put_u32(&mut h, 12, self.sequence);
        h[16..24].copy_from_slice(&self.first_sample.to_le_bytes());
        put_u32(&mut h, 24, rate);
        h[28..30].copy_from_slice(&(SAMPLES as u16).to_le_bytes());
        h[30..32].copy_from_slice(&(self.flags as u16).to_le_bytes());
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_requests_are_rejected() {
        for len in 0..64 {
            if len != 32 {
                assert!(Request::parse(&[0; 64][..len]).is_err());
            }
        }
        let mut p = [0; 32];
        p[..4].copy_from_slice(&MAGIC);
        p[4] = 1;
        p[5] = 1;
        assert!(Request::parse(&p).is_ok());
        for i in [4, 5, 6, 7, 12, 28, 31] {
            let mut bad = p;
            bad[i] = 255;
            assert!(Request::parse(&bad).is_err());
        }
    }
    #[test]
    fn sample_header_has_explicit_offsets() {
        let block = Block {
            generation: 7,
            sequence: 8,
            first_sample: 512,
            flags: FLAG_SYNTHETIC,
            data: [0; BLOCK_BYTES],
        };
        let h = block.header(48000);
        assert_eq!(&h[..8], b"C918\x01\x80\x20\x00");
        assert_eq!(get_u32(&h, 8), 7);
        assert_eq!(get_u32(&h, 24), 48000);
        assert_eq!(&h[28..], [0, 2, 4, 0]);
    }

    #[test]
    fn host_configuration_golden_packet() {
        let raw = *b"C918\x01\x02\x00\x00\x11\x00\x00\x00\xc0\xac\xd8\x00\x80\xbb\x00\x00\x10\x27\x00\x00\x02\x00\x00\x00\x00\x00\x00\x00";
        let request = Request::parse(&raw).unwrap();
        assert_eq!(request.id, 17);
        assert_eq!(
            request.config,
            Config {
                frequency: 14_200_000,
                rate: 48_000,
                bandwidth: 10_000,
                flags: 2
            }
        );
    }

    #[test]
    fn probe_and_reset_accept_only_an_empty_payload() {
        let mut raw = [0; REQUEST_LEN];
        raw[..4].copy_from_slice(&MAGIC);
        raw[4] = VERSION;
        for opcode in [PROBE, RESET] {
            raw[5] = opcode;
            raw[12] = 0;
            assert_eq!(Request::parse(&raw).unwrap().opcode, opcode);
            raw[12] = 1;
            assert!(Request::parse(&raw).is_err());
        }
    }
}
