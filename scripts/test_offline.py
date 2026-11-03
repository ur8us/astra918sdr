#!/usr/bin/env python3
"""Offline cross-process acceptance. Starts only the local simulator."""
import contextlib
import pathlib
import socket
import struct
import subprocess
import tempfile
import time
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]

def exact(sock, n):
    data = bytearray()
    while len(data) < n:
        part = sock.recv(n - len(data))
        if not part:
            raise EOFError("Disconnected")
        data.extend(part)
    return bytes(data)

class Client:
    def __init__(self, port):
        self.socket = socket.create_connection(("127.0.0.1", port), timeout=4)
        self.sequence = 0
    def close(self):
        self.socket.close()
    def command(self, cmd, payload=b"", fragment=256, status=0):
        self.sequence += 1
        raw = bytearray(256)
        struct.pack_into("<4sBBBBIHH", raw, 0, b"AST1", 1, cmd, 0, 0, self.sequence, len(payload), 0)
        raw[16:16 + len(payload)] = payload
        for start in range(0, 256, fragment):
            self.socket.sendall(raw[start:start + fragment])
        result = exact(self.socket, 256)
        assert result[:6] == raw[:6] and result[8:12] == raw[8:12]
        assert result[6] == status, (cmd, result[6], status)
        length = struct.unpack_from("<H", result, 12)[0]
        assert length <= 240 and not any(result[16 + length:])
        return result[16:16 + length]

