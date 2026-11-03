#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Offline USB mocks only: python3 -m unittest discover -s scripts -p test_tx_usb.py."""

import io
import signal
import struct
import sys
import unittest
from collections import deque
from contextlib import redirect_stderr, redirect_stdout
from types import SimpleNamespace
from unittest.mock import MagicMock, Mock, patch

import tx_carrier
import tx_usb as usb


class UsbError(Exception):
    def __init__(self, message="USB failure", errno=None, backend_error_code=None):
        super().__init__(message)
        self.errno = errno
        self.backend_error_code = backend_error_code


class UsbTimeout(UsbError):
    pass


class NoBackendError(ValueError):
    pass


def plan_payload(mode=1):
    return struct.pack(
        "<BBHHHI4xQQQ",
        1,
        mode,
        20,
        3,
        128,
        12345,
        14_074_000_000,
        281_480_000_000,
        1_688_880_000_000,
    )


def error_reply(status, payload=b""):
    def mutate(reply):
        reply[6] = status
        struct.pack_into("<H", reply, 12, len(payload))
        reply[16:] = bytes(240)
        reply[16 : 16 + len(payload)] = payload

    return mutate


class Device:
    iSerialNumber = 1

    def __init__(self):
        self.requests = []
        self.writes = []
        self.reads = []
        self.partial = bytearray()
        self.pending = bytearray()
        self.outcomes = deque()
        self.fragment = 64
        self.read_error = None
        self.write_error = None
        self.status = bytearray(128)
        self.status[35] = 1
        self.status[93] = 4
        self.status[127] = 1 | 8 | 16 | 32
        self.status[112:116] = b"\xff" * 4

    def get_active_configuration(self):
        return SimpleNamespace(bConfigurationValue=1)

    def write(self, endpoint, data, timeout):
        self.writes.append((endpoint, bytes(data), timeout))
        if self.write_error:
            error, self.write_error = self.write_error, None
            raise error
        count = min(self.fragment, len(data))
        self.partial.extend(data[:count])
        if len(self.partial) == 256:
            request = bytes(self.partial)
            self.partial.clear()
            self.requests.append(request)
            command = request[5]
            if command == 0x43:
                mode, _ = struct.unpack_from("<BQ", request, 16)
                if mode:
                    payload = plan_payload(mode)
                else:
                    payload = bytearray(128)
                    payload[35] = 1  # RX configured, no error, TX inactive.
            elif command in (0x13, 0x36, 0x46, 0x47):
                if command in (0x46, 0x47):
                    if command == 0x46:
                        self.status[93] = request[16]
                    else:
                        self.status[127] = (self.status[127] & ~32) | (32 if request[16] else 0)
                    revision = int.from_bytes(self.status[96:100], "little") + 1
                    self.status[96:100] = revision.to_bytes(4, "little")
                if command == 0x36:
                    self.status[112:116] = self.status[96:100]
                payload = self.status
            elif command == 0x44:
                payload = struct.pack("<II", 0x10, 8_960_000) * 8
            else:
                payload = b""
            reply = bytearray(256)
            usb.HEADER.pack_into(
                reply,
                0,
                b"AST1",
                1,
                command,
                0,
                0,
                struct.unpack_from("<I", request, 8)[0],
                len(payload),
                0,
            )
            reply[16 : 16 + len(payload)] = payload
            if self.outcomes:
                self.outcomes.popleft()(reply)
            self.pending.extend(reply)
        return count

    def read(self, endpoint, size, timeout):
        self.reads.append((endpoint, size, timeout))
        if self.read_error:
            error, self.read_error = self.read_error, None
            raise error
        if not self.pending:
            raise UsbTimeout("timed out")
        result = self.pending[: min(size, self.fragment)]
        del self.pending[: len(result)]
        return result


