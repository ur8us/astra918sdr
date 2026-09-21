#!/usr/bin/env python3
"""Software samples only. Needs WSJT-X v2.7.0 source and installed jt9/wsprd.

Builds upstream encoders in artifacts; their GPL source is not copied into this
repository. Pass --source /path/to/WSJTX. No RF hardware or generator is used.
"""
import argparse
import pathlib
import subprocess
import wave
import numpy as np
from scipy.special import erf

ROOT = pathlib.Path(__file__).resolve().parents[1]
OUT = ROOT / 'artifacts/weak-signals'

def run(args, **kwargs):
    return subprocess.run([str(a) for a in args], check=True, **kwargs)

def build(source):
    lib = source / 'lib'
    files = ['packjt.f90', '77bit/packjt77.f90', 'crc.f90', 'ft8/encode174_91.f90',
             'ft8/genft8.f90', 'ft4/genft4.f90', 'chkcall.f90', 'grid2deg.f90',
             'deg2grid.f90', 'fmtmsg.f90']
    run(['c++', '-O2', '-c', lib/'crc14.cpp', '-o', OUT/'crc14.o'])
    run(['gfortran', '-O2', '-fallow-argument-mismatch', '-I'+str(lib),
         '-I'+str(lib/'ft8'), '-I'+str(lib/'ft4'), *[lib/f for f in files],
         ROOT/'scripts/weak_signal_tones.f90', OUT/'crc14.o', '-lstdc++', '-o', OUT/'tones'], cwd=OUT)
    run(['cc', '-O2', *[lib/'wsprd'/f for f in ['wsprsim.c', 'wsprsim_utils.c',
         'wsprd_utils.c', 'fano.c', 'tab.c', 'nhash.c']], '-lm', '-o', OUT/'wsprsim'])

def wav(path, values):
    with wave.open(str(path), 'wb') as f:
        f.setparams((1,2,12000,len(values),'NONE','not compressed'))
        f.writeframes(np.asarray(values, dtype='<i2').tobytes())

def generate(mode):
    if mode == 'WSPR':
        r = subprocess.run([str(OUT/'wsprsim'), '-c', '-s', '99', '-o', str(OUT/'test.c2'), 'K1ABC FN42 33'], capture_output=True, text=True)
        # Upstream wsprsim returns 1 after successfully writing the file.
        text = r.stdout + r.stderr
        (OUT/'wspr-encoder.txt').write_text(text)
        import re
        candidates = re.findall(r'[0-3](?:\s+[0-3]){161}', text)
        if not candidates:
            candidates = re.findall(r'[0-3]{162}', text)
        assert candidates, text
        tones = np.array([int(c) for c in candidates[0] if c in '0123'])
        frequency = 1500 + (np.repeat(tones,8192)-1.5)*12000/8192
        signal = np.sin(np.cumsum(2*np.pi*frequency/12000))
        start, length = 24000, 1440000
    else:
        text = run([OUT/'tones', mode], capture_output=True, text=True).stdout.strip()
        tones = np.array([int(c) for c in text])
        nsps, bt = (1920,2) if mode == 'FT8' else (576,1)
        x = np.arange(1,3*nsps+1)/nsps-1.5
        c = np.pi*np.sqrt(2/np.log(2))*bt
        pulse = .5*(erf(c*(x+.5))-erf(c*(x-.5)))
        frequency = np.zeros((len(tones)+2)*nsps)
        for n,tone in enumerate(tones): frequency[n*nsps:(n+3)*nsps] += tone*pulse
        if mode == 'FT8':
            frequency[:2*nsps] += tones[0]*pulse[nsps:]
            frequency[-2*nsps:] += tones[-1]*pulse[:2*nsps]
            frequency = frequency[nsps:(len(tones)+1)*nsps]
        signal = np.sin(np.cumsum(2*np.pi*(1500+frequency*12000/nsps)/12000))
        ramp = nsps//8 if mode == 'FT8' else nsps
        envelope = (1-np.cos(np.arange(ramp)*np.pi/ramp))/2
        signal[:ramp] *= envelope; signal[-ramp:] *= envelope[::-1]
        start = 6000 if mode == 'FT8' else 6000-nsps
        length = 180000 if mode == 'FT8' else 90000
    samples = np.zeros(length)
    samples[start:start+len(signal)] = 12000*signal
    path = OUT/f'{mode}_000000.wav'
    wav(path,samples)
    return path

def main():
    p = argparse.ArgumentParser(); p.add_argument('--source',type=pathlib.Path,required=True)
    args = p.parse_args(); OUT.mkdir(parents=True,exist_ok=True)
    build(args.source.resolve())
    run(['cargo','build','--release','-p','astra918-host','--example','audio_roundtrip'],cwd=ROOT)
    for mode in ['FT8','FT4','WSPR']:
        original = generate(mode)
        for sideband,offset in [('USB',45000),('LSB',-45000)]:
            output = OUT/f'{mode}_{sideband}_000000.wav'
            run([ROOT/'target/release/examples/audio_roundtrip',original,output,str(offset),sideband])
            command = ['wsprd','-f','14.0956',output] if mode=='WSPR' else ['jt9','-8' if mode=='FT8' else '-5',output]
            result = run(command,cwd=OUT,capture_output=True,text=True)
            (OUT/f'{mode}_{sideband}.txt').write_text(result.stdout+result.stderr)
            assert 'K1ABC' in result.stdout and 'FN42' in result.stdout, result.stdout+result.stderr
            print(f'{mode} {sideband} offset={offset}: decoded K1ABC FN42 through production DSP',flush=True)

if __name__ == '__main__': main()
