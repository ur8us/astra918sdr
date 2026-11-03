#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""USB-only AST1 transport and explicit carrier/test commands.

No device is opened on import. Only interface 4 is claimed; USB configuration,
kernel drivers, CAT, audio and I/Q are left to their owners. Flash is written
only when configure_output(save=True) explicitly requests it.
"""

import argparse
import math
import signal
import struct
import sys
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass

VID, PID = 0xC0DE, 0x091A
INTERFACE, COMMAND_EP, REPLY_EP = 4, 0x03, 0x84
PACKET_SIZE = 256
HEADER = struct.Struct("<4sBBBBIHH")
OUTPUT_COMMAND, FC0_COMMAND = 0x43, 0x44
OUTPUT_REQUEST = struct.Struct("<BQ")
PLAN_REPLY = struct.Struct("<BBHHHI4xQQQ")
LDIV_MIN_HZ, LDIV_MAX_HZ = 240_000_000, 316_000_000
STATUS_REASONS = {
    1: "unknown command; firmware may need the USB TX/test commands",
    2: "protocol version mismatch",
    3: "invalid payload length",
    4: "invalid argument or frequency",
    5: "unsupported by this firmware",
    6: "receiver busy or experiment has sealed commands",
    7: "bandwidth unavailable",
    8: "receiver hardware I/O failed or timed out; output may have changed",
    9: "PLL did not lock",
    10: "internal receiver error",
}
FIRMWARE_ERRORS = {
    1: "protocol",
    2: "frequency",
    3: "profile",
    4: "unqualified",
    5: "I2C",
    6: "timeout",
    7: "readback",
    8: "not configured",
    9: "capture",
    10: "USB",
}


class TxUsbError(RuntimeError):
    """Actionable USB, firmware or reply error."""


@contextmanager
def _defer_sigint():
    """Finish one transaction before Ctrl-C; preserve transaction exceptions."""
    if threading.current_thread() is not threading.main_thread():
        yield
        return
    pending = False

    def record_interrupt(signum, frame):
        nonlocal pending
        pending = True

    previous = signal.signal(signal.SIGINT, record_interrupt)
    try:
        yield
    finally:
        signal.signal(signal.SIGINT, previous)
    # An exception from the transaction skips this check and retains priority.
    if pending:
        raise KeyboardInterrupt


def frequency_hz(value):
    """Parse integer Hz without float rounding or unsigned overflow."""
    try:
        frequency = int(value, 10)
    except ValueError as error:
        raise argparse.ArgumentTypeError(
            "frequency must be an integer in Hz"
        ) from error
    if not 0 < frequency <= 0xFFFFFFFFFFFFFFFF:
        raise argparse.ArgumentTypeError(
            "frequency must be positive and fit unsigned 64-bit Hz"
        )
    return frequency


def serial_number(value):
    if not value.strip():
        raise argparse.ArgumentTypeError("serial must not be empty")
    return value


def _load_pyusb():
    try:
        import usb.core
        import usb.util
    except ImportError as error:
        raise TxUsbError(
            "PyUSB is missing for this Python. Install python3-usb for system Python "
            "(Debian/Ubuntu: sudo apt install python3-usb), or install pyusb in your "
            "Python environment."
        ) from error
    return usb.core, usb.util


@dataclass(frozen=True)
class Plan:
    version: int
    mode: int
    divider: int
    l: int  # noqa: E741 - PLL register name
    n: int
    f: int
    actual_rf_millihz: int
    ldiv_millihz: int
    vco_millihz: int


@dataclass(frozen=True)
class Measurement:
    status: int
    raw: int

    @property
    def valid(self):
        return bool(self.status & 0x10) and not (self.status & 0x10010000)

    @property
    def hz(self):
        return self.raw * 1000 / 32 if self.valid else None


def print_plan(plan):
    print(
        f"Planner v{plan.version}: mode={plan.mode}, divider={plan.divider}, "
        f"L={plan.l}, N={plan.n}, F={plan.f}"
    )
    print(
        f"Nominal RF outputs={plan.actual_rf_millihz / 1000:.3f} Hz"
        + (" (disabled for LDIV test)" if plan.mode == 2 else "")
        + ", "
        f"LDIV={plan.ldiv_millihz / 1000:.3f} Hz, "
        f"VCO={plan.vco_millihz / 1000:.3f} Hz"
    )


class AstraUsb:
    """One persistent claim, bounded transfers, and strict AST1 reply checks."""

    def __init__(self, serial=None, timeout_ms=5000):
        if timeout_ms <= 0:
            raise ValueError("USB timeout must be positive")
        self.serial = serial
        self.timeout_ms = timeout_ms
        self.device = None
        self.sequence = 0
        self._claimed = False
        self._synchronized = False

    def __enter__(self):
        self._core, self._util = _load_pyusb()
        try:
            devices = list(self._core.find(find_all=True, idVendor=VID, idProduct=PID))
            if self.serial is not None:
                matches = []
                for device in devices:
                    try:
                        if (
                            self._util.get_string(device, device.iSerialNumber)
                            == self.serial
                        ):
                            matches.append(device)
                    finally:
                        self._util.dispose_resources(device)
                devices = matches
            if not devices:
                identity = (
                    f" with serial {self.serial!r}" if self.serial is not None else ""
                )
                raise TxUsbError(
                    f"No Astra918 USB receiver{identity} found (c0de:091a)."
                )
            if len(devices) != 1:
                raise TxUsbError(
                    "Multiple Astra918 USB receivers found; select one with --serial."
                )
            self.device = devices[0]
            if self.device.get_active_configuration().bConfigurationValue != 1:
                raise TxUsbError(
                    "Receiver USB is not configured as expected; reconnect USB."
                )
            self._util.claim_interface(self.device, INTERFACE)
            self._claimed = True
            self._drain_stale()
            self._synchronized = True
            return self
        except self._core.USBError as error:
            self.close(quiet=True)
            raise self._usb_error("Open/claim vendor interface 4", error) from error
        except self._core.NoBackendError as error:
            self.close(quiet=True)
            raise TxUsbError(
                "PyUSB has no libusb backend; install the system libusb library."
            ) from error
        except BaseException:
            self.close(quiet=True)
            raise

    def __exit__(self, exc_type, exc, traceback):
        self.close(quiet=exc_type is not None)

    def close(self, quiet=False):
        device, self.device = self.device, None
        if device is None:
            return
        try:
            try:
                if self._claimed:
                    self._util.release_interface(device, INTERFACE)
            finally:
                self._claimed = False
                self._synchronized = False
                self._util.dispose_resources(device)
        except Exception as error:
            if not quiet:
                raise TxUsbError(f"Release USB vendor interface: {error}") from error

    @staticmethod
    def _usb_error(action, error):
        if (
            getattr(error, "errno", None) == 16
            or getattr(error, "backend_error_code", None) == -6
        ):
            reason = (
                "vendor interface busy; close the GUI, SDR++ or other USB controller"
            )
        elif (
            getattr(error, "errno", None) in (1, 13)
            or getattr(error, "backend_error_code", None) == -3
        ):
            reason = "USB access denied; check receiver udev permissions"
        else:
            reason = str(error)
        return TxUsbError(f"{action}: {reason}")

    def _drain_stale(self):
        # Same bounded drain as the Rust host: at most 16 reads of 20 ms.
        for _ in range(16):
            try:
                chunk = self.device.read(REPLY_EP, PACKET_SIZE, timeout=20)
            except self._core.USBTimeoutError:
                return
            if not chunk:
                break
        raise TxUsbError(
            "USB reply queue did not drain; close other controllers and reconnect USB."
        )

    @_defer_sigint()
    def command(self, command, payload=b""):
        if self.device is None or not self._claimed:
            raise TxUsbError("USB vendor interface is not open.")
        if len(payload) > 240:
            raise ValueError("AST1 payload exceeds 240 bytes")
        if not self._synchronized:
            raise TxUsbError(
                "USB transport lost framing; only STOP recovery is permitted."
            )
        self.sequence = (self.sequence + 1) & 0xFFFFFFFF
        packet = bytearray(PACKET_SIZE)
        HEADER.pack_into(
            packet, 0, b"AST1", 1, command, 0, 0, self.sequence, len(payload), 0
        )
        packet[HEADER.size : HEADER.size + len(payload)] = payload
        self._synchronized = False
        deadline = time.monotonic() + self.timeout_ms / 1000

        def remaining_ms():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TxUsbError(f"USB command 0x{command:02X} deadline expired.")
            return max(1, math.ceil(remaining * 1000))

        try:
            used = 0
            while used < PACKET_SIZE:
                count = self.device.write(
                    COMMAND_EP, packet[used:], timeout=remaining_ms()
                )
                if not 0 < count <= PACKET_SIZE - used:
                    raise TxUsbError(
                        "USB command write made no progress or returned an invalid size."
                    )
                used += count
            reply = bytearray()
            while len(reply) < PACKET_SIZE:
                chunk = self.device.read(
                    REPLY_EP, PACKET_SIZE - len(reply), timeout=remaining_ms()
                )
                if not 0 < len(chunk) <= PACKET_SIZE - len(reply):
                    raise TxUsbError(
                        "USB reply read made no progress or returned an invalid size."
                    )
                reply.extend(chunk)
        except self._core.USBError as error:
            raise self._usb_error(f"USB command 0x{command:02X}", error) from error
        magic, version, echoed, status, reserved, sequence, length, reserved2 = (
            HEADER.unpack_from(reply)
        )
        if (magic, version, echoed, sequence) != (b"AST1", 1, command, self.sequence):
            raise TxUsbError(
                f"Mismatched AST1 reply for command 0x{command:02X}; transport lost framing."
            )
        if length > 240 or reserved or reserved2 or any(reply[HEADER.size + length :]):
            raise TxUsbError("Malformed AST1 reply length, reserved bytes or padding.")
        self._synchronized = True
        if status:
            detail = ""
            if status == 8 and length == 4:
                code = struct.unpack_from("<I", reply, HEADER.size)[0]
                detail = (
                    f"; firmware Error code {code} "
                    f"({FIRMWARE_ERRORS.get(code, 'unknown')})"
                )
            raise TxUsbError(
                f"Receiver rejected command 0x{command:02X}: status {status} "
                f"({STATUS_REASONS.get(status, 'unknown status')}){detail}."
            )
        return bytes(reply[HEADER.size : HEADER.size + length])

    def start(self, mode, frequency):
        if mode not in (1, 2):
            raise ValueError("Start requires mode 1 (manual carrier) or 2 (LDIV test)")
        if not isinstance(frequency, int) or not 0 < frequency <= 0xFFFFFFFFFFFFFFFF:
            raise ValueError("Frequency must be positive unsigned 64-bit integer Hz")
        if mode == 2 and not LDIV_MIN_HZ <= frequency <= LDIV_MAX_HZ:
            raise ValueError("LDIV frequency must be 240000000..316000000 Hz")
        data = self.command(OUTPUT_COMMAND, OUTPUT_REQUEST.pack(mode, frequency))
        if len(data) != PLAN_REPLY.size:
            raise TxUsbError(f"Start reply must be 40 bytes; received {len(data)}.")
        plan = Plan(*PLAN_REPLY.unpack(data))
        if plan.version != 1 or plan.mode != mode or any(data[12:16]):
            raise TxUsbError("Invalid planner reply version, mode or reserved padding.")
        return plan

    def configure_output(self, drive_ma=None, slew=None, save=False):
        """Independent pad settings; only an explicit save writes flash.

        Check all requested capabilities before making any changes. SAVE saves
        the complete current transceiver settings, not only the pad settings.
        """
        if drive_ma is not None and drive_ma not in (2, 4, 8, 12):
            raise ValueError("Drive strength must be 2, 4, 8 or 12 mA")
        if slew is not None and slew not in ("slow", "fast"):
            raise ValueError("Slew rate must be slow or fast")
        if drive_ma is None and slew is None and not save:
            return
        status = self.command(0x13)
        if len(status) != 128:
            raise TxUsbError("Output configuration requires a 128-byte status reply.")
        if drive_ma is not None and not status[127] & 8:
            raise TxUsbError("Firmware does not support drive-strength control (0x46).")
        if slew is not None and not status[127] & 16:
            raise TxUsbError("Firmware does not support slew-rate control (0x47).")
        if drive_ma is not None:
            status = self.command(0x46, bytes([drive_ma]))
            if len(status) != 128 or not status[127] & 8 or status[93] != drive_ma:
                raise TxUsbError(
                    "Drive-strength setting was not confirmed; carrier not started."
                )
        if slew is not None:
            status = self.command(0x47, bytes([int(slew == "fast")]))
            if (
                len(status) != 128
                or not status[127] & 16
                or bool(status[127] & 32) != (slew == "fast")
            ):
                raise TxUsbError(
                    "Slew-rate setting was not confirmed; carrier not started."
                )
        if save:
            status = self.command(0x36)
            if len(status) != 128 or status[96:100] != status[112:116]:
                raise TxUsbError(
                    "Save did not confirm the current revision; flash save unconfirmed."
                )
            print("Current transceiver settings saved to flash.", flush=True)

    def stop(self):
        try:
            if not self._synchronized:
                self._drain_stale()
                self._synchronized = True
            data = self.command(OUTPUT_COMMAND, OUTPUT_REQUEST.pack(0, 0))
            if len(data) != 128:
                raise TxUsbError(
                    f"STOP reply must be 128-byte status; received {len(data)}."
                )
            error = int.from_bytes(data[52:56], "little")
            if not data[35] or error or data[127] & 2:
                raise TxUsbError(
                    f"Receiver did not recover RX: configured={bool(data[35])}, "
                    f"TX active={bool(data[127] & 2)}, error={error}."
                )
        except Exception as error:
            raise TxUsbError(
                f"STOP failed; output state is unconfirmed: {error}"
            ) from error

    def measure_batch(self):
        data = self.command(FC0_COMMAND)
        if len(data) != 64:
            raise TxUsbError(
                f"FC0 reply must be 64 bytes (eight samples); received {len(data)}."
            )
        return [
            Measurement(status, raw) for status, raw in struct.iter_unpack("<II", data)
        ]

    @contextmanager
    def output(self, mode, frequency, leave=False):
        """Arm cleanup before START, including ambiguous failed START replies."""
        try:
            yield self.start(mode, frequency)
        except BaseException:
            try:
                self.stop()
            except Exception as error:
                print(f"Cleanup: {error}", file=sys.stderr)
            raise
        else:
            if not leave:
                self.stop()
