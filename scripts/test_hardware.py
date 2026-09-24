#!/usr/bin/env python3
"""Explicit, receive-only Linux hardware acceptance (PyUSB, pyserial, NumPy).

Never flashes, saves settings, resets USB, detaches drivers or transmits RF.
Claims only vendor interface 4. Use --controls for reversible receiver edits.
"""
import argparse
import glob
import json
import pathlib
import struct
import subprocess
import threading
import time

import numpy as np
import serial
import usb.core
import usb.util


class Receiver:
    def __init__(self, identity):
        self.device = next((d for d in usb.core.find(find_all=True, idVendor=0xc0de,
                           idProduct=0x091a) if d.serial_number == identity), None)
        if self.device is None:
            raise RuntimeError(f'Receiver {identity} not found or inaccessible')
        assert self.device.get_active_configuration().bConfigurationValue == 1
        usb.util.claim_interface(self.device, 4)
        self.sequence = 0

    def command(self, command, payload=b'', expected=0):
        self.sequence += 1
        raw = bytearray(256)
        struct.pack_into('<4sBBBBIHH', raw, 0, b'AST1', 1, command, 0, 0,
                         self.sequence, len(payload), 0)
        raw[16:16+len(payload)] = payload
        assert self.device.write(0x03, raw, timeout=5000) == 256
        reply = bytearray()
        while len(reply) < 256:
            reply.extend(self.device.read(0x84, 256-len(reply), timeout=5000))
        assert reply[:6] == raw[:6] and reply[8:12] == raw[8:12], reply.hex()
        assert reply[6] == expected, (hex(command), reply[6], expected)
        return bytes(reply[16:16+struct.unpack_from('<H', reply, 12)[0]])

    def state(self):
        p = self.command(0x13)
        assert len(p) == 128
        values = {name: struct.unpack_from(fmt, p, offset)[0] for name, fmt, offset in [
            ('dial', '<Q', 0), ('center', '<Q', 80), ('offset', '<i', 88),
            ('generation', '<I', 36), ('dropped', '<I', 40), ('capture_faults', '<I', 44),
            ('usb_faults', '<I', 48), ('error', '<I', 52), ('revision', '<I', 96),
            ('underruns', '<I', 100), ('overruns', '<I', 104), ('audio_stalls', '<I', 108),
            ('saved_revision', '<I', 112), ('low', '<H', 94), ('high', '<H', 116),
            ('capacitor', '<H', 76)]}
        values.update({name: p[offset] for name, offset in [('input', 28), ('rf_manual', 30),
                       ('if_manual', 31), ('rf_gain', 32), ('if_gain', 33), ('streaming', 34),
                       ('configured', 35), ('lf_gain', 78), ('lf_attenuator', 79), ('mode', 92)]})
        assert values['center'] == values['dial'] - values['offset']
        return values

    def close(self):
        usb.util.release_interface(self.device, 4)
        usb.util.dispose_resources(self.device)

    def stop_iq(self):
        self.command(0x31)
        deadline = time.monotonic() + 1
        while True:
            try:
                self.device.read(0x85, 16384, timeout=30)
            except usb.core.USBTimeoutError:
                return
            assert time.monotonic() < deadline, 'I/Q did not stop'


def cat(port, command):
    port.reset_input_buffer()
    port.write(command.encode())
    reply = port.read_until(b';').decode()
    assert reply.endswith(';') and reply != '?;', (command, reply)
    return reply


def restore(receiver, s):
    # Offset zero first permits restoring any previously valid dial/sideband.
    receiver.command(0x33, struct.pack('<i', 0))
    receiver.command(0x20, struct.pack('<Q', s['dial']))
    receiver.command(0x34, bytes([s['mode']]))
    receiver.command(0x35, struct.pack('<HH', s['low'], s['high']))
    receiver.command(0x33, struct.pack('<i', s['offset']))
    current = receiver.state()
    for block, name in enumerate(['rf_gain', 'if_gain', 'lf_gain', 'lf_attenuator']):
        if current[name] != s[name]:
            if block >= 2:
                receiver.command(0x24, b'\x01')
            receiver.command(0x26, bytes([int(block == 1), 1]))
            receiver.command(0x27, bytes([block, s[name]]))
    receiver.command(0x24, bytes([s['input']]))
    for block, name in enumerate(['rf_manual', 'if_manual']):
        receiver.command(0x26, bytes([block, s[name]]))
    receiver.command(0x2d, struct.pack('<H', s['capacitor']))


