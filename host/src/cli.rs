use anyhow::{Result, ensure};
use astra918_firmware::{astra as a, control_v2 as v2};
use astra918_host::{Client, devices};
use clap::{Parser, Subcommand};
#[derive(Parser)]
struct Args {
    #[arg(long, conflicts_with = "serial")]
    simulator: Option<String>,
    #[arg(long)]
    serial: Option<String>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    List,
    Status,
    /// Nominal USB audio tuning error, actual minus requested (not crystal error).
    TuningError,
    /// Read or set both RF outputs' drive strengths in mA (2, 4, 8, 12).
    TxDrive {
        ma: Option<u8>,
    },
    /// Read or set the PTT-to-RF and RF-to-PTT relay delay (0..1000 ms).
    TxRelayDelay {
        #[arg(value_parser = clap::value_parser!(u16).range(0..=1000))]
        ms: Option<u16>,
    },
    /// Read or set both RF outputs' slew rates (slow, fast).
    TxSlew {
        #[arg(value_parser=["slow", "fast"])]
        rate: Option<String>,
    },
    /// Read or set RF outputs (one RF output inverted or matching the other).
    TxOutputs {
        #[arg(value_parser=["differential", "in-phase"])]
        mode: Option<String>,
    },
    /// Read or set synthetic I/Q monitoring of USB playback during TX.
    TxIqMonitor {
        #[arg(value_parser=["on", "off"])]
        enabled: Option<String>,
    },
    TxConfig {
        #[arg(value_parser=["on", "off"])]
        enabled: String,
        #[arg(long, default_value_t = -40, allow_hyphen_values = true)]
        threshold: i8,
        #[arg(long, default_value_t = 10)]
        key_ms: u8,
        #[arg(long, default_value_t = 100)]
        unkey_ms: u8,
    },
    RegisterRead {
        #[arg(value_parser = parse_byte)]
        register: u8,
    },
    RegisterWrite {
        #[arg(value_parser = parse_byte)]
        register: u8,
        #[arg(value_parser = parse_byte)]
        value: u8,
        /// Read back a stable register after writing; do not use for self-clearing commands.
        #[arg(long)]
        verify: bool,
    },
    Tune {
        hz: u64,
    },
    Offset {
        #[arg(allow_hyphen_values = true)]
        hz: i32,
    },
    Mode {
        #[arg(value_parser=["usb","lsb"])]
        mode: String,
    },
    Filter {
        low: u16,
        high: u16,
    },
    Input {
        #[arg(value_parser=["auto","lf","hf","vhf"])]
        input: String,
    },
    GainMode {
        #[arg(value_parser=["rf","if"])]
        block: String,
        #[arg(value_parser=["auto","manual"])]
        mode: String,
    },
    Gain {
        block: u8,
        code: u8,
    },
    Capacitor {
        code: u16,
    },
    Reference {
        #[arg(value_parser=["internal","external"])]
        source: String,
    },
    Gpio {
        index: u8,
        value: u8,
    },
    IqCompensation {
        #[arg(value_parser=["running", "large-retune"])]
        kind: String,
        #[arg(value_parser=["on", "off"])]
        state: String,
    },
    /// Run one internal-pilot I/Q calibration at the current receiver tune.
    CalibrateIqNow,
    Save,
    Retry,
    Start,
    Stop,
}

fn parse_byte(text: &str) -> Result<u8, String> {
    let (digits, radix) =
        if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            (hex, 16)
        } else if let Some(binary) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
            (binary, 2)
        } else {
            (text, 10)
        };
    u8::from_str_radix(digits, radix)
        .map_err(|_| format!("expected a byte (0..255), decimal, 0x hex, or 0b binary: {text}"))
}