class UsbTests(unittest.TestCase):
    def setUp(self):
        self.device = Device()
        self.core = SimpleNamespace(
            USBError=UsbError,
            USBTimeoutError=UsbTimeout,
            NoBackendError=NoBackendError,
            find=Mock(return_value=[self.device]),
        )
        self.util = SimpleNamespace(
            claim_interface=Mock(),
            release_interface=Mock(),
            dispose_resources=Mock(),
            get_string=Mock(return_value="ASTRA-1"),
        )
        self.loader = patch.object(
            usb, "_load_pyusb", return_value=(self.core, self.util)
        )
        self.loader.start()
        self.addCleanup(self.loader.stop)
        self.stdout = io.StringIO()
        self.stderr = io.StringIO()
        for context in (redirect_stdout(self.stdout), redirect_stderr(self.stderr)):
            context.__enter__()
            self.addCleanup(context.__exit__, None, None, None)

    def modes(self):
        return [
            struct.unpack_from("<BQ", request, 16)
            for request in self.device.requests
            if request[5] == 0x43
        ]

    def test_wire_layout_fragmented_transfers_and_persistent_claim(self):
        with usb.AstraUsb("ASTRA-1") as receiver:
            with receiver.output(1, 14_074_000) as plan:
                self.assertEqual(
                    (plan.mode, plan.divider, plan.l, plan.n, plan.f),
                    (1, 20, 3, 128, 12345),
                )
                self.assertEqual(plan.actual_rf_millihz, 14_074_000_000)
                self.assertEqual(plan.ldiv_millihz, 281_480_000_000)
                self.assertEqual(plan.vco_millihz, 1_688_880_000_000)
        self.assertEqual(self.modes(), [(1, 14_074_000), (0, 0)])
        self.core.find.assert_called_once_with(
            find_all=True, idVendor=0xC0DE, idProduct=0x091A
        )
        self.util.claim_interface.assert_called_once_with(self.device, 4)
        self.util.release_interface.assert_called_once_with(self.device, 4)
        for sequence, request in enumerate(self.device.requests, 1):
            self.assertEqual(len(request), 256)
            self.assertEqual(
                usb.HEADER.unpack_from(request),
                (b"AST1", 1, 0x43, 0, 0, sequence, 9, 0),
            )
            self.assertEqual(request[25:], bytes(231))
        self.assertTrue(
            all(
                endpoint == 3 and 0 < timeout <= 5000
                for endpoint, _, timeout in self.device.writes
            )
        )
        self.assertTrue(all(endpoint == 0x84 for endpoint, _, _ in self.device.reads))

    def test_default_carrier_waits_then_stops(self):
        with patch.object(tx_carrier, "wait_for_key") as wait:
            self.assertEqual(tx_carrier.main(["14074000"]), 0)
        wait.assert_called_once_with()
        self.assertEqual(self.modes(), [(1, 14_074_000), (0, 0)])

    def test_independent_output_options_and_explicit_save_precede_start(self):
        for args, expected in [
            (["--drive-ma", "12"], [0x13, 0x46, 0x43]),
            (["--slew", "slow"], [0x13, 0x47, 0x43]),
            (["--drive-ma", "8", "--slew", "slow", "--save"], [0x13, 0x46, 0x47, 0x36, 0x43]),
            (["--save"], [0x13, 0x36, 0x43]),
        ]:
            with self.subTest(args=args):
                self.device.requests.clear()
                self.assertEqual(tx_carrier.main(["14074000", "--leave", *args]), 0)
                self.assertEqual([r[5] for r in self.device.requests], expected)
                if "--save" in args:
                    self.assertEqual(self.device.status[96:100], self.device.status[112:116])
        self.assertEqual(self.device.status[93], 8)
        self.assertEqual(self.device.status[127] & 32, 0)

    def test_save_after_stop_and_options_never_key_tx(self):
        self.assertEqual(tx_carrier.main(["--stop", "--drive-ma", "12", "--slew", "slow", "--save"]), 0)
        self.assertEqual([r[5] for r in self.device.requests], [0x43, 0x13, 0x46, 0x47, 0x36])
        self.assertEqual(self.modes(), [(0, 0)])

    def test_missing_slew_capability_prevents_partial_change_and_carrier(self):
        self.device.status[127] &= ~16
        self.assertEqual(tx_carrier.main(["14074000", "--drive-ma", "12", "--slew", "slow", "--save"]), 1)
        self.assertEqual([r[5] for r in self.device.requests], [0x13])
        self.assertIn("0x47", self.stderr.getvalue())

    def test_setter_or_save_failure_never_starts_carrier(self):
        for failure, expected in [(2, "not confirmed"), (3, "status 8")]:
            with self.subTest(failure=failure):
                self.device.requests.clear()
                self.device.outcomes.extend([lambda r: None] * (failure - 1))
                self.device.outcomes.append(
                    (lambda r: r.__setitem__(16 + 93, 4)) if failure == 2 else error_reply(8)
                )
                self.assertEqual(tx_carrier.main(["14074000", "--drive-ma", "12", "--save"]), 1)
                self.assertEqual(self.modes(), [])
                self.assertIn(expected, self.stderr.getvalue())

    def test_leave_returns_without_wait_or_stop(self):
        with patch.object(tx_carrier, "wait_for_key") as wait:
            self.assertEqual(tx_carrier.main(["14074000", "--leave"]), 0)
        wait.assert_not_called()
        self.assertEqual(self.modes(), [(1, 14_074_000)])
        self.util.release_interface.assert_called_once()

    def test_stop_needs_no_frequency(self):
        self.assertEqual(tx_carrier.main(["--stop"]), 0)
        self.assertEqual(self.modes(), [(0, 0)])

    def test_ctrl_c_during_wait_stops_and_releases(self):
        with patch.object(tx_carrier, "wait_for_key", side_effect=KeyboardInterrupt):
            self.assertEqual(tx_carrier.main(["14074000"]), 130)
        self.assertEqual(self.modes(), [(1, 14_074_000), (0, 0)])
        self.util.release_interface.assert_called_once()

    def test_sigint_during_start_read_finishes_reply_before_stop_and_restores_handler(
        self,
    ):
        previous = signal.getsignal(signal.SIGINT)
        original_read = self.device.read
        interrupted = False
        with usb.AstraUsb() as receiver:
            original_stop = receiver.stop

            def interrupt_read(*args, **kwargs):
                nonlocal interrupted
                chunk = original_read(*args, **kwargs)
                if not interrupted:
                    interrupted = True
                    self.assertFalse(receiver._synchronized)
                    handler = signal.getsignal(signal.SIGINT)
                    self.assertIsNot(handler, previous)
                    handler(signal.SIGINT, None)
                return chunk

            def checked_stop():
                self.assertTrue(receiver._synchronized)
                self.assertEqual(self.device.pending, b"")
                self.assertIs(signal.getsignal(signal.SIGINT), previous)
                original_stop()

            with (
                patch.object(self.device, "read", side_effect=interrupt_read),
                patch.object(receiver, "stop", side_effect=checked_stop) as stop,
            ):
                with self.assertRaises(KeyboardInterrupt):
                    with receiver.output(1, 14_074_000, leave=True):
                        self.fail(
                            "Pending SIGINT must interrupt before yielding the plan"
                        )
                stop.assert_called_once()
            self.assertTrue(interrupted)
            self.assertIs(signal.getsignal(signal.SIGINT), previous)
        self.assertEqual(self.modes(), [(1, 14_074_000), (0, 0)])
        self.util.release_interface.assert_called_once()

    def test_pending_sigint_preserves_transport_error_and_restores_handler(self):
        previous = signal.getsignal(signal.SIGINT)
        original_read = self.device.read
        interrupted = False

        def interrupt_and_fail_read(*args, **kwargs):
            nonlocal interrupted
            if not interrupted:
                interrupted = True
                signal.getsignal(signal.SIGINT)(signal.SIGINT, None)
                raise UsbTimeout("START read timed out")
            return original_read(*args, **kwargs)

        with usb.AstraUsb() as receiver:
            with patch.object(self.device, "read", side_effect=interrupt_and_fail_read):
                with self.assertRaisesRegex(usb.TxUsbError, "START read timed out"):
                    with receiver.output(1, 14_074_000):
                        self.fail("Transport error must retain priority over SIGINT")
            self.assertIs(signal.getsignal(signal.SIGINT), previous)
        self.assertEqual(self.modes(), [(1, 14_074_000), (0, 0)])
        self.assertNotIn("STOP failed", self.stderr.getvalue())

    def test_worker_thread_command_does_not_install_signal_handlers(self):
        with (
            patch.object(usb.threading, "current_thread", return_value=object()),
            patch.object(signal, "signal") as install_handler,
        ):
            self.assertEqual(tx_carrier.main(["--stop"]), 0)
            install_handler.assert_not_called()

    def test_leave_print_failure_stops(self):
        with patch.object(
            tx_carrier, "print_plan", side_effect=OSError("output closed")
        ):
            self.assertEqual(tx_carrier.main(["14074000", "--leave"]), 1)
        self.assertEqual(self.modes(), [(1, 14_074_000), (0, 0)])

    def test_start_rejected_still_stops_even_with_leave(self):
        self.device.outcomes.append(lambda reply: reply.__setitem__(6, 9))
        self.assertEqual(tx_carrier.main(["14074000", "--leave"]), 1)
        self.assertEqual(self.modes(), [(1, 14_074_000), (0, 0)])
        self.assertIn("status 9 (PLL did not lock)", self.stderr.getvalue())

    def test_malformed_start_reply_recovers_stop(self):
        self.device.outcomes.append(lambda reply: reply.__setitem__(0, 0))
        self.assertEqual(tx_carrier.main(["14074000", "--leave"]), 1)
        self.assertEqual(self.modes(), [(1, 14_074_000), (0, 0)])

    def test_start_read_timeout_drains_stale_reply_before_stop(self):
        def fail_read(reply):
            self.device.read_error = UsbTimeout("start reply lost")

        self.device.outcomes.append(fail_read)
        self.assertEqual(tx_carrier.main(["14074000"]), 1)
        self.assertEqual(self.modes(), [(1, 14_074_000), (0, 0)])
        self.assertNotIn("STOP failed", self.stderr.getvalue())

    def test_stop_failure_is_not_reported_as_success(self):
        self.device.outcomes.append(lambda reply: reply.__setitem__(6, 8))
        self.assertEqual(tx_carrier.main(["--stop"]), 1)
        self.assertIn("output state is unconfirmed", self.stderr.getvalue())
        self.assertNotIn("STOP acknowledged", self.stdout.getvalue())

    def test_stop_rejects_unrecovered_rx_flags_and_firmware_errors(self):
        for name, mutate, detail in [
            (
                "not configured",
                lambda reply: reply.__setitem__(16 + 35, 0),
                "configured=False, TX active=False, error=0",
            ),
            (
                "TX still active",
                lambda reply: reply.__setitem__(16 + 127, 2),
                "configured=True, TX active=True, error=0",
            ),
            (
                "firmware I2C error",
                lambda reply: struct.pack_into("<I", reply, 16 + 52, 5),
                "configured=True, TX active=False, error=5",
            ),
            (
                "firmware error high bytes",
                lambda reply: struct.pack_into("<I", reply, 16 + 52, 0x01020304),
                "configured=True, TX active=False, error=16909060",
            ),
        ]:
            with self.subTest(recovery=name):
                self.stdout.seek(0)
                self.stdout.truncate()
                self.stderr.seek(0)
                self.stderr.truncate()
                self.device.outcomes.append(mutate)
                self.assertEqual(tx_carrier.main(["--stop"]), 1)
                self.assertIn(
                    "STOP failed; output state is unconfirmed", self.stderr.getvalue()
                )
                self.assertIn(detail, self.stderr.getvalue())
                self.assertNotIn("STOP acknowledged", self.stdout.getvalue())
        self.assertEqual(self.modes(), [(0, 0)] * 4)
        self.assertEqual(self.util.release_interface.call_count, 4)

    def test_stop_accepts_saved_tx_enabled_when_tx_is_inactive(self):
        self.device.outcomes.append(lambda reply: reply.__setitem__(16 + 127, 1))
        self.assertEqual(tx_carrier.main(["--stop"]), 0)
        self.assertIn("STOP acknowledged", self.stdout.getvalue())

    def test_cleanup_failure_preserves_original_error_and_reports_unknown_state(self):
        self.device.outcomes.extend(
            [
                lambda reply: reply.__setitem__(6, 9),
                lambda reply: reply.__setitem__(6, 8),
            ]
        )
        self.assertEqual(tx_carrier.main(["14074000", "--leave"]), 1)
        self.assertIn("status 9", self.stderr.getvalue())
        self.assertIn(
            "STOP failed; output state is unconfirmed", self.stderr.getvalue()
        )

    def test_strict_ast1_reply_validation(self):
        mutations = {
            "magic": lambda p: p.__setitem__(0, 0),
            "version": lambda p: p.__setitem__(4, 2),
            "command": lambda p: p.__setitem__(5, 0),
            "sequence": lambda p: p.__setitem__(8, 99),
            "reserved": lambda p: p.__setitem__(7, 1),
            "reserved2": lambda p: p.__setitem__(14, 1),
            "length": lambda p: struct.pack_into("<H", p, 12, 241),
            "padding": lambda p: p.__setitem__(255, 1),
        }
        for name, mutate in mutations.items():
            with self.subTest(field=name):
                self.device.outcomes.append(mutate)
                with usb.AstraUsb() as receiver:
                    with self.assertRaises(usb.TxUsbError):
                        receiver.command(0x44)
                    receiver.stop()

    def test_strict_start_reply_metadata_and_length(self):
        mutations = [
            lambda p: struct.pack_into("<H", p, 12, 41),
            lambda p: p.__setitem__(16, 2),
            lambda p: p.__setitem__(17, 2),
            lambda p: p.__setitem__(28, 1),
        ]
        for mutate in mutations:
            self.device.outcomes.append(mutate)
            with usb.AstraUsb() as receiver:
                with self.assertRaises(usb.TxUsbError):
                    with receiver.output(1, 14_074_000):
                        self.fail("Invalid plan accepted")
                self.assertEqual(self.modes()[-1], (0, 0))

    def test_strict_fc0_and_stop_lengths(self):
        with usb.AstraUsb() as receiver:
            self.device.outcomes.append(lambda p: struct.pack_into("<H", p, 12, 65))
            with self.assertRaisesRegex(usb.TxUsbError, "64 bytes"):
                receiver.measure_batch()
            self.device.outcomes.append(lambda p: struct.pack_into("<H", p, 12, 127))
            with self.assertRaisesRegex(usb.TxUsbError, "128-byte status"):
                receiver.stop()

    def test_status_error_explains_old_firmware(self):
        self.device.outcomes.append(lambda p: p.__setitem__(6, 1))
        self.assertEqual(tx_carrier.main(["14074000"]), 1)
        self.assertIn("firmware may need", self.stderr.getvalue())

    def test_status8_decodes_little_endian_firmware_error_codes(self):
        for code, name in [
            (1, "protocol"),
            (2, "frequency"),
            (3, "profile"),
            (4, "unqualified"),
            (5, "I2C"),
            (6, "timeout"),
            (7, "readback"),
            (8, "not configured"),
            (9, "capture"),
            (10, "USB"),
            (0x01020304, "unknown"),
        ]:
            with self.subTest(code=code), usb.AstraUsb() as receiver:
                self.device.outcomes.append(error_reply(8, struct.pack("<I", code)))
                with self.assertRaises(usb.TxUsbError) as error:
                    receiver.start(2, 250_000_000)
                self.assertIn("command 0x43: status 8", str(error.exception))
                self.assertIn(
                    f"firmware Error code {code} ({name})", str(error.exception)
                )
                receiver.stop()

    def test_firmware_error_detail_requires_status8_and_exactly_four_bytes(self):
        for status, payload in [
            (8, b""),
            (8, b"\x05\x00\x00"),
            (8, b"\x05\x00\x00\x00\x00"),
            (9, struct.pack("<I", 5)),
        ]:
            with (
                self.subTest(status=status, payload=payload),
                usb.AstraUsb() as receiver,
            ):
                self.device.outcomes.append(error_reply(status, payload))
                with self.assertRaises(usb.TxUsbError) as error:
                    receiver.start(2, 250_000_000)
                self.assertIn(f"status {status}", str(error.exception))
                self.assertNotIn("firmware Error code", str(error.exception))
                receiver.stop()

    def test_no_device_multiple_devices_and_serial_mismatch(self):
        for devices, args, message in [
            ([], [], "No Astra918"),
            ([self.device, Device()], [], "--serial"),
            ([self.device], ["--serial", "OTHER"], "OTHER"),
        ]:
            with self.subTest(message=message):
                self.core.find.return_value = devices
                with self.assertRaisesRegex(usb.TxUsbError, message):
                    with usb.AstraUsb("OTHER" if args else None):
                        self.fail("Unexpected receiver")
        self.util.claim_interface.assert_not_called()
        self.assertEqual(self.device.writes, [])

    def test_busy_permission_and_no_backend_errors(self):
        for error, message in [
            (UsbError(errno=16), "busy"),
            (UsbError(backend_error_code=-6), "busy"),
            (UsbError(errno=13), "permissions"),
            (NoBackendError(), "libusb"),
        ]:
            with self.subTest(message=message):
                self.util.claim_interface.side_effect = error
                with self.assertRaisesRegex(usb.TxUsbError, message):
                    with usb.AstraUsb():
                        self.fail("Unexpected claim")
        self.util.release_interface.assert_not_called()
        self.assertEqual(self.util.dispose_resources.call_count, 4)

    def test_stale_partial_reply_drained_before_first_command(self):
        self.device.pending.extend(b"old reply fragment" * 5)
        self.assertEqual(tx_carrier.main(["--stop"]), 0)
        self.assertGreaterEqual(
            sum(timeout == 20 for _, _, timeout in self.device.reads), 3
        )

    def test_continuously_stale_endpoint_is_bounded_and_releases(self):
        self.device.pending.extend(b"x" * 2048)
        with self.assertRaisesRegex(usb.TxUsbError, "did not drain"):
            with usb.AstraUsb():
                self.fail("Drain should fail")
        self.assertEqual(len(self.device.reads), 16)
        self.util.release_interface.assert_called_once()
        self.util.dispose_resources.assert_called_once()
        self.assertEqual(self.device.writes, [])

    def test_close_disposes_even_when_release_fails(self):
        self.util.release_interface.side_effect = UsbError("disconnected")
        self.assertEqual(tx_carrier.main(["--stop"]), 1)
        self.util.dispose_resources.assert_called_once()

    def test_no_progress_and_usb_write_failure_restore(self):
        for failure in (UsbError("write failed"), None):
            with self.subTest(failure=failure):
                with usb.AstraUsb() as receiver:
                    original = self.device.write
                    if failure:
                        self.device.write_error = failure
                        with self.assertRaises(usb.TxUsbError):
                            with receiver.output(1, 14_074_000):
                                self.fail("Write should fail")
                    else:
                        with patch.object(
                            self.device, "write", side_effect=[0, *[64] * 4]
                        ):
                            with self.assertRaisesRegex(usb.TxUsbError, "no progress"):
                                receiver.start(1, 14_074_000)
                        # Restore a real mock write for STOP recovery.
                        self.device.write = original
                        receiver.stop()
                self.assertEqual(self.modes()[-1], (0, 0))

    def test_invalid_cli_arguments_never_open_usb(self):
        cases = [
            (tx_carrier, []),
            (tx_carrier, ["0"]),
            (tx_carrier, ["-1"]),
            (tx_carrier, ["nan"]),
            (tx_carrier, ["14074000.5"]),
            (tx_carrier, [str(1 << 64)]),
            (tx_carrier, ["--leave"]),
            (tx_carrier, ["--stop", "14074000"]),
            (tx_carrier, ["--stop", "--leave"]),
            (tx_carrier, ["14074000", "--serial", ""]),
            (tx_carrier, ["14074000", "--drive-ma", "3"]),
            (tx_carrier, ["14074000", "--slew", "medium"]),
        ]
        for module, args in cases:
            with self.subTest(script=module.__name__, args=args):
                with self.assertRaises(SystemExit) as error:
                    module.main(args)
                self.assertEqual(error.exception.code, 2)
        self.core.find.assert_not_called()

    def test_helpers_reject_invalid_modes_and_frequencies_without_commands(self):
        with usb.AstraUsb() as receiver:
            for mode, frequency in [
                (0, 0),
                (3, 1),
                (1, 0),
                (1, -1),
                (1, 1 << 64),
                (1, 1.5),
                (2, 239999999),
                (2, 316000001),
            ]:
                with self.assertRaises(ValueError):
                    receiver.start(mode, frequency)
        self.assertEqual(self.device.writes, [])

    def test_sequence_wraparound(self):
        with usb.AstraUsb() as receiver:
            receiver.sequence = 0xFFFFFFFF
            receiver.stop()
        self.assertEqual(struct.unpack_from("<I", self.device.requests[0], 8)[0], 0)