def controls(receiver, port):
    initial = receiver.state()
    try:
        for offset in [-45000, 0, 45000]:
            receiver.command(0x33, struct.pack('<i', offset))
            assert receiver.state()['dial'] == initial['dial']
            for mode in [1, 2]:
                receiver.command(0x34, bytes([mode]))
                assert cat(port, 'MD;') == f'MD{mode};'
                assert cat(port, 'FA00007074049;FA;') == 'FA00007074049;'
                s = receiver.state()
                assert s['dial'] == 7074049 and s['offset'] == offset
                receiver.command(0x20, struct.pack('<Q', initial['dial']))
                assert cat(port, 'FA;') == f"FA{initial['dial']:011d};"
        receiver.command(0x33, struct.pack('<i', 59000), expected=7)
        receiver.command(0x39, struct.pack('<Q', 14074000))
        assert receiver.state()['offset'] == 0
        for mode in [1, 2]:
            receiver.command(0x34, bytes([mode]))
            for offset in [-45049, 0, 45049]:
                dial = 14074000 + offset
                receiver.command(0x38, struct.pack('<Q', dial))
                s = receiver.state()
                assert s['center'] == 14074000 and s['offset'] == offset
                assert cat(port, 'FA;') == f'FA{dial:011d};'
        receiver.command(0x38, struct.pack('<Q', 14574000), expected=7)
        for rf_input in range(4):
            receiver.command(0x24, bytes([rf_input]))
            assert receiver.state()['input'] == rf_input
        receiver.command(0x24, bytes([initial['input']]))
        for block in range(2):
            for manual in [1, 0]:
                receiver.command(0x26, bytes([block, manual]))
                assert receiver.state()[['rf_manual', 'if_manual'][block]] == manual
        receiver.command(0x24, b'\x01')
        for block in range(2):
            receiver.command(0x26, bytes([block, 1]))
        for block, name in enumerate(['rf_gain', 'if_gain', 'lf_gain', 'lf_attenuator']):
            receiver.command(0x27, bytes([block, 3]))
            assert receiver.state()[name] == 3
        receiver.command(0x35, struct.pack('<HH', 300, 3000))
        assert cat(port, 'FW;') == 'FW2700;'
        receiver.command(0x2d, struct.pack('<H', 123))
        assert receiver.state()['capacitor'] == 123
        assert receiver.state()['saved_revision'] == initial['saved_revision']
    finally:
        restore(receiver, initial)
        restored = receiver.state()
        for name in ['dial', 'offset', 'mode', 'low', 'high', 'input', 'rf_manual',
                     'if_manual', 'rf_gain', 'if_gain', 'lf_gain', 'lf_attenuator', 'capacitor']:
            assert restored[name] == initial[name], (name, initial, restored)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--serial', required=True)
    parser.add_argument('--seconds', type=int, default=30)
    parser.add_argument('--controls', action='store_true')
    parser.add_argument('--stall', action='store_true')
    parser.add_argument('--audio-backend', choices=['alsa', 'pipewire'], default='alsa')
    parser.add_argument('--output', type=pathlib.Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    paths = glob.glob(f'/dev/serial/by-id/*{args.serial}*-if02')
    assert len(paths) == 1, paths
    receiver = Receiver(args.serial)
    port = serial.Serial(paths[0], 115200, timeout=2)
    recording = None
    thread = None
    stop = threading.Event()
    result = {'serial': args.serial, 'initial': receiver.state(), 'audio_backend': args.audio_backend}
    try:
        assert result['initial']['configured'] and not result['initial']['error'], result
        assert cat(port, 'ID;') == 'ID020;'
        receiver.stop_iq()
        if args.controls:
            controls(receiver, port)
            result['controls'] = 'passed'
        if args.audio_backend == 'pipewire':
            source = f'alsa_input.usb-Astra918_project_Astra918_Audio_CAT_SDR_{args.serial}-00.mono-fallback'
            with (args.output/'audio.s16').open('wb') as audio:
                recording = subprocess.Popen(['pw-record', '--target', source, '--rate', '12000',
                    '--channels', '1', '--format', 's16', '-'], stdout=audio,
                    stderr=(args.output/'audio.log').open('w'))
        else:
            recording = subprocess.Popen(['arecord', '-q', '-D', 'hw:CARD=SDR,DEV=0',
                '-f', 'S16_LE', '-r', '12000', '-c', '1', '-t', 'raw',
                str(args.output/'audio.s16')], stderr=(args.output/'audio.log').open('w'))
        time.sleep(1)
        assert recording.poll() is None, 'Audio capture failed'
        receiver.command(0x30)
        result['baseline'] = receiver.state()
        frames = []
        errors = []
        def receive():
            pending = bytearray()
            previous = None
            try:
                while not stop.is_set():
                    pending.extend(receiver.device.read(0x85, 2112-len(pending), timeout=2000))
                    if len(pending) != 2112:
                        continue
                    assert pending[:4] == b'ASIQ', pending[:64].hex()
                    gen, seq, sample, rate, count = struct.unpack_from('<IIQIH', pending, 8)
                    assert rate == 120000 and count == 512
                    if previous and gen == previous[0]:
                        assert seq == (previous[1]+1) & 0xffffffff
                        assert sample == previous[2]+512
                    previous = (gen, seq, sample)
                    values = np.frombuffer(pending, dtype='<i2', offset=64).astype(np.float64)
                    frames.append((time.monotonic(), float(np.sqrt(np.mean(values*values)))))
                    pending.clear()
            except Exception as error:
                if not stop.is_set(): errors.append(repr(error))
        thread = threading.Thread(target=receive)
        thread.start()
        start = time.monotonic()
        with (args.output/'status.jsonl').open('w') as log:
            while time.monotonic()-start < args.seconds:
                s = receiver.state()
                log.write(json.dumps({'seconds': time.monotonic()-start, **s})+'\n'); log.flush()
                assert cat(port, 'FA;') == f"FA{s['dial']:011d};"
                assert s['configured'] and s['streaming'] and not s['error'], s
                assert not errors, errors
                assert recording.poll() is None, 'Audio capture exited'
                time.sleep(1)
        result['final'] = receiver.state()
        stop.set(); thread.join(timeout=3)
        assert not thread.is_alive() and not errors, errors
        result['frames'] = len(frames)
        result['iq_rate'] = (len(frames)-1)*512/(frames[-1][0]-frames[0][0])
        result['iq_rms_mean'] = float(np.mean([r for _, r in frames]))
        assert abs(result['iq_rate']/120000-1) < .01, result['iq_rate']
        for name in ['capture_faults', 'dropped', 'usb_faults', 'underruns', 'overruns', 'audio_stalls']:
            assert result['final'][name] == result['baseline'][name], (name, result)
        if args.stall:
            time.sleep(.5)  # No I/Q reads: deliberately fill the bounded queue.
            s = receiver.state()
            assert not s['streaming'] and s['configured'] and not s['error'], s
            assert cat(port, 'FA;') == f"FA{s['dial']:011d};"
            assert s['dropped'] > result['baseline']['dropped']
            assert s['usb_faults'] > result['baseline']['usb_faults']
            for name in ['capture_faults', 'underruns', 'overruns', 'audio_stalls']:
                assert s[name] == result['baseline'][name], (name, s)
            result['stall'] = s
        receiver.stop_iq()
        before = (args.output/'audio.s16').stat().st_size
        time.sleep(2)
        assert (args.output/'audio.s16').stat().st_size > before
        assert cat(port, 'ID;') == 'ID020;'
        result['stopped'] = receiver.state()
        result['audio_after_iq_stop'] = 'passed'
        result['passed'] = True
    except BaseException as error:
        result['passed'] = False
        result['error'] = repr(error)
        raise
    finally:
        stop.set()
        if thread is not None:
            thread.join(timeout=3)
        if recording is not None:
            recording.terminate(); recording.wait(timeout=5)
            path = args.output/'audio.s16'
            audio = np.fromfile(path, dtype='<i2').astype(np.float64) if path.exists() else np.array([])
            result['audio_samples'] = len(audio)
            result['audio_rms'] = float(np.sqrt(np.mean(audio*audio))) if len(audio) else 0
        try:
            receiver.stop_iq()
        except Exception as error:
            result['cleanup_error'] = repr(error)
        finally:
            receiver.close(); port.close()
        (args.output/'result.json').write_text(json.dumps(result, indent=2)+'\n')
        print(json.dumps(result, indent=2), flush=True)


if __name__ == '__main__':
    main()
