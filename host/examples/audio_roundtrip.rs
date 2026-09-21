//! Offline PCM -> synthetic 120 ksps I/Q -> production DSP -> PCM.
use anyhow::{Result, ensure};
use astra918_firmware::{astra::Settings, cat::Mode, dsp::Demodulator};
use std::{env, fs};
fn main() -> Result<()> {
    let args: Vec<_> = env::args().collect();
    ensure!(args.len() == 5, "input.wav output.wav offset USB|LSB");
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