class ReportingAndInputTests(unittest.TestCase):
    def test_planner_labels_nominal_rf_outputs_and_disabled_ldiv_output(self):
        for mode in (1, 2):
            with self.subTest(mode=mode):
                plan = usb.Plan(*usb.PLAN_REPLY.unpack(plan_payload(mode)))
                output = io.StringIO()
                with redirect_stdout(output):
                    usb.print_plan(plan)
                self.assertIn("Nominal RF outputs=14074000.000 Hz", output.getvalue())
                self.assertEqual(
                    "(disabled for LDIV test)" in output.getvalue(), mode == 2
                )
                self.assertNotIn("Actual RF=", output.getvalue())

    def test_fc0_done_fail_died_and_scaling(self):
        for status, valid in [
            (0, False),
            (0x10, True),
            (0x10010, False),
            (0x10000010, False),
            (0x10010010, False),
            (0x11, True),
        ]:
            with self.subTest(status=status):
                sample = usb.Measurement(status, 123)
                self.assertEqual(sample.valid, valid)
                self.assertEqual(sample.hz, 3843.75 if valid else None)

    def test_non_tty_accepts_enter_or_eof(self):
        for text in ("\n", ""):
            with (
                patch.object(sys, "stdin", io.StringIO(text)),
                redirect_stdout(io.StringIO()),
            ):
                tx_carrier.wait_for_key()

    @unittest.skipIf(sys.platform == "win32", "POSIX terminal handling")
    def test_tty_restored_after_key_or_ctrl_c(self):
        import termios
        import tty

        for error in (None, KeyboardInterrupt):
            stdin = MagicMock()
            stdin.isatty.return_value = True
            stdin.fileno.return_value = 123
            stdin.read.side_effect = error
            with (
                patch.object(sys, "stdin", stdin),
                redirect_stdout(io.StringIO()),
                patch.object(termios, "tcgetattr", return_value=["previous"]),
                patch.object(termios, "tcsetattr") as restore,
                patch.object(tty, "setcbreak") as cbreak,
            ):
                if error:
                    with self.assertRaises(KeyboardInterrupt):
                        tx_carrier.wait_for_key()
                else:
                    tx_carrier.wait_for_key()
                cbreak.assert_called_once_with(123)
                restore.assert_called_once_with(123, termios.TCSADRAIN, ["previous"])

    def test_windows_keypress_mock(self):
        stdin = MagicMock()
        stdin.isatty.return_value = True
        msvcrt = SimpleNamespace(getwch=Mock(return_value="x"))
        with (
            patch.object(sys, "platform", "win32"),
            patch.object(sys, "stdin", stdin),
            patch.dict(sys.modules, {"msvcrt": msvcrt}),
            redirect_stdout(io.StringIO()),
        ):
            tx_carrier.wait_for_key()
            msvcrt.getwch.assert_called_once()
            msvcrt.getwch.return_value = "\x03"
            with self.assertRaises(KeyboardInterrupt):
                tx_carrier.wait_for_key()

    def test_missing_pyusb_has_install_hint(self):
        with patch.dict(sys.modules, {"usb": None}):
            with self.assertRaisesRegex(usb.TxUsbError, "python3-usb"):
                usb._load_pyusb()


if __name__ == "__main__":
    unittest.main()
