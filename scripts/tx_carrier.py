#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Generate a manual carrier on the RF outputs over USB.

python3 scripts/tx_carrier.py 14074000          # any key stops; Enter on non-TTY
python3 scripts/tx_carrier.py 14074000 --leave  # return with carrier still active
python3 scripts/tx_carrier.py --stop           # stop and restore the receiver
python3 scripts/tx_carrier.py 14074000 --drive-ma 12 --slew slow --save

Requires PyUSB and firmware command 0x43. Failed starts and Ctrl-C send STOP.
Drive strength and slew rate apply to both pins. Without --save they are volatile.
--save saves all current transceiver settings to flash before starting the carrier.
"""

import argparse
import sys

from tx_usb import AstraUsb, TxUsbError, frequency_hz, print_plan, serial_number


def wait_for_key():
    if not sys.stdin.isatty():
        print("Press Enter to STOP (EOF also stops).", flush=True)
        sys.stdin.readline()
    elif sys.platform == "win32":
        import msvcrt

        print("Press any key to STOP.", flush=True)
        if msvcrt.getwch() == "\x03":
            raise KeyboardInterrupt
    else:
        import termios
        import tty

        descriptor = sys.stdin.fileno()
        previous = termios.tcgetattr(descriptor)
        print("Press any key to STOP.", flush=True)
        try:
            # cbreak keeps Ctrl-C handling while accepting a single key.
            tty.setcbreak(descriptor)
            sys.stdin.read(1)
        finally:
            termios.tcsetattr(descriptor, termios.TCSADRAIN, previous)


def main(argv=None):
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "frequency",
        type=frequency_hz,
        nargs="?",
        help="manual RF outputs carrier frequency in integer Hz",
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--leave",
        action="store_true",
        help="leave carrier active after successful setup; no wait or STOP",
    )
    mode.add_argument(
        "--stop", action="store_true", help="STOP only; do not supply a frequency"
    )
    parser.add_argument(
        "--serial", type=serial_number, help="select receiver USB serial number"
    )
    parser.add_argument(
        "--drive-ma", type=int, choices=(2, 4, 8, 12),
        help="RF outputs drive strength in mA",
    )
    parser.add_argument(
        "--slew", choices=("slow", "fast"),
        help="RF outputs edge rate",
    )
    parser.add_argument(
        "--save", action="store_true",
        help="save all current settings to flash after applying output options",
    )
    args = parser.parse_args(argv)
    if args.stop and args.frequency is not None:
        parser.error("--stop does not accept a frequency")
    if not args.stop and args.frequency is None:
        parser.error("a frequency is required unless --stop is used")
    try:
        with AstraUsb(args.serial) as receiver:
            if args.stop:
                receiver.stop()
                if args.drive_ma is not None or args.slew is not None or args.save:
                    receiver.configure_output(args.drive_ma, args.slew, args.save)
            else:
                if args.drive_ma is not None or args.slew is not None or args.save:
                    receiver.configure_output(args.drive_ma, args.slew, args.save)
                with receiver.output(1, args.frequency, leave=args.leave) as plan:
                    print_plan(plan)
                    if args.leave:
                        print(
                            "Carrier left active. Stop with: python3 scripts/tx_carrier.py --stop"
                            + (
                                f" --serial {args.serial}"
                                if args.serial is not None
                                else ""
                            ),
                            flush=True,
                        )
                    else:
                        wait_for_key()
            if not args.leave:
                print("STOP acknowledged; receiver restored.")
    except KeyboardInterrupt:
        print("Interrupted.", file=sys.stderr)
        return 130
    except (TxUsbError, OSError) as error:
        print(f"Error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
