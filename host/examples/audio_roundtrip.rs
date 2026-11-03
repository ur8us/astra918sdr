//! Offline PCM -> synthetic 120 ksps I/Q -> production DSP -> PCM.
use anyhow::{Result, ensure};
use astra918_firmware::{
    astra::Settings,
    cat::Mode,
    dsp::Demodulator,
    tx_monitor::{self, AudioBuffer, Monitor},
};
use std::{env, fs};
fn main() -> Result<()> {
    let args: Vec<_> = env::args().collect();
    ensure!(
        args.len() == 5 || (args.len() == 6 && args[5] == "--tx-monitor"),
        "input.wav output.wav offset USB|LSB [--tx-monitor]"
    );
    let mut bytes = fs::read(&args[1])?;
    ensure!(
        &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "WAV required"
    );
    let mut data = None;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into()?) as usize;
        ensure!(at + 8 + len <= bytes.len(), "Truncated WAV");
        if &bytes[at..at + 4] == b"fmt " {
            ensure!(
                bytes[at + 8..at + 12] == [1, 0, 1, 0]
                    && u32::from_le_bytes(bytes[at + 12..at + 16].try_into()?) == 12000
                    && bytes[at + 22..at + 24] == [16, 0],
                "Need mono PCM16 at 12kHz"
            );
        }
        if &bytes[at..at + 4] == b"data" {
            data = Some((at + 8, len));
            break;
        }
        at += 8 + len + len % 2;
    }
    let (start, len) = data.ok_or_else(|| anyhow::anyhow!("Missing samples"))?;
    let settings = Settings {
        dial: 14_074_049,
        offset: args[3].parse()?,
        mode: if args[4] == "LSB" {
            Mode::Lsb
        } else {
            Mode::Usb
        },
        ..Settings::default()
    };
    ensure!(settings.validate().is_ok(), "Invalid settings");
    let mut dsp = Demodulator::new(settings);
    if args.len() == 6 {
        monitor_roundtrip(&mut bytes[start..start + len], settings, &mut dsp);
        ensure!(dsp.clipped == 0, "Unexpected clipping");
        fs::write(&args[2], bytes)?;
        return Ok(());
    }
    let carrier = settings.dial as f64 - f64::from(settings.hardware().frequency);
    let mut sample_index = 0;
    for at in (start..start + len).step_by(2) {
        let input = f64::from(i16::from_le_bytes(bytes[at..at + 2].try_into()?));
        for _ in 0..10 {
            let phase = std::f64::consts::TAU * carrier * sample_index as f64 / 120000.;
            // Both sidebands of a real audio input are present; the channelizer
            // must select the requested sideband and retain its audio polarity.
            let (_, audio) =
                dsp.process((input * phase.cos()) as i16, (input * phase.sin()) as i16);
            if let Some(audio) = audio {
                bytes[at..at + 2].copy_from_slice(&audio.to_le_bytes());
            }
            sample_index += 1;
        }
    }
    ensure!(dsp.clipped == 0, "Unexpected clipping");
    fs::write(&args[2], bytes)?;
    Ok(())
}

fn monitor_roundtrip(bytes: &mut [u8], settings: Settings, dsp: &mut Demodulator) {
    let input: Vec<_> = bytes
        .chunks_exact(2)
        .map(|sample| i16::from_le_bytes(sample.try_into().unwrap()))
        .collect();
    let mut monitor = Monitor::default();
    let mut queue = AudioBuffer::new();
    let mut cursor = 0;
    let mut written = 0;
    while written < input.len() {
        let (available, _) = queue.available();
        for _ in available..64 {
            queue.push(&[input.get(cursor).copied().unwrap_or(0)]);
            cursor += 1;
        }
        let (available, discontinuity) = queue.available();
        let needed = monitor.prepare(512, true, available, discontinuity);
        let mut batch = [0; tx_monitor::MAX_INPUT];
        queue.take(&mut batch[..needed]);
        let mut frame = [0; 2048];
        monitor.render(
            &mut frame,
            &batch[..needed],
            tx_monitor::Config {
                enabled: true,
                offset: settings.offset,
                mode: settings.mode,
            },
        );
        for sample in frame.chunks_exact(4) {
            let i = i16::from_le_bytes(sample[..2].try_into().unwrap());
            let q = i16::from_le_bytes(sample[2..].try_into().unwrap());
            if let (_, Some(audio)) = dsp.process(i, q)
                && written < input.len()
            {
                bytes[written * 2..written * 2 + 2].copy_from_slice(&audio.to_le_bytes());
                written += 1;
            }
        }
    }
}
