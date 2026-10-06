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
            Action::Configure | Action::Clock | Action::Capacitor | Action::Retry | Action::Channel
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
    s.radio.commit(
        settings,
        if action == Action::Clock {
            Action::Configure
        } else {
            action
        },
    );
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
                    return Ok(());
                }
                let (settings, action) = s.radio.prepare(cmd, p)?;
                apply(&mut s, settings, action).map_err(|_| a::Status::Io)
            });
            s.radio
                .response(&raw, outcome.err().unwrap_or(a::Status::Ok))
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
            frame.extend_from_slice(&i.to_le_bytes());
            frame.extend_from_slice(&q.to_le_bytes());
            if let Some(v) = v {
                pcm.extend_from_slice(&(if r.configured { v } else { 0 }).to_le_bytes());
            }
        }
        if r.streaming && r.configured && !stall && iq.try_send(frame).is_err() {
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
    let state = Arc::new(Mutex::new(Shared {
        radio,
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
