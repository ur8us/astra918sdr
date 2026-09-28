use anyhow::Result;
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
    Save,
    Retry,
    Start,
    Stop,
}
fn main() -> Result<()> {
    let args = Args::parse();
    if matches!(args.command, Command::List) {
        for d in devices()? {
            println!("{}", d.label);
        }
        return Ok(());
    }
    let mut c = if let Some(addr) = args.simulator {
        Client::tcp(&addr)?
    } else {
        Client::usb(
            args.serial
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("Specify --serial or --simulator HOST:PORT"))?,
        )?
    };
    let (cmd, p) = match args.command {
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
        Command::Save => (a::SAVE, vec![]),
        Command::Retry => (a::RETRY, vec![]),
        Command::Start => (v2::START, vec![]),
        Command::Stop => (v2::STOP, vec![]),
        _ => (v2::STATUS, vec![]),
    };
    c.command(cmd, &p)?;
    let s = c.state()?;
    println!("{s:#?}");
    Ok(())
}
