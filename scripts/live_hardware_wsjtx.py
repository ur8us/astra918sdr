#!/usr/bin/env python3
"""Run isolated receive-only WSJT-X against a physical Astra918 on Linux.

Requires the receiver's PipeWire source and CAT device. Never changes normal
WSJT-X settings, the default audio route, or receiver flash. No simulated input.
"""
import argparse
import glob
import json
import os
import pathlib
import signal
import socket
import struct
import subprocess
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--serial', required=True)
    p.add_argument('--mode', choices=['FT8', 'FT4', 'WSPR'], default='FT8')
    p.add_argument('--frequency', type=int, default=14074000)
    p.add_argument('--seconds', type=int, default=600)
    p.add_argument('--udp-port', type=int, default=22381)
    args = p.parse_args()
    cat = glob.glob(f'/dev/serial/by-id/*{args.serial}*-if02')
    assert len(cat) == 1, cat
    source = f'alsa_input.usb-Astra918_project_Astra918_Audio_CAT_SDR_{args.serial}-00.mono-fallback'
    nodes = json.loads(subprocess.check_output(['pw-dump'], text=True))
    assert any(n.get('info', {}).get('props', {}).get('node.name') == source for n in nodes), source
    directory = pathlib.Path(tempfile.mkdtemp(prefix=f'hardware-wsjtx-{args.mode}-', dir=ROOT/'artifacts'))
    config, data = directory/'config', directory/'data'
    config.mkdir(); data.mkdir()
    (config/'WSJT-X - AstraHardware.ini').write_text(f'''[Configuration]
MyCall=W1AW
MyGrid=FN42
Rig=Kenwood TS-480
CATSerialPort={cat[0]}
CATSerialRate=115200
Polling=1
SoundInName={source}
AudioInputChannel=Mono
PSKReporter=false
UploadSpots=false
UDPServer=127.0.0.1
UDPServerPort={args.udp_port}
AcceptUDPRequests=true
MonitorLastUsed=true
[Common]
Mode={args.mode}
DialFreq={args.frequency}
PctTx=0
UploadSpots=false
BandHopping=false
[WideGraph]
BinsPerPixel=4
''')
    env = dict(os.environ, XDG_CONFIG_HOME=str(config), XDG_DATA_HOME=str(data),
               QT_QPA_PLATFORM='xcb', PULSE_SOURCE=source)
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind(('127.0.0.1', args.udp_port)); udp.settimeout(1)
    app = subprocess.Popen(['wsjtx', '--rig-name=AstraHardware'], env=env,
                           stdout=(directory/'app.log').open('w'), stderr=subprocess.STDOUT,
                           start_new_session=True)
    print(f'WSJT-X PID {app.pid}; physical source {source}; logs {directory}', flush=True)
    deadline = time.monotonic()+args.seconds
    try:
        with (directory/'udp.jsonl').open('w') as log:
            while time.monotonic() < deadline and app.poll() is None:
                try: packet, address = udp.recvfrom(65536)
                except socket.timeout: continue
                if len(packet) < 16: continue
                kind, length = struct.unpack_from('>II', packet, 8)
                payload = packet[16+length:]
                entry = {'time': time.time(), 'type': kind, 'payload_hex': payload.hex()}
                if kind == 0:
                    udp.sendto(packet[:16+length] + struct.pack('>II', 3, 5) + b'Astra' + struct.pack('>I', 1) + b'1', address)
                elif kind == 1 and len(payload) >= 8:
                    entry['dial'] = struct.unpack_from('>Q', payload)[0]
                elif kind == 2:
                    entry['decode'] = repr(payload)
                log.write(json.dumps(entry)+'\n'); log.flush()
                if kind in (1, 2): print(entry, flush=True)
    finally:
        # WSJT-X starts a jt9 decoder. End this isolated process group too,
        # including a decoder whose GUI parent has already exited.
        try: os.killpg(app.pid, signal.SIGTERM)
        except ProcessLookupError: pass
        try: app.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(app.pid, signal.SIGKILL); app.wait()
        try: os.killpg(app.pid, signal.SIGKILL)
        except ProcessLookupError: pass
        udp.close()
    print(f'Evidence retained in {directory}', flush=True)


if __name__ == '__main__':
    main()