class Acceptance(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="astra-offline-")
        self.port = 17350
        for port in range(17350, 18350, 4):
            ports = []
            try:
                for p in range(port, port + 4):
                    s = socket.socket(); s.bind(("127.0.0.1", p)); ports.append(s)
                self.port = port
                break
            except OSError:
                pass
            finally:
                for s in ports: s.close()
        self.start()
    def start(self):
        self.process = subprocess.Popen([str(ROOT / "target/release/astra918-sim"), "--port", str(self.port), "--settings", self.directory.name], stdout=subprocess.DEVNULL)
        for _ in range(100):
            try:
                self.client = Client(self.port)
                self.client.command(0x13)
                return
            except (OSError, EOFError): time.sleep(.03)
        self.fail("Simulator did not start")
    def tearDown(self):
        self.client.close(); self.process.terminate(); self.process.wait(timeout=3); self.directory.cleanup()
    def restart(self):
        self.client.close(); self.process.terminate(); self.process.wait(timeout=3); self.start()
    def cat(self, command):
        with socket.create_connection(("127.0.0.1", self.port + 2), timeout=3) as s:
            s.sendall(command)
            data = b""
            while not data.endswith(b";"): data += s.recv(128)
            return data
    def test_bidirectional_frequency_and_offset(self):
        c = self.client
        self.assertEqual(self.cat(b"FW;"), b"FW3400;")
        c.command(0x33, struct.pack("<i", 10000), fragment=7)
        self.assertEqual(self.cat(b"FA00007074049;FA;"), b"FA00007074049;")
        state = c.command(0x13)
        self.assertEqual(struct.unpack_from("<Q", state)[0], 7074049)
        self.assertEqual(struct.unpack_from("<Q", state, 80)[0], 7064049)
        c.command(0x20, struct.pack("<Q", 14074000))
        self.assertEqual(self.cat(b"FA;"), b"FA00014074000;")
        c.command(0x33, struct.pack("<i", -12000))
        self.assertEqual(self.cat(b"FA;"), b"FA00014074000;")
        self.assertEqual(struct.unpack_from("<Q", c.command(0x13), 80)[0], 14086000)
        c.command(0x33, struct.pack("<i", 59000), status=7)
        self.assertEqual(struct.unpack_from("<i", c.command(0x13), 88)[0], -12000)
    def test_iq_stop_retune_metadata_and_audio(self):
        with socket.create_connection(("127.0.0.1", self.port+1), timeout=3) as iq, socket.create_connection(("127.0.0.1", self.port+3), timeout=3) as audio:
            self.client.command(0x30)
            frame=exact(iq,2112);self.assertEqual(frame[:4],b"ASIQ")
            gen=struct.unpack_from("<I",frame,8)[0]
            self.cat(b"FA00007074000;FA;")
            for _ in range(150):
                frame=exact(iq,2112)
                if struct.unpack_from("<I",frame,8)[0]!=gen: break
            self.assertEqual(struct.unpack_from("<Q",frame,40)[0],7074000)
            self.client.command(0x31)
            state=self.client.command(0x13);self.assertEqual(state[34:36],bytes([0,1]))
            self.assertTrue(any(exact(audio,2400)))
            self.assertEqual(self.cat(b"FA;"),b"FA00007074000;")
    def test_save_and_interrupted_save_recovery(self):
        self.client.command(0x20,struct.pack("<Q",7074000));self.client.command(0x36)
        self.client.command(0x20,struct.pack("<Q",14074000));self.restart()
        self.assertEqual(struct.unpack_from("<Q",self.client.command(0x13))[0],7074000)
        self.client.command(0x20,struct.pack("<Q",21074000));self.client.command(0x36)
        # Corrupt only the new simulator save, retaining the prior valid slot.
        path=pathlib.Path(self.directory.name)/"slot0.bin"
        path.write_bytes(path.read_bytes()[:256])
        self.restart();self.assertEqual(struct.unpack_from("<Q",self.client.command(0x13))[0],7074000)
    def test_combined_iq_tx_settings_save_restart_and_factory_reset(self):
        self.client.command(0x3e, bytes([3, 2]))
        self.client.command(0x4a, struct.pack("<BbBB", 0, -52, 17, 83))
        self.client.command(0x49, struct.pack("<H", 91))
        self.client.command(0x46, bytes([8]))
        self.client.command(0x47, bytes([0]))
        self.client.command(0x48, bytes([1]))
        self.client.command(0x4b, bytes([1]))
        self.client.command(0x20, struct.pack("<Q", 14074037))
        self.client.command(0x36)
        self.restart()
        state = self.client.command(0x13)
        self.assertEqual(state[120], 255)
        self.assertEqual(state[123], 2)
        self.assertEqual(state[62:64], bytes([0, 204]))
        self.assertEqual(state[125:127], bytes([17, 83]))
        self.assertEqual(struct.unpack_from("<H", state, 60)[0], 91)
        self.assertEqual(state[93], 8)
        self.assertEqual(state[127] & 160, 128)
        self.assertEqual(state[64:69], b"TXIQ\x01")
        self.assertEqual(struct.unpack_from("<Q", state, 80)[0], 14074037)
        self.assertEqual(state[96:100], state[112:116])
        calibrated = self.client.command(0x42)
        self.assertEqual(calibrated[124], 2)
        reset = self.client.command(0x3f)
        self.assertEqual(reset[62:64], bytes([1, 216]))
        self.assertEqual(reset[123], 1)
        self.assertEqual(reset[64:69], b"TXIQ\x00")
        self.assertEqual(struct.unpack_from("<H", reset, 60)[0], 50)
        self.restart()
        self.assertEqual(self.client.command(0x13)[123], 1)
    def test_hardware_failure_simulation_and_recovery(self):
        self.client.command(0x70,b"\x01")
        self.assertEqual(self.cat(b"FA00007074000;"),b"?;")
        state=self.client.command(0x13);self.assertEqual(state[35],0)
        self.assertEqual(self.cat(b"FA;"),b"FA00014200000;")
        self.client.command(0x37);self.assertEqual(self.client.command(0x13)[35],1)
    def test_tx_monitor_toggles_without_interrupting_iq(self):
        import cmath
        import math
        self.client.command(0x33, struct.pack('<i', 10000))
        self.client.command(0x34, bytes([1]))
        with socket.create_connection(('127.0.0.1', self.port + 1), timeout=3) as iq:
            self.client.command(0x30)
            self.client.command(0x70, bytes([4]))
            previous = None
            for enabled in [True, False, True]:
                self.client.command(0x4b, bytes([enabled]))
                samples = []
                for index in range(90):
                    frame = exact(iq, 2112)
                    counters = struct.unpack_from('<IIQ', frame, 8)
                    if previous is not None:
                        self.assertEqual(counters, (previous[0], previous[1] + 1, previous[2] + 512))
                    previous = counters
                    if index >= 66:
                        samples.extend(complex(i, q) for i, q in struct.iter_unpack('<hh', frame[64:]))
                if enabled:
                    def level(frequency):
                        return abs(sum(v * cmath.exp(-2j * math.pi * frequency * n / 120000)
                                       for n, v in enumerate(samples))) / len(samples)
                    wanted = level(8500)
                    self.assertGreater(wanted, 5000)
                    self.assertLess(level(11500), wanted / 50)
                else:
                    self.assertTrue(all(v == 0 for v in samples))
                state = self.client.command(0x13)
                self.assertEqual(state[34:36], bytes([1, 1]))
                self.assertEqual(state[68], enabled)
            self.client.command(0x70, bytes([0]))
            self.assertEqual(self.client.command(0x13)[34:36], bytes([1, 1]))
    def test_only_one_vendor_owner(self):
        with contextlib.closing(Client(self.port)) as second:
            with self.assertRaises((EOFError,OSError)): second.command(0x13)
        self.assertEqual(self.cat(b"ID;"),b"ID020;")
        self.client.command(0x13)
    def test_cpp_client(self):
        binary=ROOT.parent/"astra918sdr-sdrpp/build/astra918_sim_test"
        if not binary.exists(): self.skipTest("Build C++ client first")
        self.client.close();time.sleep(.1)
        result=subprocess.run([str(binary),f"127.0.0.1:{self.port}"],text=True,capture_output=True,timeout=10)
        self.assertEqual(result.returncode,0,result.stderr)
    def test_sdrpp_module_lifecycle(self):
        directory=ROOT.parent/"astra918sdr-sdrpp/build"
        if not (directory/"astra918_module_test").exists(): self.skipTest("Build SDR++ module first")
        self.client.close();time.sleep(.1)
        result=subprocess.run([str(directory/"astra918_module_test"),str(directory/"astra918_source.so"),f"127.0.0.1:{self.port}"],text=True,capture_output=True,timeout=30)
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)

if __name__=="__main__": unittest.main(verbosity=2)
