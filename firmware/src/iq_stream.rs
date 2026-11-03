//! Wire counters belong to USB delivery, including placeholder samples in TX.
use crate::astra::{IQ_FRAME, IQ_HEADER, IQ_SAMPLES};

#[derive(Default)]
pub struct Timeline {
    generation: Option<u32>,
    sequence: u32,
    sample: u64,
}

impl Timeline {
    /// Stamp every outgoing frame, regardless of capture versus zero producer.
    /// A generation change starts a new epoch; switching producers does not.
    pub fn stamp(&mut self, frame: &mut [u8; IQ_FRAME]) {
        let generation = u32::from_le_bytes(frame[8..12].try_into().unwrap());
        if self.generation != Some(generation) {
            self.generation = Some(generation);
            self.sequence = 0;
            self.sample = 0;
        }
        frame[12..16].copy_from_slice(&self.sequence.to_le_bytes());
        frame[16..24].copy_from_slice(&self.sample.to_le_bytes());
        self.sequence = self.sequence.wrapping_add(1);
        self.sample = self.sample.wrapping_add(IQ_SAMPLES as u64);
    }
}

pub fn zero_frame(header: &[u8; IQ_HEADER]) -> [u8; IQ_FRAME] {
    let mut frame = [0; IQ_FRAME];
    frame[..IQ_HEADER].copy_from_slice(header);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::astra::Receiver;

    #[test]
    fn rx_tx_queued_rx_and_rx_resume_share_one_wire_sequence() {
        let mut radio = Receiver {
            generation: 9,
            ..Default::default()
        };
        let mut timeline = Timeline::default();
        // Each producer supplies its own stale/reset counters. Neither may
        // reset the wire counters, even within the same generation on resume.
        for (index, zero) in [false, false, true, true, true, false, false]
            .into_iter()
            .enumerate()
        {
            let mut frame = zero_frame(&radio.iq_header(0, 0));
            if !zero {
                frame[IQ_HEADER..].fill(0x55);
            }
            let payload = frame[IQ_HEADER..].to_vec();
            timeline.stamp(&mut frame);
            assert_eq!(
                u32::from_le_bytes(frame[12..16].try_into().unwrap()),
                index as u32
            );
            assert_eq!(
                u64::from_le_bytes(frame[16..24].try_into().unwrap()),
                index as u64 * 512
            );
            assert_eq!(&frame[IQ_HEADER..], payload);
            assert_eq!(&frame[24..IQ_HEADER], &radio.iq_header(0, 0)[24..]);
        }
        radio.generation += 1;
        let mut first = zero_frame(&radio.iq_header(99, 999));
        timeline.stamp(&mut first);
        assert_eq!(&first[12..24], &[0; 12]);
        assert!(first[IQ_HEADER..].iter().all(|v| *v == 0));
        let mut rx = zero_frame(&radio.iq_header(0, 0));
        timeline.stamp(&mut rx);
        assert_eq!(u32::from_le_bytes(rx[12..16].try_into().unwrap()), 1);
        assert_eq!(u64::from_le_bytes(rx[16..24].try_into().unwrap()), 512);
    }

    #[test]
    fn wire_counters_wrap_without_a_false_generation_change() {
        let mut timeline = Timeline {
            generation: Some(1),
            sequence: u32::MAX,
            sample: u64::MAX - 511,
        };
        let radio = Receiver {
            generation: 1,
            ..Default::default()
        };
        let mut frame = zero_frame(&radio.iq_header(0, 0));
        timeline.stamp(&mut frame);
        timeline.stamp(&mut frame);
        assert_eq!(&frame[12..24], &[0; 12]);
    }
}
