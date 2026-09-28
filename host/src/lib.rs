//! Portable vendor connection. One owner serializes control; IQ has its own endpoint/socket.
use anyhow::{Context, Result, bail, ensure};
use astra918_firmware::{astra as a, control_v2 as v2};
pub use astra918_firmware::{
    astra::{Receiver, ReferenceClock, Settings},
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
        wire.set_read_timeout(Some(Duration::from_secs(5)))?;
        wire.set_write_timeout(Some(Duration::from_secs(5)))?;
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
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
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
        ensure!(status == 0, "Receiver rejected command: status {status}");
        Ok(payload.to_vec())
    }
    pub fn state(&mut self) -> Result<Receiver> {
        decode(&self.command(v2::STATUS, &[])?)
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
    };
    settings
        .validate()
        .map_err(|e| anyhow::anyhow!("Invalid settings {e:?}"))?;
    ensure!(
        p[30] <= 1 && p[31] <= 1 && p[34] <= 1 && p[35] <= 1,
        "Invalid state flags"
    );
    Ok(Receiver {
        settings,
        revision: u32at(96),
        generation: u32at(36),
        streaming: p[34] != 0,
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
    })
}
