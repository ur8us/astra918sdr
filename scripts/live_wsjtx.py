#!/usr/bin/env python3
"""Linux desktop acceptance helper: isolated WSJT-X + temporary PipeWire audio.

Run test_weak_signals.py first, then this script. Close the isolated WSJT-X
window to clean up. Normal WSJT-X settings and default audio routes are untouched.
Only loopback sockets and a newly allocated PTY are used.
"""
import argparse
import os
import pathlib
import re
import socket
import struct
import subprocess
import threading
import time
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]

def main():
    p = argparse.ArgumentParser()
    p.add_argument('--mode',choices=['FT8','FT4','WSPR'],default='FT8')
    p.add_argument('--port',type=int,default=18350)
    p.add_argument('--verify',action='store_true',help='Exit successfully after a live decode and CAT frequency readback')
    args = p.parse_args()
    (ROOT/'artifacts').mkdir(exist_ok=True)
    directory = pathlib.Path(tempfile.mkdtemp(prefix=f'wsjtx-live-{args.mode}-',dir=ROOT/'artifacts'))
    config = directory/'config'; config.mkdir(exist_ok=True)
    data = directory/'data'; data.mkdir(exist_ok=True)
    env = dict(os.environ, XDG_CONFIG_HOME=str(config), XDG_DATA_HOME=str(data),
               QT_QPA_PLATFORM='xcb', PULSE_SOURCE='astra_offline_source')
    children = []
    def launch(command, **kwargs):
        child = subprocess.Popen([str(s) for s in command],**kwargs); children.append(child); return child
    try:
        virtual = launch(['pw-loopback','--channels','1','-m','MONO',
              '--capture-props','media.class=Audio/Sink node.name=astra_offline_sink node.description="Astra Offline Input"',
              '--playback-props','media.class=Audio/Source node.name=astra_offline_source node.description="Astra Offline Receiver"'])
        sim = launch([ROOT/'target/release/astra918-sim','--port',str(args.port),'--cat-pty',
              '--settings',directory/'receiver','--audio-wav',ROOT/f'artifacts/weak-signals/{args.mode}_000000.wav',
              '--utc-align'],stdout=subprocess.PIPE,text=True,bufsize=1)
        pty = None
        while pty is None:
            line = sim.stdout.readline()
            if not line: raise RuntimeError('Simulator failed')
            print(line,end='',flush=True)
            match = re.search(r'/dev/pts/\d+',line)
            if match: pty = match[0]
        profile = f'''[Configuration]
MyCall=W1AW
MyGrid=FN42
Rig=Kenwood TS-480
CATSerialPort={pty}
CATSerialRate=115200
Polling=1
SoundInName=astra_offline_source
AudioInputChannel=Mono
PSKReporter=false
UDPServer=127.0.0.1
UDPServerPort=22380
AcceptUDPRequests=true
MonitorLastUsed=false
[Common]
Mode={args.mode}
[WideGraph]
BinsPerPixel=4
[MainWindow]
DialFreq=14074000
'''
        (config/'WSJT-X - AstraOffline.ini').write_text(profile)
        udp = socket.socket(socket.AF_INET,socket.SOCK_DGRAM); udp.bind(('127.0.0.1',22380))
        decoded = threading.Event()
        frequency_seen = threading.Event()
        def log_udp():
            with (directory/'udp.log').open('w') as log:
                while True:
                    b,address = udp.recvfrom(65536)
                    if len(b)<16: continue
                    kind = struct.unpack_from('>I',b,8)[0]
                    length = struct.unpack_from('>I',b,12)[0]
                    payload = b[16+length:]
                    if kind == 0:
                        udp.sendto(b[:8]+struct.pack('>I',0)+b[12:16+length]+struct.pack('>I',3)+struct.pack('>I',5)+b'Astra'+struct.pack('>I',1)+b'1',address)
                    if kind == 1 and len(payload)>=8:
                        dial = struct.unpack_from('>Q',payload)[0]
                        if dial == 14074049: frequency_seen.set()
                        text = f'Status dial={dial}'
                    else: text = f'UDP type={kind} payload={payload!r}'
                    if kind == 2 and b'K1ABC FN42' in payload: decoded.set()
                    print(text,flush=True); log.write(text+'\n'); log.flush()
        threading.Thread(target=log_udp,daemon=True).start()
        audio = socket.create_connection(('127.0.0.1',args.port+3))
        player = launch(['pw-cat','--playback','--target','astra_offline_sink','--rate','12000',
                        '--channels','1','--format','s16','-'],stdin=subprocess.PIPE)
        def pipe():
            try:
                while b:=audio.recv(4096): player.stdin.write(b); player.stdin.flush()
            except (BrokenPipeError,OSError): pass
        threading.Thread(target=pipe,daemon=True).start()
        time.sleep(1)
        if virtual.poll() is not None or player.poll() is not None:
            raise RuntimeError('Temporary audio route failed to start')
        log = (directory/'wsjtx.log').open('w')
        app = launch(['wsjtx','--rig-name=AstraOffline'],env=env,stdout=log,stderr=log)
        print(f'Isolated WSJT-X PID {app.pid}; simulator 127.0.0.1:{args.port}; logs {directory}',flush=True)
        if args.verify:
            deadline = time.monotonic()+300
            while not decoded.is_set() and app.poll() is None and time.monotonic()<deadline:
                wspr = data/'WSJT-X - AstraOffline/ALL_WSPR.TXT'
                if args.mode == 'WSPR' and wspr.exists() and 'K1ABC FN42 33' in wspr.read_text(): decoded.set()
                time.sleep(.2)
            if not decoded.is_set(): raise RuntimeError('No live decode within 300 s')
            from test_offline import Client
            client = Client(args.port)
            try: client.command(0x20, struct.pack('<Q',14074049))
            finally: client.close()
            if not frequency_seen.wait(10): raise RuntimeError('WSJT-X did not adopt external CAT frequency')
            print(f'PASS: live {args.mode} decoded K1ABC FN42 and WSJT-X adopted 14074049 Hz',flush=True)
        else:
            app.wait()
    finally:
        for child in reversed(children):
            if child.poll() is None: child.terminate()
        for child in children:
            try: child.wait(timeout=3)
            except subprocess.TimeoutExpired: child.kill(); child.wait()

if __name__=='__main__': main()
