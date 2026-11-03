//! Portable vendor connection. One owner serializes control; IQ has its own endpoint/socket.
use anyhow::{Context, Result, bail, ensure};
use astra918_firmware::{astra as a, control_v2 as v2};
pub use astra918_firmware::{
    astra::{IfFrequency, Receiver, ReferenceClock, Settings, VfoSign},
    cat::Mode,
    controls::{Controls, RfInput},
};
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};
pub enum Wire {
    Tcp(TcpStream),
    Usb(rusb::DeviceHandle<rusb::Context>),
}
pub struct Client {
    wire: Wire,
    sequence: u32,
    usable: bool,
}
#[derive(Clone, Debug)]
pub struct Device {
    pub serial: String,
    pub label: String,
}
fn status_reason(status: u8) -> &'static str {
    match status {
        1 => "unknown command",
        2 => "protocol version mismatch",
        3 => "invalid payload length",
        4 => "invalid argument",
        5 => "unsupported by this firmware",
        6 => "receiver busy or experiment has sealed commands",
        7 => "bandwidth unavailable",
        8 => {
            "receiver hardware I/O failed or timed out; a register write may still have taken effect"
        }
        9 => "PLL did not lock",
        10 => "internal receiver error",
        _ => "unknown status",
    }
}
pub fn devices() -> Result<Vec<Device>> {
    use rusb::UsbContext;
    let context = rusb::Context::new()?;
    let mut out = Vec::new();
    for dev in context.devices()?.iter() {
        let desc = dev.device_descriptor()?;
        if desc.vendor_id() != a::VID || desc.product_id() != a::PID {
            continue;
        }
        if let Ok(handle) = dev.open()
            && let Ok(serial) = handle.read_serial_number_string_ascii(&desc)
        {
            out.push(Device {
                label: format!("Astra918 {serial}"),
                serial,
            });
        }
    }
    Ok(out)
}
impl Client {
    pub fn tcp(address: &str) -> Result<Self> {
        let wire = TcpStream::connect(address).context("Connect simulator")?;
        wire.set_read_timeout(Some(Duration::from_secs(10)))?;
        wire.set_write_timeout(Some(Duration::from_secs(10)))?;
        Ok(Self {
            wire: Wire::Tcp(wire),
            sequence: 0,
            usable: true,
        })
    }
    pub fn usb(serial: &str) -> Result<Self> {
        use rusb::UsbContext;
        let context = rusb::Context::new()?;
        for dev in context.devices()?.iter() {
            let desc = dev.device_descriptor()?;
            if desc.vendor_id() != a::VID || desc.product_id() != a::PID {
                continue;
            }
            let handle = dev.open()?;
            if handle.read_serial_number_string_ascii(&desc)? != serial {
                continue;
            }
            ensure!(
                handle.active_configuration()? == 1,
                "Receiver is not configured; reconnect USB"
            );
            handle
                .claim_interface(a::INTERFACE)
                .context("Vendor interface busy or inaccessible; close the other controller")?;
            // An interrupted owner can leave a partial AST1 reply queued on
            // the IN endpoint. Drain it before starting this process's
            // sequence at 1, otherwise every new CLI invocation reads the
            // stale bytes and reports a mismatched reply.
            let mut stale = [0u8; 256];
            let mut drained = false;
            for _ in 0..16 {
                match handle.read_bulk(a::REPLY_EP, &mut stale, Duration::from_millis(20)) {
                    Err(rusb::Error::Timeout) => {
                        drained = true;
                        break;
                    }
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) => {
                        let _ = handle.release_interface(a::INTERFACE);
                        return Err(error).context("Drain stale Astra918 USB replies");
                    }
                }
            }
            if !drained {
                let _ = handle.release_interface(a::INTERFACE);
                bail!("Astra918 USB reply queue did not drain; reconnect USB");
            }
            return Ok(Self {
                wire: Wire::Usb(handle),
                sequence: 0,
                usable: true,
            });
        }
        bail!("Receiver serial not found")
    }
    pub fn command(&mut self, cmd: u8, p: &[u8]) -> Result<Vec<u8>> {
        ensure!(self.usable, "Transport lost framing; reconnect");
        ensure!(p.len() <= 240, "Command payload too large");
        self.usable = false;
        self.sequence = self.sequence.wrapping_add(1);
        let raw = a::request(cmd, self.sequence, p);
        let mut out = [0; 256];
        match &mut self.wire {
            Wire::Tcp(s) => {
                s.write_all(&raw)?;
                s.read_exact(&mut out)?;
            }
            Wire::Usb(s) => {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                let mut used = 0;
                while used < 256 {
                    ensure!(
                        std::time::Instant::now() < deadline,
                        "USB write deadline expired"
                    );
                    let n = s.write_bulk(
                        a::COMMAND_EP,
                        &raw[used..],
                        deadline.saturating_duration_since(std::time::Instant::now()),
                    )?;
                    ensure!(n > 0, "No USB write progress");
                    used += n;
                }
                used = 0;
                while used < 256 {
                    ensure!(
                        std::time::Instant::now() < deadline,
                        "USB read deadline expired"
                    );
                    let n = s.read_bulk(
                        a::REPLY_EP,
                        &mut out[used..],
                        deadline.saturating_duration_since(std::time::Instant::now()),
                    )?;
                    ensure!(n > 0, "No USB read progress");
                    used += n;
                }
            }
        }
        ensure!(
            &out[..4] == b"AST1"
                && out[4] == 1
                && out[5] == cmd
                && out[8..12] == self.sequence.to_le_bytes(),
            "Mismatched reply; reconnect"
        );
        let status = out[6];
        out[6] = 0;
        let (_, payload) = a::parse(&out).map_err(|e| anyhow::anyhow!("Malformed reply: {e:?}"))?;
        self.usable = true;
        ensure!(
            status == 0,
            "Receiver rejected command: status {status} ({}, command 0x{cmd:02X})",
            status_reason(status)
        );
        Ok(payload.to_vec())
    }
    pub fn state(&mut self) -> Result<Receiver> {
        self.state_with_features().map(|(receiver, _)| receiver)
    }
    /// Read the authoritative receiver state and its feature flags atomically.
    pub fn state_with_features(&mut self) -> Result<(Receiver, u8)> {
        let payload = self.command(v2::STATUS, &[])?;
        let receiver = decode(&payload)?;
        Ok((receiver, payload[120]))
    }
    /// Nominal synthesized USB audio channel minus requested channel, in µHz.
    pub fn rx_tuning_error(&mut self) -> Result<i64> {
        let payload = self.command(a::RX_TUNING_ERROR, &[])?;
        ensure!(payload.len() == 8, "Invalid tuning-error reply length");
        Ok(i64::from_le_bytes(payload[..].try_into()?))
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        if let Wire::Usb(s) = &self.wire {
            let _ = s.release_interface(a::INTERFACE);
        }
    }
}
pub fn decode(p: &[u8]) -> Result<Receiver> {
    ensure!(p.len() == a::STATE_SIZE, "Invalid state length");
    let combined = p[120] & a::FEATURE_COMBINED_SETTINGS != 0;
    let has_tx = p[127] & 1 != 0;
    let has_iq = p[120] & a::FEATURE_IQ_COMPENSATION != 0 && (combined || !has_tx);
    let (enable, threshold) = if combined { (62, 63) } else { (123, 124) };
    ensure!(!has_tx || p[enable] <= 1, "Invalid TX enabled flag");
    let u32at = |o| u32::from_le_bytes(p[o..o + 4].try_into().unwrap());
    let settings = Settings {
        dial: u64::from_le_bytes(p[..8].try_into()?),
        offset: i32::from_le_bytes(p[88..92].try_into()?),
        mode: match p[92] {
            1 => Mode::Lsb,
            2 => Mode::Usb,
            _ => bail!("Invalid mode"),
        },
        low: u16::from_le_bytes(p[94..96].try_into()?),
        high: u16::from_le_bytes(p[116..118].try_into()?),
        controls: Controls {
            input: RfInput::parse(p[28]).map_err(|_| anyhow::anyhow!("Invalid input"))?,
            rf_auto: p[30] == 0,
            if_auto: p[31] == 0,
            rf_gain: p[32],
            if_gain: p[33],
            lf_mf_capacitor: u16::from_le_bytes(p[76..78].try_into()?),
            lf_gain: p[78],
            lf_attenuator: p[79],
        },
        reference: ReferenceClock::parse(p[118])
            .map_err(|_| anyhow::anyhow!("Invalid reference clock"))?,
        gpio: p[119],
        vfo_sign: if p[120] & 0x20 != 0 {
            VfoSign::parse(p[121]).map_err(|_| anyhow::anyhow!("Invalid VFO sign"))?
        } else {
            VfoSign::Auto
        },
        if_frequency: if p[120] & 0x20 != 0 {
            IfFrequency::parse(p[122]).map_err(|_| anyhow::anyhow!("Invalid IF frequency"))?
        } else {
            IfFrequency::Auto
        },
        tx_enabled: has_tx && p[enable] == 1,
        tx_threshold_dbfs: if has_tx { p[threshold] as i8 } else { -40 },
        tx_key_ms: if p[127] & 1 != 0 { p[125] } else { 10 },
        tx_unkey_ms: if p[127] & 1 != 0 { p[126] } else { 100 },
        tx_relay_delay_ms: if p[120] & a::FEATURE_TX_RELAY_DELAY != 0 {
            u16::from_le_bytes([p[60], p[61]])
        } else {
            50
        },
        tx_drive_ma: if p[127] & a::FEATURE_TX_DRIVE != 0 {
            p[93]
        } else {
            4
        },
        tx_slew_fast: p[127] & a::FEATURE_TX_SLEW == 0 || p[127] & a::TX_SLEW_FAST != 0,
        tx_differential: p[127] & a::FEATURE_TX_OUTPUTS == 0
            || p[127] & a::TX_OUTPUTS_IN_PHASE == 0,
        running_iq: if has_iq { p[123] & 1 != 0 } else { true },
        iq_on_large_retune: has_iq && p[123] & 2 != 0,
    };
    settings
        .validate()
        .map_err(|e| anyhow::anyhow!("Invalid settings {e:?}"))?;
    ensure!(
        p[30] <= 1 && p[31] <= 1 && p[34] <= 1 && p[35] <= 1,
        "Invalid state flags"
    );
    ensure!(!has_iq || p[123] & !3 == 0, "Invalid I/Q settings");
    ensure!(!has_iq || p[124] <= 2, "Invalid I/Q calibration status");
    Ok(Receiver {
        settings,
        revision: u32at(96),
        generation: u32at(36),
        streaming: p[34] != 0,
        tx_active: p[127] & 2 != 0,
        configured: p[35] != 0,
        error: u32at(52),
        dropped: u32at(40),
        capture_faults: u32at(44),
        usb_faults: u32at(48),
        underruns: u32at(100),
        overruns: u32at(104),
        audio_stalls: u32at(108),
        saved_revision: u32at(112),
        chip_status: p[56],
        rssi: i16::from_le_bytes(p[58..60].try_into()?),
        iq_cal_status: if has_iq { p[124] } else { 0 },
    })
}
