//! Bounded asynchronous USB microphone packetizer. Variable 11/12/13-sample
//! packets absorb CMX918-vs-USB crystal drift without dropping normal samples.
pub const MAX_PACKET_BYTES: usize = 26;
const CAPACITY: usize = 384;
const TARGET: usize = 96;

pub struct AudioQueue {
    samples: [i16; CAPACITY],
    head: usize,
    len: usize,
    primed: bool,
    pub underruns: u32,
    pub overruns: u32,
}
impl Default for AudioQueue {
    fn default() -> Self {
        Self::new()
    }
}
impl AudioQueue {
    pub const fn new() -> Self {
        Self {
            samples: [0; CAPACITY],
            head: 0,
            len: 0,
            primed: false,
            underruns: 0,
            overruns: 0,
        }
    }
    /// Configuration/lifecycle boundaries retain counters but invalidate audio.
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.primed = false;
    }
    pub fn push(&mut self, samples: &[i16]) {
        if self.len + samples.len() > CAPACITY {
            self.overruns = self.overruns.saturating_add(1);
            self.clear();
        }
        if samples.len() > CAPACITY {
            return;
        }
        for &sample in samples {
            self.samples[(self.head + self.len) % CAPACITY] = sample;
            self.len += 1;
        }
    }
    pub fn packet(&mut self, bytes: &mut [u8; MAX_PACKET_BYTES]) -> usize {
        bytes.fill(0);
        if !self.primed {
            if self.len < TARGET {
                return 24;
            }
            self.primed = true;
        }
        let count = if self.len > TARGET + 24 {
            13
        } else if self.len < TARGET - 24 {
            11
        } else {
            12
        };
        if self.len < count {
            self.underruns = self.underruns.saturating_add(1);
            self.clear();
            return 24;
        }
        for chunk in bytes[..count * 2].chunks_exact_mut(2) {
            chunk.copy_from_slice(&self.samples[self.head].to_le_bytes());
            self.head = (self.head + 1) % CAPACITY;
            self.len -= 1;
        }
        count * 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drift_does_not_lose_or_duplicate_samples() {
        for rate in [11994u64, 12000, 12006] {
            let mut q = AudioQueue::new();
            let mut data = [0; MAX_PACKET_BYTES];
            let mut produced = 0u64;
            let mut consumed = 0u64;
            // 30 simulated seconds with 2 ms DMA blocks and +/-500 ppm clocks.
            for ms in 1..=30000 {
                let due = ms * rate / 1000;
                while produced + 24 <= due {
                    let mut block = [0i16; 24];
                    for (i, v) in block.iter_mut().enumerate() {
                        *v = ((produced + i as u64) % 30000 + 1) as i16;
                    }
                    q.push(&block);
                    produced += 24;
                }
                let len = q.packet(&mut data);
                for b in data[..len].chunks_exact(2) {
                    let v = i16::from_le_bytes([b[0], b[1]]);
                    if v != 0 {
                        assert_eq!(v, (consumed % 30000 + 1) as i16);
                        consumed += 1;
                    }
                }
            }
            assert_eq!(q.underruns, 0);
            assert_eq!(q.overruns, 0);
            assert!((48..144).contains(&(produced - consumed)));
        }
    }
    #[test]
    fn stalls_and_configuration_changes_discard_stale_audio() {
        let mut q = AudioQueue::new();
        let mut bytes = [0; MAX_PACKET_BYTES];
        q.push(&[100; 384]);
        q.push(&[200; 96]);
        assert_eq!(q.overruns, 1);
        q.packet(&mut bytes);
        assert_eq!(&bytes[..2], &200i16.to_le_bytes());
        for _ in 0..20 {
            q.packet(&mut bytes);
        }
        assert_eq!(q.underruns, 1);
        assert_eq!(bytes, [0; MAX_PACKET_BYTES]);
        q.push(&[100; 96]);
        q.clear();
        q.packet(&mut bytes);
        assert_eq!(bytes, [0; MAX_PACKET_BYTES]);
    }
}
