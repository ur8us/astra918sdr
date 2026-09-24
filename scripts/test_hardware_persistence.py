#!/usr/bin/env python3
"""Explicit RP2350A/4 MiB persistence acceptance using USB and a selected SWD probe.

Close receiver applications first. This test SAVES settings and RESETS the MCU.
--torn-commit additionally erases only the newest record's commit page through
SWD, preserving the surrounding sector. It models an interrupted save; it is
not a timed power-cut test. Backups and logs are retained before any mutation.
The original receiver settings are explicitly saved again on successful exit.
"""
import argparse
import json
import pathlib
import re
import struct
import subprocess
import time
import zlib

from test_hardware import Receiver, restore

BASE = 0x103fe000
FIELDS = ('dial', 'offset', 'mode', 'low', 'high', 'input', 'rf_manual',
          'if_manual', 'rf_gain', 'if_gain', 'lf_gain', 'lf_attenuator', 'capacitor')


def settings(state):
    return {key: state[key] for key in FIELDS}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--serial', required=True)
    p.add_argument('--probe', required=True)
    p.add_argument('--output', type=pathlib.Path, required=True)
    p.add_argument('--torn-commit', action='store_true')
    args = p.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)

    def probe(action, name, extra=()):
        command = ['probe-rs', action, '--chip', 'RP235x', '--probe', args.probe,
                   '--speed', '1000', *extra]
        run = subprocess.run(command, capture_output=True, text=True, timeout=120)
        (args.output/f'{name}.stdout').write_text(run.stdout)
        (args.output/f'{name}.stderr').write_text(run.stderr)
        run.check_returncode()
        return run.stdout

    def flash_snapshot(name):
        raw = probe('read', name, ['b32', hex(BASE), '2048'])
        words = re.findall(r'\b[0-9a-fA-F]{8}\b', raw)
        assert len(words) == 2048, f'Unexpected SWD output: {len(words)} words'
        data = b''.join(struct.pack('<I', int(word, 16)) for word in words)
        (args.output/f'{name}.bin').write_bytes(data)
        return data

    def connect():
        deadline = time.monotonic() + 15
        while True:
            receiver = None
            try:
                receiver = Receiver(args.serial)
                state = receiver.state()
                assert state['configured'] and not state['error'], state
                return receiver
            except Exception:
                if receiver is not None:
                    receiver.close()
                if time.monotonic() >= deadline:
                    raise
                time.sleep(.3)

    def reboot(name):
        probe('reset', name)
        time.sleep(1)
        return connect()

    def record(name, state):
        (args.output/f'{name}.json').write_text(json.dumps(state, indent=2)+'\n')
        print(name, settings(state), flush=True)

    receiver = connect()
    initial = receiver.state()
    assert not initial['streaming'], 'Stop I/Q and close controllers first'
    record('initial', initial)
    receiver.close()
    # Readback backs up BOTH reserved sectors, including any legacy data.
    flash_snapshot('before')
    receiver = connect()
    a = dict(initial, dial=7074049, offset=-12000, mode=1, low=300, high=2800,
             input=1, rf_manual=1, if_manual=1, rf_gain=5, if_gain=7,
             lf_gain=3, lf_attenuator=2, capacitor=321)
    b = dict(initial, dial=14074000, offset=45000, mode=2, low=100, high=3500,
             input=2, rf_manual=0, if_manual=0, capacitor=987)
    try:
        restore(receiver, a)
        receiver.command(0x36)
        saved = receiver.state()
        assert saved['saved_revision'] == saved['revision']
        record('saved-a', saved)
        restore(receiver, b)
        assert receiver.state()['saved_revision'] == saved['saved_revision']
        receiver.close(); receiver = None
        receiver = reboot('reset-unsaved-b')
        state = receiver.state()
        assert settings(state) == settings(a), state
        record('restored-a', state)
        restore(receiver, b)
        receiver.command(0x36)
        record('saved-b', receiver.state())
        receiver.close(); receiver = None
        data = flash_snapshot('two-valid-records')
        records = []
        for slot in range(2):
            body = data[slot*4096:slot*4096+256]
            commit = data[slot*4096+256:slot*4096+512]
            assert body[:4] == b'ASNV' and commit[:4] == b'DONE'
            assert body[252:] == commit[4:8] == struct.pack('<I', zlib.crc32(body[:252]))
            records.append(struct.unpack_from('<I', body, 8)[0])
        if args.torn_commit:
            newest = int(0 < (records[1]-records[0]) % (2**32) < 2**31)
            address = BASE + newest*4096 + 256
            blank = args.output/'missing-commit.bin'
            blank.write_bytes(b'\xff'*256)
            probe('download', 'inject-missing-commit', ['--binary-format', 'bin',
                  '--base-address', hex(address), '--restore-unwritten', '--verify', str(blank)])
            after = flash_snapshot('after-injection')
            expected = bytearray(data)
            expected[newest*4096+256:newest*4096+512] = b'\xff'*256
            assert after == expected, 'SWD changed bytes outside the selected commit page'
            receiver = reboot('reset-torn-record')
            state = receiver.state()
            assert settings(state) == settings(a), state
            record('fallback-a', state)
        else:
            receiver = reboot('reset-saved-b')
            assert settings(receiver.state()) == settings(b)
        restore(receiver, initial)
        receiver.command(0x36)
        receiver.close(); receiver = None
        receiver = reboot('reset-restored-original')
        state = receiver.state()
        assert settings(state) == settings(initial), state
        record('final', state)
        (args.output/'result.json').write_text(json.dumps({
            'passed': True, 'torn_commit_injected': args.torn_commit,
            'physical_power_cycle': False, 'final_settings': settings(state)}, indent=2)+'\n')
    finally:
        if receiver is not None:
            receiver.close()


if __name__ == '__main__':
    main()
