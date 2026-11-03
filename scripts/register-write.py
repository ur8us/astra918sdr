#!/usr/bin/env python3
"""Write one CMX918 register through the Astra918 USB vendor interface."""

import argparse
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[1]


def byte(text):
    try:
        value = int(text, 0) if text.lower().startswith(("0x", "0b")) else int(text, 10)
    except ValueError as error:
        raise argparse.ArgumentTypeError(f"invalid byte: {text}") from error
    if not 0 <= value <= 255:
        raise argparse.ArgumentTypeError("register address and value must be between 0 and 255")
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("register", type=byte, help="register address (decimal, 0x hex, or 0b binary)")
    parser.add_argument("value", type=byte, help="byte to write (decimal, 0x hex, or 0b binary)")
    target = parser.add_mutually_exclusive_group()
    target.add_argument("--serial", help="receiver serial; optional when exactly one is connected")
    target.add_argument("--simulator", metavar="HOST:PORT")
    parser.add_argument("--cli", type=Path, help="use an existing astra918ctl binary instead of cargo run")
    args = parser.parse_args()

    command = ([str(args.cli.expanduser().resolve())] if args.cli else
               ["cargo", "run", "--quiet", "--locked", "-p", "astra918-host",
                "--bin", "astra918ctl", "--"])
    if args.serial:
        command += ["--serial", args.serial]
    elif args.simulator:
        command += ["--simulator", args.simulator]
    command += ["register-write", f"0x{args.register:02X}", f"0x{args.value:02X}"]
    raise SystemExit(subprocess.call(command, cwd=ROOT))


if __name__ == "__main__":
    main()
