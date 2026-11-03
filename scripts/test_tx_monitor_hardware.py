#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Explicit Linux/PipeWire TX playback/IQ acceptance using a digital-mode WAV.

Keys normal audio-driven TX at the current CAT frequency. Requires --run,
transmit enabled and I/Q synthesis enabled. Exercises USB recording, playback,
control and I/Q together, then returns to RX.
"""
import argparse
import json
import pathlib
import struct
import subprocess
import threading
import time
import wave

import numpy as np

from tx_usb import AstraUsb


def analyze(frames, times, statuses, source):
    """Check real delivery timing as well as the synthesized sample level."""
    times = np.asarray(times)
    intervals = []
    start = None
    for timestamp, active in statuses:
        if active and start is None:
            start = timestamp
        elif not active and start is not None:
            intervals.append((start, timestamp))
            start = None
    if start is not None:
        intervals.append((start, statuses[-1][0]))
    if not intervals:
        raise RuntimeError('No transmission was detected during playback')
    start, end = max(intervals, key=lambda interval: interval[1] - interval[0])
    selected = (times > start + .5) & (times < end - .5)
    if selected.sum() < 100:
        raise RuntimeError('Need at least two seconds of sustained TX audio')
    iq = np.array([np.frombuffer(frame[64:], dtype='<i2') for frame in frames])
    rms = np.sqrt(np.mean(iq.astype(float) ** 2, axis=1))
    rate = (selected.sum() - 1) * 512 / np.ptp(times[selected])
    level = float(np.median(rms[selected]))
    source_level = float(np.sqrt(np.mean(source[source != 0].astype(float) ** 2)))
    silent = float(np.mean(rms[selected] == 0))
    result = {'iq_samples_per_second': float(rate), 'median_iq_rms': level,
              'source_rms': source_level, 'silent_frame_fraction': silent,
              'frames': len(frames)}
    if not 118800 <= rate <= 121200 or silent > .01 or level < source_level * .1:
        raise RuntimeError('TX I/Q timing or amplitude failed. ' + json.dumps(result))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='store_true', help='Permit hardware access and audio-driven TX')
    parser.add_argument('--wav', type=pathlib.Path, required=True, help='Mono PCM16, 12 kHz digital-mode audio')
    parser.add_argument('--serial')
    parser.add_argument('--target', help='PipeWire output node, discovered automatically when omitted')
    parser.add_argument('--record-source', help='PipeWire capture node, discovered automatically when omitted')
    args = parser.parse_args()
    if not args.run:
        parser.error('--run is required for this hardware transmission test')
    with wave.open(str(args.wav), 'rb') as audio:
        if (audio.getnchannels(), audio.getsampwidth(), audio.getframerate()) != (1, 2, 12000):
            parser.error('WAV must be mono PCM16 at 12 kHz')
        source = np.frombuffer(audio.readframes(audio.getnframes()), dtype='<i2')
    duration = len(source) / 12000
    if not 3 <= duration <= 180 or not np.any(source):
        parser.error('Need 3–180 seconds of digital-mode audio')
    nodes = json.loads(subprocess.check_output(['pw-dump']))
    names = [node.get('info', {}).get('props', {}).get('node.name', '') for node in nodes]

    def select_node(explicit, prefix, option):
        if explicit:
            return explicit
        matches = [name for name in names if name.startswith(prefix)
                   and (args.serial is None or args.serial in name)]
        if len(matches) != 1:
            parser.error('Select the Astra918 PipeWire node with ' + option)
        return matches[0]

    target = select_node(args.target, 'alsa_output.usb-Astra918_', '--target')
    record_source = select_node(args.record_source, 'alsa_input.usb-Astra918_', '--record-source')
    frames, times, statuses, errors = [], [], [], []
    stop = threading.Event()
    with AstraUsb(args.serial, timeout_ms=15000) as radio:
        initial = radio.command(0x13)
        if not initial[35] or initial[127] & 2:
            raise RuntimeError('The transceiver must be in healthy RX before this test')
        if not initial[62] or initial[64:69] != b'TXIQ\x01':
            raise RuntimeError('Enable transmit and Synthesize I/Q during transmit before testing')
        radio.command(0x31)
        while True:
            try:
                radio.device.read(0x85, 16384, timeout=30)
            except radio._core.USBTimeoutError:
                break
        radio.command(0x30)
        started = time.monotonic()

        def read_iq():
            previous = None
            while not stop.is_set():
                try:
                    frame = bytes(radio.device.read(0x85, 2112, timeout=1000))
                    if len(frame) != 2112 or frame[:4] != b'ASIQ':
                        raise RuntimeError('Invalid I/Q frame')
                    counters = struct.unpack_from('<IIQ', frame, 8)
                    if previous and counters[0] == previous[0]:
                        expected = (previous[0], (previous[1] + 1) & 0xffffffff,
                                    (previous[2] + 512) & 0xffffffffffffffff)
                        if counters != expected:
                            raise RuntimeError('I/Q sequence/sample counters lost continuity')
                    previous = counters
                    frames.append(frame)
                    times.append(time.monotonic() - started)
                except Exception as error:
                    if not stop.is_set():
                        errors.append(str(error))
                    break

        reader = threading.Thread(target=read_iq)
        reader.start()
        playback = None
        recording = None
        try:
            recording = subprocess.Popen(['pw-record', '--target', record_source,
                '--rate', '12000', '--channels', '1', '--format', 's16', '-'],
                stdout=subprocess.DEVNULL)
            time.sleep(.3)
            if recording.poll() is not None:
                raise RuntimeError('PipeWire recording failed')
            playback = subprocess.Popen(['pw-play', '--target', target, str(args.wav)])
            while time.monotonic() - started < duration + 2:
                status = radio.command(0x13)
                statuses.append((time.monotonic() - started, bool(status[127] & 2)))
                if errors:
                    raise RuntimeError('; '.join(errors))
                time.sleep(.1)
            if playback.wait(timeout=2) != 0:
                raise RuntimeError('PipeWire playback failed')
        finally:
            if playback and playback.poll() is None:
                playback.terminate()
                playback.wait(timeout=3)
            try:
                # Let VOX unkey after playback stops, keeping I/Q reads alive.
                for _ in range(30):
                    final = radio.command(0x13)
                    if not final[127] & 2:
                        break
                    time.sleep(.1)
                else:
                    radio.command(0x43, bytes(9))
            except Exception as error:
                errors.append('RX recovery failed: ' + str(error))
            finally:
                try:
                    radio.command(0x31)
                except Exception as error:
                    errors.append('I/Q stop failed: ' + str(error))
                finally:
                    stop.set()
                    reader.join(timeout=2)
                    if recording and recording.poll() is None:
                        recording.terminate()
                        recording.wait(timeout=3)
        final = radio.command(0x13)
        if final[127] & 2 or not final[35] or struct.unpack_from('<I', final, 52)[0]:
            print(json.dumps(analyze(frames, times, statuses, source), indent=2))
            raise RuntimeError('The transceiver did not return to healthy RX. '
                               f'configured={final[35]}, error={struct.unpack_from("<I", final, 52)[0]}')
    if errors:
        raise RuntimeError('; '.join(errors))
    print(json.dumps(analyze(frames, times, statuses, source), indent=2))


if __name__ == '__main__':
    main()
