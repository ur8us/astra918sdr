//! Shared offline receiver: vendor 7350, I/Q 7351, CAT 7352, PCM 7353.
use anyhow::{Context, Result};
use astra918_firmware::{
    astra::{self as a, Action, Receiver},
    cat::{Parser, Reply},
    dsp::Demodulator,
    persistence as nv,
};
use clap::Parser as ArgsParser;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver as Queue, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};
#[derive(ArgsParser)]
struct Args {
    #[arg(long, default_value_t = 7350)]
    port: u16,
    #[arg(long, default_value = "artifacts/simulator")]
    settings: PathBuf,
    /// Create a Linux/Unix PTY for WSJT-X's TS-480 CAT connection.
    #[arg(long)]
    cat_pty: bool,
    /// Optional mono 12 kHz signed PCM16 WAV, looped as USB-sideband modulation.
    #[arg(long)]
    audio_wav: Option<PathBuf>,
    /// Align a test WAV's repeating period to UTC for live WSJT-X reception.
    #[arg(long)]
    utc_align: bool,
}
struct Shared {
    radio: Receiver,
    registers: [u8; 256],
    save_path: PathBuf,
    save_sequence: u32,
    fail_next: bool,
    stall_iq: bool,
}
type State = Arc<Mutex<Shared>>;
fn apply(s: &mut Shared, settings: a::Settings, action: Action) -> Result<()> {
    if s.fail_next
        && matches!(
            action,
            Action::Configure
                | Action::Clock
                | Action::Capacitor
                | Action::Retry
                | Action::Channel
                | Action::CalibrateNow
                | Action::FactoryReset
        )
    {
        s.fail_next = false;
        s.radio.fault(5);
        anyhow::bail!("Injected I2C failure");
    }
    if action == Action::Save {
        fs::create_dir_all(&s.save_path)?;
        let seq = s.save_sequence.wrapping_add(1);
        let slot = s.save_path.join(format!("slot{}.bin", seq & 1));
        let bytes = nv::record(settings, seq);
        let commit = nv::commit_page(&bytes);
        let mut f = fs::File::create(slot)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        f.write_all(&commit)?;
        f.sync_all()?;
        s.save_sequence = seq;
    }
    if action == Action::FactoryReset {
        for slot in 0..2 {
            let path = s.save_path.join(format!("slot{slot}.bin"));
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        s.save_sequence = 0;
        s.registers[0x07] &= !1;
    }
    if action == Action::Clock {
        s.registers[0x07] = (s.registers[0x07] & !1) | settings.reference as u8;
    }
    s.radio.commit(
        settings,
        if action == Action::Clock {
            Action::Configure
        } else {
            action
        },
    );
    if action == Action::CalibrateNow {
        // The offline simulator has no CMX918 pilot or coefficient registers.
        s.radio.iq_cal_status = 2;
    }
    Ok(())
}
fn control(mut socket: TcpStream, state: State, owner: Arc<AtomicBool>) {
    if owner.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = socket.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = socket.set_write_timeout(Some(Duration::from_secs(2)));
    loop {
        let mut raw = [0; 256];
        if socket.read_exact(&mut raw).is_err() {
            break;
        }
        let reply = {
            let mut s = state.lock().unwrap();
            let outcome = a::parse(&raw).and_then(|(cmd, p)| {
                // Simulator-only fault controls never exist in device firmware.
                if cmd == 0x70 && p.len() == 1 {
                    s.fail_next = p[0] & 1 != 0;
                    s.stall_iq = p[0] & 2 != 0;
                    let tx_active = p[0] & 4 != 0;
                    if s.radio.tx_active && !tx_active {
                        s.radio.generation = s.radio.generation.wrapping_add(1);
                    }
                    s.radio.tx_active = tx_active;
                    return Ok(None);
                }
                let (settings, action) = s.radio.prepare(cmd, p)?;
                match action {
                    Action::RegisterRead(address) => Ok(Some(s.registers[address as usize])),
                    Action::RegisterWrite(address, value) => {
                        s.registers[address as usize] = value;
                        Ok(None)
                    }
                    _ => apply(&mut s, settings, action)
                        .map(|_| None)
                        .map_err(|_| a::Status::Io),
                }
            });
            match outcome {
                Ok(Some(value)) => a::reply(&raw, a::Status::Ok, &[value]),
                Ok(None) => s.radio.response(&raw, a::Status::Ok),
                Err(status) => s.radio.response(&raw, status),
            }
        };
        if socket.write_all(&reply).is_err() {
            break;
        }
    }
    state.lock().unwrap().radio.streaming = false;
    owner.store(false, Ordering::Release);
}
fn cat<T: Read + Write>(mut io: T, state: State) {
    let mut parser = Parser::default();
    let mut bytes = [0; 64];
    loop {
        match io.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => {
                for b in &bytes[..n] {
                    if let Some(command) = parser.push(*b) {
                        let reply = match command {
                            Err(_) => Reply::literal(b"?;"),
                            Ok(command) => {
                                let mut s = state.lock().unwrap();
                                match s.radio.cat_prepare(command) {
                                    Err(_) => Reply::literal(b"?;"),
                                    Ok(proposed) => {
                                        if proposed.is_some_and(|(settings, action)| {
                                            apply(&mut s, settings, action).is_err()
                                        }) {
                                            Reply::literal(b"?;")
                                        } else {
                                            s.radio.cat_reply(command)
                                        }
                                    }
                                }
                            }
                        };
                        if io.write_all(&reply.bytes[..reply.len]).is_err() {
                            return;
                        }
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                parser.expire()
            }
            Err(_) => break,
        }
    }
}
fn serve_data(listener: TcpListener, queue: Queue<Vec<u8>>) {
    for socket in listener.incoming().flatten() {
        let mut socket = socket;
        let _ = socket.set_write_timeout(Some(Duration::from_millis(250)));
        while queue.try_recv().is_ok() {}
        while let Ok(bytes) = queue.recv() {
            if socket.write_all(&bytes).is_err() {
                break;
            }
        }
    }
}
fn wav(path: &PathBuf) -> Result<Vec<i16>> {
    let b = fs::read(path)?;
    anyhow::ensure!(
        b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WAVE",
        "Expected WAV"
    );
    let mut pos = 12;
    let mut valid = false;
    let mut samples = Vec::new();
    while pos + 8 <= b.len() {
        let n = u32::from_le_bytes(b[pos + 4..pos + 8].try_into()?) as usize;
        let end = pos + 8 + n;
        anyhow::ensure!(end <= b.len(), "Truncated WAV");
        if &b[pos..pos + 4] == b"fmt " {
            let f = &b[pos + 8..end];
            valid = f.len() >= 16
                && f[..4] == [1, 0, 1, 0]
                && f[4..8] == 12000u32.to_le_bytes()
                && f[14..16] == 16u16.to_le_bytes();
        }
        if &b[pos..pos + 4] == b"data" {
            samples = b[pos + 8..end]
                .chunks_exact(2)
                .map(|v| i16::from_le_bytes([v[0], v[1]]))
                .collect();
        }
        pos = end + (n & 1);
    }
    anyhow::ensure!(
        valid && !samples.is_empty(),
        "WAV must be PCM16 mono at 12000 Hz"
    );
    Ok(samples)
}
fn produce(
    state: State,
    iq: SyncSender<Vec<u8>>,
    audio: SyncSender<Vec<u8>>,
    wave: Option<Vec<i16>>,
    utc_align: bool,
) {
    let mut previous = state.lock().unwrap().radio;
    let mut dsp = Demodulator::new(previous.settings);
    let mut sample = 0u64;
    let mut seq = 0u32;
    let mut started = Instant::now();
    let utc_sample = || {
        if utc_align {
            (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64()
                * 12000.) as u64
        } else {
            0
        }
    };
    let mut wave_origin = utc_sample();
    loop {
        let (r, stall) = {
            let s = state.lock().unwrap();
            (s.radio, s.stall_iq)
        };
        if r.generation != previous.generation {
            dsp = Demodulator::new(r.settings);
            sample = 0;
            seq = 0;
            started = Instant::now();
            wave_origin = utc_sample();
            previous = r;
        }
        let mut frame = Vec::with_capacity(a::IQ_FRAME);
        frame.extend_from_slice(&r.iq_header(seq, sample));
        let mut pcm = Vec::with_capacity(104);
        for n in sample..sample + 512 {
            let beat = r.settings.dial as f64 - f64::from(r.settings.hardware().frequency);
            let angle = std::f64::consts::TAU * (beat + 1500.) * n as f64 / 120_000.;
            let (i, q) = if let Some(wave) = &wave {
                // A real waveform shifted to the carrier contains symmetric sidebands;
                // the firmware's complex SSB filter selects the requested one.
                let v = f64::from(wave[((wave_origin + n / 10) % wave.len() as u64) as usize]);
                let phase = std::f64::consts::TAU * beat * n as f64 / 120_000.;
                ((v * phase.cos()) as i16, (v * phase.sin()) as i16)
            } else {
                ((10000. * angle.cos()) as i16, (10000. * angle.sin()) as i16)
            };
            let ((i, q), v) = dsp.process(i, q);
            let receiving = r.configured && !r.tx_active;
            frame.extend_from_slice(&(if receiving { i } else { 0 }).to_le_bytes());
            frame.extend_from_slice(&(if receiving { q } else { 0 }).to_le_bytes());
            if let Some(v) = v {
                pcm.extend_from_slice(&(if receiving { v } else { 0 }).to_le_bytes());
            }
        }
        // Device firmware keeps the USB timeline alive with zeros while RF
        // hardware is unavailable, including TX/RX recovery.
        if r.streaming && !stall && iq.try_send(frame).is_err() {
            let mut s = state.lock().unwrap();
            s.radio.dropped = s.radio.dropped.saturating_add(1);
            s.radio.streaming = false;
        }
        let _ = audio.try_send(pcm);
        sample += 512;
        seq = seq.wrapping_add(1);
        let due = Duration::from_secs_f64(sample as f64 / 120_000.);
        if due > started.elapsed() {
            thread::sleep(due - started.elapsed());
        }
    }
}
#[cfg(unix)]
fn pty(state: State) -> Result<()> {
    use std::os::fd::FromRawFd;
    let (mut master, mut slave) = (-1, -1);
    let mut name = [0i8; 128];
    // SAFETY: openpty receives valid writable outputs and null optional attributes.
    anyhow::ensure!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                name.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0,
        "openpty failed"
    );
    // SAFETY: newly allocated descriptors are exclusively transferred to File.
    let master = unsafe { fs::File::from_raw_fd(master) };
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    // SAFETY: openpty writes a NUL-terminated name within the platform PTY path bound.
    println!(
        "CAT PTY: {}",
        unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_string_lossy()
    );
    thread::spawn(move || {
        let _keep_slave = slave;
        cat(master, state);
    });
    Ok(())
}
fn main() -> Result<()> {
    let args = Args::parse();
    anyhow::ensure!(args.port <= 65532, "Port range exceeds 65535");
    let mut radio = Receiver::default();
    let mut saved = None;
    for slot in 0..2 {
        if let Ok(bytes) = fs::read(args.settings.join(format!("slot{slot}.bin")))
            && bytes.len() == 512
            && let Some((settings, seq)) =
                nv::decode(bytes[..256].try_into()?, bytes[256..].try_into()?)
            && saved.is_none_or(|(_, old)| nv::newer(seq, old))
        {
            saved = Some((settings, seq));
        }
    }
    if let Some((settings, _)) = saved {
        radio.settings = settings;
        radio.saved_revision = 0;
    }
    radio.commit(radio.settings, Action::Configure);
    let mut registers = [0u8; 256];
    registers[0x07] = 0x30 | radio.settings.reference as u8;
    registers[0x97] = 0x61;
    let state = Arc::new(Mutex::new(Shared {
        radio,
        registers,
        save_path: args.settings,
        save_sequence: saved.map_or(0, |(_, s)| s),
        fail_next: false,
        stall_iq: false,
    }));
    let listen =
        |n| TcpListener::bind(("127.0.0.1", args.port + n)).context("Bind simulator endpoint");
    let control_listener = listen(0)?;
    let iq_listener = listen(1)?;
    let cat_listener = listen(2)?;
    let audio_listener = listen(3)?;
    let (iq_tx, iq_rx) = mpsc::sync_channel(8);
    let (audio_tx, audio_rx) = mpsc::sync_channel(64);
    thread::spawn(move || serve_data(iq_listener, iq_rx));
    thread::spawn(move || serve_data(audio_listener, audio_rx));
    let cat_state = state.clone();
    thread::spawn(move || {
        for socket in cat_listener.incoming().flatten() {
            let state = cat_state.clone();
            let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));
            thread::spawn(move || cat(socket, state));
        }
    });
    if args.cat_pty {
        #[cfg(unix)]
        pty(state.clone())?;
        #[cfg(not(unix))]
        anyhow::bail!("PTY adapter is Unix-only; use CAT TCP on this platform");
    }
    let wave = args.audio_wav.as_ref().map(wav).transpose()?;
    let producer_state = state.clone();
    thread::spawn(move || produce(producer_state, iq_tx, audio_tx, wave, args.utc_align));
    println!(
        "Astra918 simulator: control {}, I/Q {}, CAT {}, PCM {} (loopback only)",
        args.port,
        args.port + 1,
        args.port + 2,
        args.port + 3
    );
    let owner = Arc::new(AtomicBool::new(false));
    for socket in control_listener.incoming().flatten() {
        let state = state.clone();
        let owner = owner.clone();
        thread::spawn(move || control(socket, state, owner));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_shot_calibration_preserves_settings_and_reports_no_simulated_pilot() {
        let mut shared = Shared {
            radio: Receiver::default(),
            registers: [0; 256],
            save_path: PathBuf::new(),
            save_sequence: 2,
            fail_next: false,
            stall_iq: false,
        };
        let settings = shared.radio.settings;
        apply(&mut shared, settings, Action::Configure).unwrap();
        shared.radio.commit(settings, Action::Save);
        let revision = shared.radio.revision;
        let saved_revision = shared.radio.saved_revision;
        let generation = shared.radio.generation;
        let sequence = shared.save_sequence;
        let (settings, action) = shared.radio.prepare(a::IQ_CALIBRATE_NOW, &[]).unwrap();
        apply(&mut shared, settings, action).unwrap();
        assert_eq!(shared.radio.settings, settings);
        assert_eq!(shared.radio.revision, revision);
        assert_eq!(shared.radio.saved_revision, saved_revision);
        assert_eq!(shared.radio.generation, generation + 1);
        assert_eq!(shared.save_sequence, sequence);
        assert_eq!(shared.radio.iq_cal_status, 2);
    }

    #[test]
    fn factory_reset_deletes_both_saved_slots_and_applies_defaults() {
        let path = std::env::temp_dir().join(format!(
            "astra918-factory-reset-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut shared = Shared {
            radio: Receiver::default(),
            registers: [0; 256],
            save_path: path.clone(),
            save_sequence: 0,
            fail_next: false,
            stall_iq: false,
        };
        let mut changed = shared.radio.settings;
        changed.gpio = 0x5a;
        shared.radio.commit(changed, Action::Logical);
        apply(&mut shared, changed, Action::Save).unwrap();
        apply(&mut shared, changed, Action::Save).unwrap();
        assert!(path.join("slot0.bin").exists());
        assert!(path.join("slot1.bin").exists());
        let (defaults, action) = shared.radio.prepare(a::FACTORY_RESET, &[]).unwrap();
        apply(&mut shared, defaults, action).unwrap();
        assert_eq!(shared.radio.settings, a::Settings::default());
        assert_eq!(shared.radio.saved_revision, u32::MAX);
        assert_eq!(shared.save_sequence, 0);
        assert!(!path.join("slot0.bin").exists());
        assert!(!path.join("slot1.bin").exists());
        fs::remove_dir(path).unwrap();
    }
}