fn main() -> Result<()> {
    let args = Args::parse();
    if matches!(args.command, Command::List) {
        for d in devices()? {
            println!("{}", d.label);
        }
        return Ok(());
    }
    let register_operation = match &args.command {
        Command::RegisterRead { register } => Some((*register, None, false)),
        Command::RegisterWrite {
            register,
            value,
            verify,
        } => Some((*register, Some(*value), *verify)),
        _ => None,
    };
    let mut c = if let Some(addr) = args.simulator {
        Client::tcp(&addr)?
    } else {
        let serial = match args.serial {
            Some(serial) => serial,
            None => match devices()?.as_slice() {
                [device] => device.serial.clone(),
                [] => anyhow::bail!("No Astra918 receiver found"),
                _ => anyhow::bail!("Multiple receivers found; specify --serial"),
            },
        };
        Client::usb(&serial)?
    };
    let (cmd, p) = match args.command {
        Command::TxConfig {
            enabled,
            threshold,
            key_ms,
            unkey_ms,
        } => (
            a::TX_CONFIG,
            vec![u8::from(enabled == "on"), threshold as u8, key_ms, unkey_ms],
        ),
        Command::TuningError => (a::RX_TUNING_ERROR, vec![]),
        Command::TxDrive { ma } => (a::TX_DRIVE, ma.into_iter().collect()),
        Command::TxIqMonitor { enabled } => (
            a::TX_IQ_MONITOR,
            enabled
                .map(|value| u8::from(value == "on"))
                .into_iter()
                .collect(),
        ),
        Command::TxRelayDelay { ms } => (
            a::TX_RELAY_DELAY,
            ms.map(|ms| ms.to_le_bytes().to_vec()).unwrap_or_default(),
        ),
        Command::TxSlew { rate } => (
            a::TX_SLEW,
            rate.map(|r| u8::from(r == "fast")).into_iter().collect(),
        ),
        Command::TxOutputs { mode } => (
            a::TX_OUTPUTS,
            mode.map(|m| u8::from(m == "in-phase"))
                .into_iter()
                .collect(),
        ),
        Command::RegisterRead { register } => (v2::REGISTER_READ, vec![register]),
        Command::RegisterWrite {
            register, value, ..
        } => (v2::REGISTER_WRITE, vec![register, value]),
        Command::Tune { hz } => (v2::FREQUENCY_SET, hz.to_le_bytes().to_vec()),
        Command::Offset { hz } => (a::OFFSET, hz.to_le_bytes().to_vec()),
        Command::Mode { mode } => (a::MODE, vec![if mode == "usb" { 2 } else { 1 }]),
        Command::Filter { low, high } => {
            let mut p = low.to_le_bytes().to_vec();
            p.extend_from_slice(&high.to_le_bytes());
            (a::AUDIO_FILTER, p)
        }
        Command::Input { input } => (
            v2::INPUT_SET,
            vec![
                ["auto", "lf", "hf", "vhf"]
                    .iter()
                    .position(|x| *x == input)
                    .unwrap() as u8,
            ],
        ),
        Command::GainMode { block, mode } => (
            v2::GAIN_MODE,
            vec![(block == "if") as u8, (mode == "manual") as u8],
        ),
        Command::Gain { block, code } => (v2::GAIN_SET, vec![block, code]),
        Command::Capacitor { code } => (v2::LF_MF_CAPACITOR_SET, code.to_le_bytes().to_vec()),
        Command::Reference { source } => (a::REFERENCE_SET, vec![u8::from(source == "external")]),
        Command::Gpio { index, value } => {
            anyhow::ensure!(
                index < 8 && value <= 1,
                "GPIO needs index 0..7 and value 0 or 1"
            );
            let mask = 1u8 << index;
            (
                a::GPIO_UPDATE,
                vec![mask, if value == 1 { mask } else { 0 }],
            )
        }
        Command::IqCompensation { kind, state } => {
            let bit = if kind == "running" {
                a::IQ_RUNNING
            } else {
                a::IQ_LARGE_RETUNE
            };
            (
                a::IQ_COMPENSATION_SET,
                vec![bit, if state == "on" { bit } else { 0 }],
            )
        }
        Command::CalibrateIqNow => (a::IQ_CALIBRATE_NOW, vec![]),
        Command::Save => (a::SAVE, vec![]),
        Command::Retry => (a::RETRY, vec![]),
        Command::Start => (v2::START, vec![]),
        Command::Stop => (v2::STOP, vec![]),
        _ => (v2::STATUS, vec![]),
    };
    let response = match c.command(cmd, &p) {
        Ok(response) => response,
        Err(error)
            if register_operation
                .is_some_and(|(_, written, verify)| written.is_some() && verify)
                && error
                    .to_string()
                    .contains("Receiver rejected command: status 8") =>
        {
            eprintln!(
                "Register write reported receiver I/O failure; checking readback before deciding whether it latched."
            );
            Vec::new()
        }
        Err(error) => return Err(error),
    };
    if cmd == a::TX_IQ_MONITOR && p.is_empty() {
        ensure!(
            response.len() == 1 && response[0] <= 1,
            "Invalid TX I/Q monitor reply"
        );
        println!(
            "Synthesize I/Q during transmit {}",
            if response[0] == 1 { "on" } else { "off" }
        );
    } else if cmd == a::TX_DRIVE && p.is_empty() {
        ensure!(
            response.len() == 1 && matches!(response[0], 2 | 4 | 8 | 12),
            "Invalid drive-strength reply"
        );
        println!("TX outputs drive strength: {} mA", response[0]);
    } else if cmd == a::TX_SLEW && p.is_empty() {
        ensure!(
            response.len() == 1 && response[0] <= 1,
            "Invalid slew-rate reply"
        );
        println!(
            "TX outputs edge rate: {}",
            if response[0] == 1 { "fast" } else { "slow" }
        );
    } else if cmd == a::TX_OUTPUTS && p.is_empty() {
        ensure!(
            response.len() == 1 && response[0] <= 1,
            "Invalid TX outputs reply"
        );
        println!(
            "TX outputs: {}",
            if response[0] == 0 {
                "Differential"
            } else {
                "In-phase"
            }
        );
    } else if cmd == a::TX_RELAY_DELAY && p.is_empty() {
        ensure!(response.len() == 2, "Invalid TX/RX relay delay reply");
        println!(
            "TX/RX Relay delay: {} ms",
            u16::from_le_bytes([response[0], response[1]])
        );
    } else if cmd == a::RX_TUNING_ERROR {
        ensure!(response.len() == 8, "Invalid tuning-error reply length");
        let value = i64::from_le_bytes(response[..].try_into()?);
        println!(
            "USB audio tuning error (nominal actual - requested): {:+.6} Hz",
            value as f64 / 1_000_000.
        );
    } else if let Some((register, written, verify)) = register_operation {
        if let Some(value) = written {
            if verify {
                let readback = c.command(v2::REGISTER_READ, &[register])?;
                ensure!(readback.len() == 1, "Invalid register-read reply length");
                ensure!(
                    readback[0] == value,
                    "Register 0x{register:02X} write unverified: requested 0x{value:02X}, read 0x{:02X}",
                    readback[0]
                );
                println!(
                    "register 0x{register:02X} written: 0x{value:02X} (0b{value:08b}); readback verified"
                );
            } else {
                println!(
                    "register 0x{register:02X} written: 0x{value:02X} (0b{value:08b}); ACK only, readback not checked"
                );
            }
        } else {
            anyhow::ensure!(response.len() == 1, "Invalid register-read reply length");
            let value = response[0];
            println!("register 0x{register:02X}: 0x{value:02X} (0b{value:08b})");
        }
    } else {
        let s = c.state()?;
        println!("{s:#?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Args, Command, parse_byte};
    use clap::Parser;

    #[test]
    fn register_arguments_accept_decimal_hex_and_binary_bytes() {
        for (input, expected) in [
            ("97", 97),
            ("0x97", 0x97),
            ("0X07", 7),
            ("0b10010111", 0x97),
        ] {
            assert_eq!(parse_byte(input), Ok(expected));
        }
        assert!(parse_byte("0x100").is_err());
        assert!(parse_byte("0b2").is_err());
    }

    #[test]
    fn register_write_verification_is_explicit() {
        let args =
            Args::try_parse_from(["astra918ctl", "register-write", "0x83", "0x02", "--verify"])
                .unwrap();
        assert!(matches!(
            args.command,
            Command::RegisterWrite {
                register: 0x83,
                value: 2,
                verify: true
            }
        ));
        let args = Args::try_parse_from(["astra918ctl", "register-write", "0x83", "0x02"]).unwrap();
        assert!(matches!(
            args.command,
            Command::RegisterWrite { verify: false, .. }
        ));
    }
}
