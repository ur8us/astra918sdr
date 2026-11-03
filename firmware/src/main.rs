#![no_std]
#![no_main]
#[cfg(not(any(feature = "rp235xa", feature = "rp235xb")))]
compile_error!("Select rp235xa or rp235xb");
#[cfg(all(feature = "rp235xa", feature = "rp235xb"))]
compile_error!("Select only one RP2350 variant");
mod capture;
mod i2c_bus;
mod tx_counter;
mod tx_output;
use tx_output::{tx_divider_disable, tx_divider_enable};
mod usb_audio;
mod usb_vendor;
use astra918_firmware::{
    Error,
    astra::{self as a, Action, Receiver, Settings},
    audio::{AudioQueue, MAX_PACKET_BYTES},
    cat::{Command, Mode, Parser, Reply},
    chip::Chip,
    dsp::Demodulator,
    persistence as nv,
    tx_audio::Vox,
    tx_monitor::{self, AudioBuffer as MonitorAudio, Monitor},
    tx_pll,
};
use core::{
    cell::RefCell,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
use defmt_rtt as _;
use embassy_executor::{Executor, Spawner};
use embassy_futures::select::{Either, select};
use embassy_rp::{
    bind_interrupts,
    flash::{Blocking, Flash},
    gpio::{Level, Output},
    i2c,
    multicore::{Stack, spawn_core1},
    peripherals::{FLASH, I2C0, USB},
    usb,
};
use embassy_sync::{
    blocking_mutex::{Mutex, raw::CriticalSectionRawMutex},
    channel::Channel,
    signal::Signal,
};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embassy_usb::{
    Builder, Config, Handler,
    class::cdc_acm::{CdcAcmClass, State},
    control::{OutResponse, Recipient, Request, RequestType},
    driver::{Endpoint, EndpointIn, EndpointOut},
};
use static_cell::StaticCell;
defmt::timestamp!("{=u64:us}", Instant::now().as_micros());
bind_interrupts!(pub struct Irqs {
    USBCTRL_IRQ => usb::InterruptHandler<USB>;
    I2C0_IRQ => i2c::InterruptHandler<I2C0>;
    PIO0_IRQ_0 => embassy_rp::pio::InterruptHandler<embassy_rp::peripherals::PIO0>;
});
type UsbDriver = usb::Driver<'static, USB>;
type In = usb::Endpoint<'static, USB, usb::In>;
type Out = usb::Endpoint<'static, USB, usb::Out>;
type Storage = Flash<'static, FLASH, Blocking, { nv::FLASH_SIZE }>;

async fn key_tx_relay(ptt: &mut Output<'_>, led: &mut Output<'_>, ms: u16) {
    astra918_firmware::tx_relay::key(&mut embassy_time::Delay, ms, |on| {
        ptt.set_level(if on { Level::High } else { Level::Low });
        led.set_level(if on { Level::High } else { Level::Low });
    })
    .await;
}

async fn unkey_tx_relay(ptt: &mut Output<'_>, led: &mut Output<'_>, ms: u16) {
    TX_MONITOR_AUDIO_KEYED.store(false, Ordering::Release);
    let keyed = ptt.is_set_high();
    astra918_firmware::tx_relay::unkey(
        &mut embassy_time::Delay,
        ms,
        keyed,
        || unsafe { tx_divider_disable() },
        |on| {
            ptt.set_level(if on { Level::High } else { Level::Low });
            led.set_level(if on { Level::High } else { Level::Low });
        },
    )
    .await;
}
static REQUESTS: Channel<CriticalSectionRawMutex, [u8; 256], 1> = Channel::new();
static REPLIES: Channel<CriticalSectionRawMutex, [u8; 256], 1> = Channel::new();
static CAT_REQUESTS: Channel<CriticalSectionRawMutex, Command, 1> = Channel::new();
static CAT_REPLIES: Channel<CriticalSectionRawMutex, Reply, 1> = Channel::new();
static CMX_RESET_REQUEST: AtomicU32 = AtomicU32::new(0);
static CMX_RESET_COUNT: AtomicU32 = AtomicU32::new(0);

#[embassy_executor::task]
async fn cmx_reset_task(mut resetn: Output<'static>) {
    loop {
        if CMX_RESET_REQUEST.swap(0, Ordering::AcqRel) != 0 {
            resetn.set_low();
            Timer::after_millis(20).await;
            resetn.set_high();
            CMX_RESET_COUNT.fetch_add(1, Ordering::Release);
            defmt::info!("CMX918 RESETN pulsed via GP2");
        }
        Timer::after_millis(10).await;
    }
}
enum Engine {
    Pause,
    Run(Receiver),
    Update(Receiver),
}
static ENGINE: Channel<CriticalSectionRawMutex, (u32, Engine), 1> = Channel::new();
static ENGINE_ACK: Channel<CriticalSectionRawMutex, (u32, Result<(), Error>), 1> = Channel::new();
static ENGINE_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static BLOCKS: Channel<CriticalSectionRawMutex, ([u8; a::IQ_FRAME], u32), 8> = Channel::new();
static AUDIO: Mutex<CriticalSectionRawMutex, RefCell<AudioQueue>> =
    Mutex::new(RefCell::new(AudioQueue::new()));
static AUDIO_CHANGED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static ACTIVE: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
static STREAMING: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU32 = AtomicU32::new(0);
static USB_EPOCH: AtomicU32 = AtomicU32::new(0);
static BOOTSEL_REQUEST: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static TX_AUDIO: Signal<CriticalSectionRawMutex, Option<astra918_firmware::tx_audio::Measurement>> =
    Signal::new();
static USB_STALLS: AtomicU32 = AtomicU32::new(0);
static IQ_FAULTS: AtomicU32 = AtomicU32::new(0);
static IQ_DROPS: AtomicU32 = AtomicU32::new(0);
static CAPTURE_ERROR: AtomicU32 = AtomicU32::new(0);
static TX_ACTIVE: AtomicBool = AtomicBool::new(false);
static TX_MONITOR_AUDIO_KEYED: AtomicBool = AtomicBool::new(false);
struct IqState {
    header: [u8; a::IQ_HEADER],
    monitor: tx_monitor::Config,
}
static IQ_STATE_HEADER: Mutex<CriticalSectionRawMutex, RefCell<IqState>> =
    Mutex::new(RefCell::new(IqState {
        header: [0; a::IQ_HEADER],
        monitor: tx_monitor::Config {
            enabled: false,
            offset: 0,
            mode: Mode::Usb,
        },
    }));
static TX_MONITOR_AUDIO: Mutex<CriticalSectionRawMutex, RefCell<MonitorAudio>> =
    Mutex::new(RefCell::new(MonitorAudio::new()));

fn publish_iq_state(radio: &Receiver) {
    IQ_STATE_HEADER.lock(|header| {
        let mut state = header.borrow_mut();
        state.header.copy_from_slice(&radio.iq_header(0, 0));
        state.monitor = tx_monitor::Config {
            enabled: radio.settings.tx_iq_monitor,
            offset: radio.settings.offset,
            mode: radio.settings.mode,
        };
        GENERATION.store(radio.generation, Ordering::Release);
    });
}
fn increment(v: &AtomicU32) {
    let _ = v.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        Some(n.saturating_add(1))
    });
}
fn clear_audio() {
    AUDIO.lock(|q| q.borrow_mut().clear());
    AUDIO_CHANGED.signal(());
}
fn audio_active(active: bool) {
    ACTIVE.store(active, Ordering::Release);
    clear_audio();
}

async fn physical_cmx_reset() {
    let before = CMX_RESET_COUNT.load(Ordering::Acquire);
    CMX_RESET_REQUEST.store(1, Ordering::Release);
    for _ in 0..50 {
        if CMX_RESET_COUNT.load(Ordering::Acquire) != before {
            Timer::after_millis(10).await;
            return;
        }
        Timer::after_millis(2).await;
    }
}

async fn tx_configure(
    chip: &mut Chip<i2c_bus::Bus, embassy_time::Delay>,
    settings: Settings,
    rf_hz: u64,
) -> Result<tx_pll::Plan, Error> {
    let plan = tx_pll::plan_millihz(rf_hz)?;
    chip.write(0x03, 1).await?;
    chip.write(0x07, 0x30 | settings.reference as u8).await?;
    chip.write(0x97, settings.hardware().xtal_control()).await?;
    Timer::after_millis(10).await;
    let fc = plan.carrier;
    chip.write(0x08, (fc >> 16) as u8).await?;
    chip.write(0x09, (fc >> 8) as u8).await?;
    chip.write(0x0a, fc as u8).await?;
    for (reg, value) in [
        (0x2a, 1),
        (0x2b, (plan.n >> 8) as u8),
        (0x2c, plan.n as u8),
        (0x2d, (plan.fraction >> 16) as u8),
        (0x2e, (plan.fraction >> 8) as u8),
        (0x2f, plan.fraction as u8),
        (0x30, 0),
        (0x31, 0),
        (0x32, plan.l),
    ] {
        chip.write(reg, value).await?;
    }
    chip.write(0x03, 3).await?;
    chip.write(0x29, 6).await?;
    let mut locked = false;
    for _ in 0..40 {
        // STATUS may briefly report the previous lock while SAS/filter
        // calibration is still running. Wait for AUTO_EXE completion first.
        if chip.read(0x29).await? & 7 == 0 && chip.read(0x06).await? & 0x28 == 0x28 {
            locked = true;
            break;
        }
        Timer::after_millis(5).await;
    }
    if !locked {
        return Err(Error::Timeout);
    }
    chip.write(0xb9, 0).await?;
    chip.write(0x5d, 2).await?;
    Ok(plan)
}

fn tx_rf(settings: Settings, audio_hz: f32) -> Result<u64, Error> {
    let tone = libm::roundf(audio_hz * 1000.0) as i64;
    let dial = settings.dial as i64 * 1000;
    let rf = match settings.mode {
        astra918_firmware::cat::Mode::Usb => dial + tone,
        astra918_firmware::cat::Mode::Lsb => dial - tone,
    };
    u64::try_from(rf).map_err(|_| Error::Frequency)
}
struct Events;
impl Handler for Events {
    fn control_out(&mut self, req: Request, data: &[u8]) -> Option<OutResponse> {
        // CDC communication interface 2 follows UAC interfaces 0 and 1.
        // Observe SET_LINE_CODING before the CDC handler consumes it, then let
        // that handler acknowledge the request before rebooting.
        if req.request_type == RequestType::Class
            && req.recipient == Recipient::Interface
            && req.index == 2
            && req.request == 0x20
            && req.length == 7
            && data.len() == 7
            && data[..4] == 1200u32.to_le_bytes()
        {
            BOOTSEL_REQUEST.signal(());
        }
        None
    }
    fn reset(&mut self) {
        STREAMING.store(false, Ordering::Release);
    }
    fn configured(&mut self, on: bool) {
        if !on {
            STREAMING.store(false, Ordering::Release);
        }
    }
    fn suspended(&mut self, on: bool) {
        if on {
            STREAMING.store(false, Ordering::Release);
        }
    }
}
#[embassy_executor::task]
async fn usb_task(mut device: embassy_usb::UsbDevice<'static, UsbDriver>) {
    device.run().await;
}
#[embassy_executor::task]
async fn bootsel_task() {
    BOOTSEL_REQUEST.wait().await;
    // The CDC handler still needs to complete the status stage of the control
    // transfer. The ROM reset does not return on successful BOOTSEL entry.
    Timer::after_millis(100).await;
    embassy_rp::rom_data::reset_to_usb_boot(0, 0);
}
#[embassy_executor::task]
async fn control_task(mut input: Out, mut output: In) {
    loop {
        input.wait_enabled().await;
        let mut raw = [0; 256];
        let mut used = 0;
        loop {
            let mut packet = [0; 64];
            match with_timeout(Duration::from_secs(1), input.read(&mut packet)).await {
                Ok(Ok(n)) => {
                    for byte in &packet[..n] {
                        raw[used] = *byte;
                        used += 1;
                        if used == 256 {
                            let epoch = USB_EPOCH.load(Ordering::Acquire);
                            REQUESTS.send(raw).await;
                            let reply = REPLIES.receive().await;
                            if epoch == USB_EPOCH.load(Ordering::Acquire) {
                                let transfer = async {
                                    for b in reply.chunks(64) {
                                        output.write(b).await?;
                                    }
                                    Ok::<(), embassy_usb::driver::EndpointError>(())
                                };
                                if !matches!(
                                    with_timeout(Duration::from_millis(250), transfer).await,
                                    Ok(Ok(()))
                                ) {
                                    increment(&IQ_FAULTS);
                                }
                            }
                            used = 0;
                        }
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => used = 0,
            }
        }
    }
}
#[embassy_executor::task]
async fn cat_task(mut cdc: CdcAcmClass<'static, UsbDriver>) {
    loop {
        cdc.wait_connection().await;
        let mut parser = Parser::default();
        let mut epoch = USB_EPOCH.load(Ordering::Acquire);
        loop {
            let mut packet = [0; 64];
            let n = match with_timeout(Duration::from_secs(1), cdc.read_packet(&mut packet)).await {
                Ok(Ok(n)) => n,
                Ok(Err(_)) => break,
                Err(_) => {
                    parser.expire();
                    continue;
                }
            };
            let current = USB_EPOCH.load(Ordering::Acquire);
            if current != epoch {
                epoch = current;
                parser = Parser::default();
            }
            let mut failed = false;
            for b in &packet[..n] {
                if let Some(command) = parser.push(*b) {
                    let reply = match command {
                        Err(_) => Reply::literal(b"?;"),
                        Ok(command) => {
                            CAT_REQUESTS.send(command).await;
                            CAT_REPLIES.receive().await
                        }
                    };
                    if epoch != USB_EPOCH.load(Ordering::Acquire) {
                        failed = true;
                        break;
                    }
                    if reply.len > 0
                        && !matches!(
                            with_timeout(
                                Duration::from_millis(100),
                                cdc.write_packet(&reply.bytes[..reply.len])
                            )
                            .await,
                            Ok(Ok(()))
                        )
                    {
                        failed = true;
                        break;
                    }
                }
            }
            if failed {
                break;
            }
        }
        Timer::after_millis(1).await;
    }
}
#[embassy_executor::task]
async fn audio_task(mut output: In) {
    loop {
        output.wait_enabled().await;
        if !ACTIVE.load(Ordering::Acquire) {
            Timer::after_millis(1).await;
            continue;
        }
        let mut packet = [0; MAX_PACKET_BYTES];
        let n = if READY.load(Ordering::Acquire) {
            AUDIO.lock(|q| q.borrow_mut().packet(&mut packet))
        } else {
            24
        };
        match select(
            AUDIO_CHANGED.wait(),
            with_timeout(Duration::from_millis(4), output.write(&packet[..n])),
        )
        .await
        {
            Either::First(()) | Either::Second(Ok(Ok(()))) => {}
            _ => {
                increment(&USB_STALLS);
                clear_audio();
                Timer::after_millis(1).await;
            }
        }
    }
}

#[embassy_executor::task]
async fn playback_task(mut input: Out) {
    loop {
        input.wait_enabled().await;
        let mut estimator = astra918_firmware::tx_audio::ToneEstimator::default();
        loop {
            let mut packet = [0u8; 26];
            match with_timeout(Duration::from_millis(10), input.read(&mut packet)).await {
                Ok(Ok(n)) if n > 0 && n % 2 == 0 => {
                    let mut samples = [0i16; 13];
                    for (sample, bytes) in samples.iter_mut().zip(packet[..n].chunks_exact(2)) {
                        *sample = i16::from_le_bytes([bytes[0], bytes[1]]);
                    }
                    estimator.push(&samples[..n / 2]);
                    let measurement = estimator.measure();
                    TX_AUDIO.signal(measurement);
                    let config = IQ_STATE_HEADER.lock(|state| state.borrow().monitor);
                    let in_span = measurement.and_then(|m| m.frequency_hz).is_none_or(|tone| {
                        let offset = config.offset as f32
                            + if config.mode == Mode::Usb {
                                tone
                            } else {
                                -tone
                            };
                        offset.abs() < 59_500.
                    });
                    TX_MONITOR_AUDIO.lock(|queue| {
                        let mut queue = queue.borrow_mut();
                        if config.enabled
                            && in_span
                            && STREAMING.load(Ordering::Acquire)
                            && TX_ACTIVE.load(Ordering::Acquire)
                            && TX_MONITOR_AUDIO_KEYED.load(Ordering::Acquire)
                            && tx_output::rf_enabled()
                        {
                            queue.push(&samples[..n / 2]);
                        } else {
                            queue.clear();
                        }
                    });
                }
                Ok(Err(_)) => {
                    TX_AUDIO.signal(None);
                    TX_MONITOR_AUDIO.lock(|queue| queue.borrow_mut().clear());
                    break;
                }
                _ => {
                    TX_AUDIO.signal(None);
                    TX_MONITOR_AUDIO.lock(|queue| queue.borrow_mut().clear());
                    Timer::after_millis(1).await;
                }
            }
        }
    }
}
#[embassy_executor::task]
async fn iq_task(mut output: In) {
    let mut timeline = astra918_firmware::iq_stream::Timeline::default();
    let mut monitor = Monitor::default();
    let period = Duration::from_micros(4_267);
    let mut next_fill = Instant::now() + period;
    loop {
        let item = select(BLOCKS.receive(), Timer::at(next_fill)).await;
        if matches!(&item, Either::Second(())) {
            next_fill += period;
            if next_fill <= Instant::now() {
                next_fill = Instant::now() + period;
            }
        }
        let (mut frame, generation) = match item {
            Either::First(frame) => frame,
            Either::Second(())
                if (TX_ACTIVE.load(Ordering::Acquire) || !READY.load(Ordering::Acquire))
                    && STREAMING.load(Ordering::Acquire) =>
            {
                let frame = IQ_STATE_HEADER.lock(|header| {
                    astra918_firmware::iq_stream::zero_frame(&header.borrow().header)
                });
                let generation = u32::from_le_bytes(frame[8..12].try_into().unwrap());
                (frame, generation)
            }
            Either::Second(()) => continue,
        };
        if !STREAMING.load(Ordering::Acquire) || generation != GENERATION.load(Ordering::Acquire) {
            continue;
        }
        if TX_ACTIVE.load(Ordering::Acquire) || !READY.load(Ordering::Acquire) {
            let config = IQ_STATE_HEADER.lock(|state| state.borrow().monitor);
            let active = config.enabled
                && TX_ACTIVE.load(Ordering::Acquire)
                && TX_MONITOR_AUDIO_KEYED.load(Ordering::Acquire)
                && tx_output::rf_enabled();
            let mut audio = [0i16; tx_monitor::MAX_INPUT];
            let count = TX_MONITOR_AUDIO.lock(|queue| {
                let mut queue = queue.borrow_mut();
                if !active {
                    queue.clear();
                }
                let (available, discontinuity) = queue.available();
                let needed = monitor.prepare(a::IQ_SAMPLES, active, available, discontinuity);
                queue.take(&mut audio[..needed])
            });
            let mut used = 0;
            for chunk in frame[a::IQ_HEADER..].chunks_exact_mut(64 * 4) {
                let end = (used + monitor.input_count(64)).min(count);
                monitor.render(chunk, &audio[used..end], config);
                used = end;
                // Playback and the millisecond PLL controller share this core.
                embassy_futures::yield_now().await;
            }
        }
        // Capture and zero-fill are producers of the same USB stream. They
        // must never reset its counters independently within one generation.
        timeline.stamp(&mut frame);
        let transfer = async {
            for packet in frame.chunks(64) {
                output.write(packet).await?;
            }
            Ok::<(), embassy_usb::driver::EndpointError>(())
        };
        if !matches!(
            with_timeout(Duration::from_millis(100), transfer).await,
            Ok(Ok(()))
        ) {
            increment(&IQ_FAULTS);
            STREAMING.store(false, Ordering::Release);
        }
    }
}
#[embassy_executor::task]
async fn engine_task(mut capture: capture::Capture) {
    let mut radio = Receiver::default();
    let mut dsp = Demodulator::new(radio.settings);
    let mut running = false;
    let mut settling = 12_000usize;
    let mut timing_blocks = 0u32;
    let mut timing_total = 0u64;
    let mut timing_max = 0u64;
    loop {
        match select(ENGINE.receive(), Timer::after_micros(250)).await {
            Either::First((ticket, command)) => {
                let result = match command {
                    Engine::Pause => {
                        running = false;
                        READY.store(false, Ordering::Release);
                        clear_audio();
                        capture.stop()
                    }
                    Engine::Run(r) => {
                        radio = r;
                        dsp = Demodulator::new(r.settings);
                        settling = 12_000;
                        let result = capture.start();
                        running = result.is_ok();
                        READY.store(running, Ordering::Release);
                        result
                    }
                    Engine::Update(r) => {
                        radio = r;
                        Ok(())
                    }
                };
                ENGINE_ACK.send((ticket, result)).await;
            }
            Either::Second(()) => {}
        }
        if !running {
            continue;
        }
        for _ in 0..2 {
            let mut data = [0; 2048];
            let got = capture.read_block(&mut data, 2);
            match got {
                Ok(None) => break,
                Err(e) => {
                    running = false;
                    let _ = capture.stop();
                    READY.store(false, Ordering::Release);
                    clear_audio();
                    CAPTURE_ERROR.store(e as u32, Ordering::Release);
                    break;
                }
                _ => {}
            }
            let mut frame = [0; a::IQ_FRAME];
            let processing_start = Instant::now();
            frame[..a::IQ_HEADER].copy_from_slice(&radio.iq_header(0, 0));
            let mut pcm = [0i16; 52];
            let mut count = 0;
            for (index, iq) in data.chunks_exact(4).enumerate() {
                let i = i16::from_le_bytes([iq[0], iq[1]]);
                let q = i16::from_le_bytes([iq[2], iq[3]]);
                let ((i, q), audio) = dsp.process(i, q);
                let p = a::IQ_HEADER + index * 4;
                frame[p..p + 2].copy_from_slice(&i.to_le_bytes());
                frame[p + 2..p + 4].copy_from_slice(&q.to_le_bytes());
                settling = settling.saturating_sub(1);
                if let Some(v) = audio {
                    pcm[count] = if settling == 0 { v } else { 0 };
                    count += 1;
                }
            }
            if ACTIVE.load(Ordering::Acquire) {
                AUDIO.lock(|q| q.borrow_mut().push(&pcm[..count]));
            }
            if STREAMING.load(Ordering::Acquire)
                && BLOCKS.try_send((frame, radio.generation)).is_err()
            {
                increment(&IQ_DROPS);
                STREAMING.store(false, Ordering::Release);
            }
            let elapsed = processing_start.elapsed().as_micros();
            timing_total += elapsed;
            timing_max = timing_max.max(elapsed);
            timing_blocks += 1;
            if timing_blocks == 1024 {
                defmt::info!(
                    "DSP block us: mean={} max={} budget=4267",
                    timing_total / 1024,
                    timing_max
                );
                timing_blocks = 0;
                timing_total = 0;
                timing_max = 0;
            }
        }
    }
}
async fn engine(command: Engine) -> Result<(), Error> {
    let sequence = ENGINE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    with_timeout(Duration::from_millis(500), async {
        // A late acknowledgement from a timed-out operation cannot confirm a
        // different operation. Drain it before queuing the next request.
        while ENGINE_ACK.try_receive().is_ok() {}
        ENGINE.send((sequence, command)).await;
        loop {
            let (ack, result) = ENGINE_ACK.receive().await;
            if ack == sequence {
                return result;
            }
        }
    })
    .await
    .map_err(|_| Error::Timeout)?
}
async fn apply(
    r: &mut Receiver,
    s: Settings,
    action: Action,
    chip: &mut Chip<i2c_bus::Bus, embassy_time::Delay>,
    flash: &mut Storage,
    save_seq: &mut u32,
) -> Result<Option<u8>, Error> {
    if s.tx_differential != r.settings.tx_differential {
        tx_output::set_polarity(s.tx_differential)?;
    }
    if s.tx_drive_ma != r.settings.tx_drive_ma
        || s.tx_slew_fast != r.settings.tx_slew_fast
        || (!TX_ACTIVE.load(Ordering::Acquire)
            && matches!(
                action,
                Action::Configure | Action::Clock | Action::Retry | Action::FactoryReset
            ))
    {
        tx_output::set_output_config(s.tx_drive_ma, s.tx_slew_fast)?;
    }
    if TX_ACTIVE.load(Ordering::Acquire) {
        match action {
            Action::Configure
            | Action::Clock
            | Action::Channel
            | Action::Capacitor
            | Action::Logical => {
                // Keep the capture engine paused; the TX loop applies the new
                // CAT dial/sideband to the PLL on its next millisecond tick.
                r.commit(s, action);
                publish_iq_state(r);
                return Ok(None);
            }
            Action::Query => return Ok(None),
            Action::Start | Action::Stop => {
                r.commit(s, action);
                publish_iq_state(r);
                STREAMING.store(r.streaming, Ordering::Release);
                return Ok(None);
            }
            _ => return Err(Error::Protocol),
        }
    }
    let mut register_value = None;
    match action {
        Action::RegisterRead(address) => {
            register_value = Some(
                with_timeout(Duration::from_millis(100), chip.read(address))
                    .await
                    .map_err(|_| Error::Timeout)??,
            );
        }
        Action::RegisterWrite(address, value) => {
            with_timeout(Duration::from_millis(100), chip.write(address, value))
                .await
                .map_err(|_| Error::Timeout)??;
        }
        Action::Configure
        | Action::Clock
        | Action::Retry
        | Action::IqSettings
        | Action::CalibrateNow
        | Action::FactoryReset => {
            r.configured = false;
            engine(Engine::Pause).await?;
            while BLOCKS.try_receive().is_ok() {}
            if action == Action::FactoryReset {
                // Erase *both* alternating slots. Erasing just the newest one
                // would resurrect an older saved configuration on next boot.
                for slot in 0..2 {
                    let start = nv::BASE + slot * nv::SECTOR as u32;
                    flash
                        .blocking_erase(start, start + nv::SECTOR as u32)
                        .map_err(|_| Error::Readback)?;
                }
                let mut page = [0; nv::PAGE];
                for offset in (0..2 * nv::SECTOR).step_by(nv::PAGE) {
                    flash
                        .blocking_read(nv::BASE + offset as u32, &mut page)
                        .map_err(|_| Error::Readback)?;
                    if page.iter().any(|&byte| byte != 0xff) {
                        return Err(Error::Readback);
                    }
                }
                *save_seq = 0;
                if r.settings != s {
                    r.settings = s;
                    r.revision = r.revision.wrapping_add(1);
                }
                r.saved_revision = u32::MAX;
                r.iq_cal_status = 0;
            }
            if action == Action::Clock && s != r.settings {
                r.settings = s;
                r.revision = r.revision.wrapping_add(1);
            }
            let reconfigured = async {
                if action == Action::CalibrateNow {
                    with_timeout(
                        Duration::from_secs(8),
                        chip.calibrate_iq_now(
                            s.hardware(),
                            s.controls,
                            s.reference,
                            s.vfo_sign.lo_below(s.hardware().frequency),
                            s.if_frequency.hz(),
                            s.running_iq,
                        ),
                    )
                    .await
                    .map_err(|_| Error::Timeout)??;
                } else {
                    with_timeout(
                        Duration::from_secs(8),
                        chip.configure_recovering_iq(
                            s.hardware(),
                            s.controls,
                            s.reference,
                            s.vfo_sign.lo_below(s.hardware().frequency),
                            s.if_frequency.hz(),
                            s.running_iq,
                            s.iq_on_large_retune,
                            action == Action::Retry
                                || (!r.settings.iq_on_large_retune && s.iq_on_large_retune),
                        ),
                    )
                    .await
                    .map_err(|_| Error::Timeout)??;
                }
                with_timeout(Duration::from_millis(100), chip.mute(false))
                    .await
                    .map_err(|_| Error::Timeout)??;
                Ok::<(), Error>(())
            }
            .await;
            if let Err(error) = reconfigured {
                if action == Action::FactoryReset {
                    // The erase has already succeeded. Report that operation
                    // as complete, while exposing the RF fault in STATUS.
                    r.fault(error as u32);
                    r.streaming = false;
                    STREAMING.store(false, Ordering::Release);
                    return Ok(None);
                }
                return Err(error);
            }
            let mut next = *r;
            next.commit(s, action);
            next.iq_cal_status = chip.iq_cal_status();
            publish_iq_state(&next);
            engine(Engine::Run(next)).await?;
            *r = next;
        }
        Action::Capacitor => {
            with_timeout(
                Duration::from_millis(100),
                chip.set_lf_mf_capacitor_live_recovering(s.controls.lf_mf_capacitor),
            )
            .await
            .map_err(|_| Error::Timeout)??;
            r.commit(s, action);
            publish_iq_state(r);
            engine(Engine::Update(*r)).await?;
        }
        Action::Logical => {
            r.commit(s, action);
            publish_iq_state(r);
            engine(Engine::Update(*r)).await?;
        }
        Action::Channel => {
            // The RF center is unchanged. Restart only the capture/DSP epoch;
            // do not write the CMX918 PLL or recalibrate the RF path.
            engine(Engine::Pause).await?;
            while BLOCKS.try_receive().is_ok() {}
            let mut next = *r;
            next.commit(s, action);
            publish_iq_state(&next);
            engine(Engine::Run(next)).await?;
            *r = next;
        }
        Action::Start | Action::Stop => {
            if action == Action::Start && !r.configured {
                return Err(Error::NotConfigured);
            }
            r.commit(s, action);
            publish_iq_state(r);
            engine(Engine::Update(*r)).await?;
            STREAMING.store(r.streaming, Ordering::Release);
        }
        Action::Save => {
            engine(Engine::Pause).await?;
            let next = save_seq.wrapping_add(1);
            let offset = nv::BASE + (next & 1) * nv::SECTOR as u32;
            let record = nv::record(s, next);
            let commit = nv::commit_page(&record);
            let result = (|| {
                flash.blocking_erase(offset, offset + nv::SECTOR as u32)?;
                flash.blocking_write(offset, &record)?;
                flash.blocking_write(offset + 256, &commit)?;
                let mut b = [0; 256];
                let mut c = [0; 256];
                flash.blocking_read(offset, &mut b)?;
                flash.blocking_read(offset + 256, &mut c)?;
                if nv::decode(&b, &c) != Some((s, next)) {
                    return Err(embassy_rp::flash::Error::Other);
                }
                Ok(())
            })();
            if result.is_ok() {
                *save_seq = next;
                r.commit(s, action);
            }
            r.generation = r.generation.wrapping_add(1);
            publish_iq_state(r);
            if r.configured {
                engine(Engine::Run(*r)).await?;
            }
            result.map_err(|_| Error::Readback)?;
        }
        Action::Query => {}
    }
    Ok(register_value)
}
#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    // Drive CMX918 RESETN actively low during reset and high afterward.
    let cmx_resetn = Output::new(p.PIN_2, Level::High);
    let mut ptt = Output::new(p.PIN_6, Level::Low);
    let mut tx_led = Output::new(p.PIN_25, Level::Low);
    // GP21 must stay low throughout RX, including immediately after boot.
    unsafe { tx_divider_disable() };
    defmt::info!("Astra918 0.1.0; composite USB; development firmware");
    let mut flash = Storage::new_blocking(p.FLASH);
    let mut saved = None;
    for slot in 0..2 {
        let mut b = [0; 256];
        let mut c = [0; 256];
        let offset = nv::BASE + slot * 4096;
        if flash.blocking_read(offset, &mut b).is_ok()
            && flash.blocking_read(offset + 256, &mut c).is_ok()
            && let Some(value) = nv::decode(&b, &c)
            && saved.is_none_or(|(_, old)| nv::newer(value.1, old))
        {
            saved = Some(value);
        }
    }
    let mut radio = Receiver::default();
    let mut save_seq = 0;
    if let Some((s, seq)) = saved {
        radio.settings = s;
        radio.saved_revision = 0;
        save_seq = seq;
    }
    let mut info = [0u32; 4];
    // SAFETY: correctly sized writable ROM information buffer, requesting chip identity only.
    let count = unsafe { embassy_rp::rom_data::get_sys_info(info.as_mut_ptr(), info.len(), 1) };
    let mut uid = [0u8; 8];
    if count == 4 {
        uid[..4].copy_from_slice(&info[3].to_be_bytes());
        uid[4..].copy_from_slice(&info[2].to_be_bytes());
    }
    static SERIAL: StaticCell<[u8; 16]> = StaticCell::new();
    let serial = SERIAL.init([b'0'; 16]);
    for (i, b) in uid.iter().enumerate() {
        serial[i * 2] = b"0123456789ABCDEF"[(b >> 4) as usize];
        serial[i * 2 + 1] = b"0123456789ABCDEF"[(b & 15) as usize];
    }
    let driver = usb::Driver::new(p.USB, Irqs);
    let mut config = Config::new(a::VID, a::PID);
    config.device_class = 0xef;
    config.device_sub_class = 2;
    config.device_protocol = 1;
    config.bcd_usb = embassy_usb::UsbVersion::TwoOne;
    config.manufacturer = Some("Astra918 project");
    config.product = Some("Astra918 Audio CAT SDR");
    config.serial_number = Some(core::str::from_utf8(serial).unwrap());
    config.max_power = 100;
    static CONFIG: StaticCell<[u8; 512]> = StaticCell::new();
    static BOS: StaticCell<[u8; 256]> = StaticCell::new();
    static MSOS: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL: StaticCell<[u8; 64]> = StaticCell::new();
    static AUDIO_CONTROL: StaticCell<usb_audio::Control> = StaticCell::new();
    static CDC: StaticCell<State> = StaticCell::new();
    static EVENTS: StaticCell<Events> = StaticCell::new();
    let mut builder = Builder::new(
        driver,
        config,
        CONFIG.init([0; 512]),
        BOS.init([0; 256]),
        MSOS.init([0; 256]),
        CONTROL.init([0; 64]),
    );
    builder.handler(EVENTS.init(Events));
    let audio = usb_audio::microphone(&mut builder, AUDIO_CONTROL.init(usb_audio::Control::new()));
    let cdc = CdcAcmClass::new(&mut builder, CDC.init(State::new()), 64);
    let (command, reply, iq) = usb_vendor::add(&mut builder);
    let playback = usb_audio::speaker(&mut builder);
    spawner.spawn(usb_task(builder.build()).unwrap());
    spawner.spawn(bootsel_task().unwrap());
    spawner.spawn(audio_task(audio).unwrap());
    spawner.spawn(playback_task(playback).unwrap());
    spawner.spawn(cat_task(cdc).unwrap());
    spawner.spawn(control_task(command, reply).unwrap());
    spawner.spawn(iq_task(iq).unwrap());
    spawner.spawn(cmx_reset_task(cmx_resetn).unwrap());
    static STACK: StaticCell<Stack<65536>> = StaticCell::new();
    static EXECUTOR: StaticCell<Executor> = StaticCell::new();
    let capture = capture::Capture::new(p.PIO0, p.DMA_CH0, p.PIN_8, p.PIN_9, p.PIN_10, Irqs);
    spawn_core1(p.CORE1, STACK.init(Stack::new()), move || {
        EXECUTOR
            .init(Executor::new())
            .run(|spawner| spawner.spawn(engine_task(capture).unwrap()));
    });
    let mut chip = Chip::new(
        i2c_bus::Bus::new(p.I2C0, p.PIN_1, p.PIN_0),
        embassy_time::Delay,
    );
    let settings = radio.settings;
    if let Err(e) = apply(
        &mut radio,
        settings,
        Action::Configure,
        &mut chip,
        &mut flash,
        &mut save_seq,
    )
    .await
    {
        radio.fault(e as u32);
    }
    let mut last_status = Instant::now();
    let mut vox = Vox::default();
    let mut tx_plan = None;
    let mut tx_last_frequency = None;
    let mut tx_measurement = None;
    let mut manual_tx = false;
    let mut manual_ldiv = false;
    let mut manual_rx_hold = false;
    let mut tx_audio_time = Instant::now();
    let mut vox_time = Instant::now();
    let mut recover_rx = false;
    let mut tx_health_time = Instant::now();
    loop {
        radio.streaming = STREAMING.load(Ordering::Acquire);
        radio.tx_active = TX_ACTIVE.load(Ordering::Acquire);
        radio.usb_faults = IQ_FAULTS.load(Ordering::Relaxed);
        radio.dropped = IQ_DROPS.load(Ordering::Relaxed);
        radio.audio_stalls = USB_STALLS.load(Ordering::Relaxed);
        AUDIO.lock(|q| {
            let q = q.borrow();
            radio.underruns = q.underruns;
            radio.overruns = q.overruns;
        });
        let error = CAPTURE_ERROR.swap(0, Ordering::AcqRel);
        if error != 0 {
            radio.fault(error);
        }
        if recover_rx {
            manual_tx = false;
            manual_ldiv = false;
            manual_rx_hold = true;
            unkey_tx_relay(&mut ptt, &mut tx_led, radio.settings.tx_relay_delay_ms).await;
            TX_ACTIVE.store(false, Ordering::Release);
            physical_cmx_reset().await;
            let settings = radio.settings;
            if let Err(error) = apply(
                &mut radio,
                settings,
                Action::Configure,
                &mut chip,
                &mut flash,
                &mut save_seq,
            )
            .await
            {
                radio.fault(error as u32);
            }
            tx_plan = None;
            tx_last_frequency = None;
            vox = Vox::default();
            recover_rx = false;
        }
        if let Some(measurement) = TX_AUDIO.try_take() {
            tx_measurement = measurement;
            tx_audio_time = Instant::now();
        }
        if tx_audio_time.elapsed() > Duration::from_millis(20) {
            tx_measurement = None;
        }
        if !manual_tx
            && !tx_measurement.is_some_and(|m| {
                m.frequency_hz.is_some()
                    && m.rms >= libm::powf(10.0, radio.settings.tx_threshold_dbfs as f32 / 20.0)
            })
        {
            manual_rx_hold = false;
        }
        let elapsed_ms = vox_time.elapsed().as_millis().min(1000);
        let mut should_transmit = tx_plan.is_some();
        for _ in 0..elapsed_ms {
            should_transmit = vox.tick(
                radio.settings.tx_enabled && !manual_tx && !manual_rx_hold,
                tx_measurement,
                radio.settings.tx_threshold_dbfs,
                radio.settings.tx_key_ms,
                radio.settings.tx_unkey_ms,
            );
        }
        if elapsed_ms > 0 {
            vox_time = Instant::now();
        }
        if manual_tx {
            should_transmit = true;
        }
        if should_transmit && tx_plan.is_none() {
            let Some(measurement) = tx_measurement else {
                continue;
            };
            let Some(tone) = measurement.frequency_hz else {
                continue;
            };
            ptt.set_low();
            publish_iq_state(&radio);
            TX_ACTIVE.store(true, Ordering::Release);
            if let Err(error) = engine(Engine::Pause).await {
                radio.fault(error as u32);
                recover_rx = true;
                continue;
            }
            while BLOCKS.try_receive().is_ok() {}
            physical_cmx_reset().await;
            match tx_rf(radio.settings, tone).and_then(tx_pll::plan_millihz) {
                Ok(_) => match tx_configure(
                    &mut chip,
                    radio.settings,
                    tx_rf(radio.settings, tone).unwrap(),
                )
                .await
                {
                    Ok(mut plan) => {
                        key_tx_relay(&mut ptt, &mut tx_led, radio.settings.tx_relay_delay_ms).await;
                        // Configuration awaits can outlast the audio packet
                        // that requested TX. Requalify before enabling GP21.
                        if let Some(latest) = TX_AUDIO.try_take() {
                            tx_measurement = latest;
                            tx_audio_time = Instant::now();
                        }
                        let valid = tx_audio_time.elapsed() < Duration::from_millis(20)
                            && tx_measurement.is_some_and(|m| {
                                m.rms
                                    >= libm::powf(
                                        10.0,
                                        radio.settings.tx_threshold_dbfs as f32 / 20.0,
                                    )
                                    && m.frequency_hz
                                        .is_some_and(|f| (100.0..=5000.0).contains(&f))
                            });
                        if !valid {
                            recover_rx = true;
                            continue;
                        }
                        let fresh_rf = tx_rf(
                            radio.settings,
                            tx_measurement.unwrap().frequency_hz.unwrap(),
                        )
                        .unwrap();
                        plan = match chip.retune_tx(fresh_rf, plan).await {
                            Ok(updated) => updated,
                            Err(error) => {
                                radio.fault(error as u32);
                                recover_rx = true;
                                continue;
                            }
                        };
                        unsafe { tx_divider_enable(plan.divider, radio.settings.tx_differential) };
                        TX_MONITOR_AUDIO_KEYED.store(true, Ordering::Release);
                        publish_iq_state(&radio);
                        TX_ACTIVE.store(true, Ordering::Release);
                        tx_last_frequency = Some(fresh_rf);
                        tx_plan = Some(plan);
                        defmt::info!("TX entered tone={}Hz divider={}", tone as u32, plan.divider);
                    }
                    Err(error) => {
                        unsafe { tx_divider_disable() };
                        radio.fault(error as u32);
                        recover_rx = true;
                    }
                },
                Err(error) => {
                    radio.fault(error as u32);
                    recover_rx = true;
                }
            }
        } else if !should_transmit && tx_plan.is_some() {
            unkey_tx_relay(&mut ptt, &mut tx_led, radio.settings.tx_relay_delay_ms).await;
            TX_ACTIVE.store(false, Ordering::Release);
            physical_cmx_reset().await;
            tx_plan = None;
            tx_last_frequency = None;
            let settings = radio.settings;
            if let Err(error) = apply(
                &mut radio,
                settings,
                Action::Configure,
                &mut chip,
                &mut flash,
                &mut save_seq,
            )
            .await
            {
                radio.fault(error as u32);
            }
            defmt::info!("TX exited");
        } else if let (Some(plan), Some(measurement)) = (tx_plan, tx_measurement)
            && !manual_tx
            && let Some(tone) = measurement.frequency_hz
            && let Ok(rf) = tx_rf(radio.settings, tone)
            && tx_last_frequency != Some(rf)
        {
            match chip.retune_tx(rf, plan).await {
                Ok(updated) => {
                    tx_last_frequency = Some(rf);
                    tx_plan = Some(updated);
                }
                Err(error) => {
                    unkey_tx_relay(&mut ptt, &mut tx_led, radio.settings.tx_relay_delay_ms).await;
                    TX_ACTIVE.store(false, Ordering::Release);
                    tx_plan = None;
                    radio.fault(error as u32);
                    recover_rx = true;
                }
            }
        }
        if tx_plan.is_some() && tx_health_time.elapsed() >= Duration::from_millis(100) {
            tx_health_time = Instant::now();
            match with_timeout(Duration::from_millis(15), chip.read(0x06)).await {
                Ok(Ok(status)) if status & 0x28 == 0x28 => radio.chip_status = status,
                _ => {
                    unkey_tx_relay(&mut ptt, &mut tx_led, radio.settings.tx_relay_delay_ms).await;
                    recover_rx = true;
                    radio.fault(Error::Timeout as u32);
                }
            }
        }
        match select(
            REQUESTS.receive(),
            select(
                CAT_REQUESTS.receive(),
                Timer::at(vox_time + Duration::from_millis(1)),
            ),
        )
        .await
        {
            Either::First(raw) => {
                radio.tx_active = TX_ACTIVE.load(Ordering::Acquire);
                if let Ok((cmd, payload)) = a::parse(&raw) {
                    if cmd == a::RX_TUNING_ERROR && manual_tx {
                        REPLIES.send(a::reply(&raw, a::Status::Busy, &[])).await;
                        continue;
                    }
                    if cmd == a::TX_MEASURE {
                        let reply = if !payload.is_empty() {
                            a::reply(&raw, a::Status::Length, &[])
                        } else if !manual_tx || !manual_ldiv {
                            a::reply(&raw, a::Status::Busy, &[])
                        } else {
                            match tx_counter::measure().await {
                                Ok(values) => a::reply(&raw, a::Status::Ok, &values),
                                Err(_) => a::reply(&raw, a::Status::Io, &[]),
                            }
                        };
                        REPLIES.send(reply).await;
                        continue;
                    }
                    if cmd == a::TX_CARRIER {
                        use astra918_firmware::tx_control::{self, Carrier};
                        let request = match Carrier::parse(payload) {
                            Ok(request) => request,
                            Err(status) => {
                                REPLIES.send(a::reply(&raw, status, &[])).await;
                                continue;
                            }
                        };
                        let frequency = match request {
                            Carrier::Stop => 0,
                            Carrier::Rf(hz) | Carrier::Ldiv(hz) => hz,
                        };
                        if request != Carrier::Stop && TX_ACTIVE.load(Ordering::Acquire) {
                            let reply = if manual_tx
                                && !manual_ldiv
                                && matches!(request, Carrier::Rf(_))
                            {
                                match chip.retune_tx(frequency * 1000, tx_plan.unwrap()).await {
                                    Ok(updated) => {
                                        tx_plan = Some(updated);
                                        a::reply(
                                            &raw,
                                            a::Status::Ok,
                                            &tx_control::report(1, updated),
                                        )
                                    }
                                    Err(error) => {
                                        unkey_tx_relay(
                                            &mut ptt,
                                            &mut tx_led,
                                            radio.settings.tx_relay_delay_ms,
                                        )
                                        .await;
                                        TX_ACTIVE.store(false, Ordering::Release);
                                        recover_rx = true;
                                        a::reply(&raw, a::Status::Io, &(error as u32).to_le_bytes())
                                    }
                                }
                            } else {
                                a::reply(&raw, a::Status::Busy, &[])
                            };
                            REPLIES.send(reply).await;
                            continue;
                        }
                        if frequency != 0 && tx_pll::plan(frequency).is_err() {
                            REPLIES.send(a::reply(&raw, a::Status::Argument, &[])).await;
                            continue;
                        }
                        // Explicit manual operation does not edit saved TX
                        // permission, CAT dial or any receiver settings.
                        unkey_tx_relay(&mut ptt, &mut tx_led, radio.settings.tx_relay_delay_ms)
                            .await;
                        manual_tx = false;
                        manual_ldiv = false;
                        manual_rx_hold = true;
                        TX_ACTIVE.store(false, Ordering::Release);
                        tx_plan = None;
                        tx_last_frequency = None;
                        vox = Vox::default();
                        let result: Result<Option<[u8; 40]>, Error> = async {
                            engine(Engine::Pause).await?;
                            while BLOCKS.try_receive().is_ok() {}
                            physical_cmx_reset().await;
                            if request == Carrier::Stop {
                                let settings = radio.settings;
                                apply(
                                    &mut radio,
                                    settings,
                                    Action::Configure,
                                    &mut chip,
                                    &mut flash,
                                    &mut save_seq,
                                )
                                .await?;
                                return Ok(None);
                            }
                            publish_iq_state(&radio);
                            TX_ACTIVE.store(true, Ordering::Release);
                            let plan =
                                tx_configure(&mut chip, radio.settings, frequency * 1000).await?;
                            if matches!(request, Carrier::Rf(_)) {
                                key_tx_relay(
                                    &mut ptt,
                                    &mut tx_led,
                                    radio.settings.tx_relay_delay_ms,
                                )
                                .await;
                                unsafe {
                                    tx_divider_enable(plan.divider, radio.settings.tx_differential)
                                };
                            }
                            // LDIV tests leave GP21 and the PA's PTT low.
                            manual_tx = true;
                            manual_ldiv = matches!(request, Carrier::Ldiv(_));
                            tx_plan = Some(plan);
                            tx_health_time = Instant::now();
                            Ok(Some(tx_control::report(
                                if matches!(request, Carrier::Rf(_)) {
                                    1
                                } else {
                                    2
                                },
                                plan,
                            )))
                        }
                        .await;
                        let reply = match result {
                            Ok(Some(report)) => a::reply(&raw, a::Status::Ok, &report),
                            Ok(None) => {
                                radio.tx_active = false;
                                radio.response(&raw, a::Status::Ok)
                            }
                            Err(error) => {
                                unkey_tx_relay(
                                    &mut ptt,
                                    &mut tx_led,
                                    radio.settings.tx_relay_delay_ms,
                                )
                                .await;
                                TX_ACTIVE.store(false, Ordering::Release);
                                manual_tx = false;
                                tx_plan = None;
                                physical_cmx_reset().await;
                                let settings = radio.settings;
                                if let Err(rx_error) = apply(
                                    &mut radio,
                                    settings,
                                    Action::Configure,
                                    &mut chip,
                                    &mut flash,
                                    &mut save_seq,
                                )
                                .await
                                {
                                    radio.fault(rx_error as u32);
                                }
                                defmt::warn!("Manual TX operation failed error={}", error as u32);
                                a::reply(&raw, a::Status::Io, &(error as u32).to_le_bytes())
                            }
                        };
                        REPLIES.send(reply).await;
                        continue;
                    }
                }
                let prepared = a::parse(&raw).and_then(|(cmd, p)| radio.prepare(cmd, p));
                let mut register_value = None;
                let status = match prepared {
                    Err(e) => e,
                    Ok((s, action)) => {
                        match apply(&mut radio, s, action, &mut chip, &mut flash, &mut save_seq)
                            .await
                        {
                            Ok(value) => {
                                register_value = value;
                                a::Status::Ok
                            }
                            Err(e) => {
                                if matches!(
                                    action,
                                    Action::Configure
                                        | Action::FactoryReset
                                        | Action::Clock
                                        | Action::Retry
                                        | Action::IqSettings
                                        | Action::CalibrateNow
                                        | Action::Capacitor
                                        | Action::Channel
                                ) {
                                    let _ = engine(Engine::Pause).await;
                                    READY.store(false, Ordering::Release);
                                    radio.fault(e as u32);
                                }
                                a::Status::Io
                            }
                        }
                    }
                };
                let reply = if status == a::Status::Ok {
                    if let Some(value) = register_value {
                        a::reply(&raw, status, &[value])
                    } else {
                        radio.response(&raw, status)
                    }
                } else {
                    radio.response(&raw, status)
                };
                REPLIES.send(reply).await;
            }
            Either::Second(Either::First(command)) => {
                let result = match radio.cat_prepare(command) {
                    Err(_) => Err(Error::Protocol),
                    Ok(None) => Ok(()),
                    Ok(Some((s, action))) => {
                        let result =
                            apply(&mut radio, s, action, &mut chip, &mut flash, &mut save_seq)
                                .await;
                        if let Err(error) = result
                            && matches!(
                                action,
                                Action::Configure
                                    | Action::FactoryReset
                                    | Action::Clock
                                    | Action::Retry
                                    | Action::IqSettings
                                    | Action::CalibrateNow
                                    | Action::Capacitor
                                    | Action::Channel
                            )
                        {
                            let _ = engine(Engine::Pause).await;
                            READY.store(false, Ordering::Release);
                            radio.fault(error as u32);
                        }
                        result.map(|_| ())
                    }
                };
                CAT_REPLIES
                    .send(if result.is_ok() {
                        radio.cat_reply(command)
                    } else {
                        Reply::literal(b"?;")
                    })
                    .await;
            }
            Either::Second(Either::Second(())) => {}
        }
        if !TX_ACTIVE.load(Ordering::Acquire) && last_status.elapsed() > Duration::from_secs(1) {
            last_status = Instant::now();
            if let Ok(Ok((status, rssi))) =
                with_timeout(Duration::from_millis(100), chip.status()).await
            {
                radio.chip_status = status;
                radio.rssi = rssi;
            }
        }
    }
}
#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: embassy_rp::block::ImageDef = embassy_rp::block::ImageDef::secure_exe();
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // A fatal software error must not leave the external PA keyed.
    unsafe { tx_divider_disable() };
    embassy_rp::pac::SIO
        .gpio_out(0)
        .value_clr()
        .write_value((1 << 6) | (1 << 25));
    READY.store(false, Ordering::Release);
    STREAMING.store(false, Ordering::Release);
    defmt::error!("panic {}", defmt::Display2Format(info));
    cortex_m::interrupt::disable();
    loop {
        cortex_m::asm::wfi();
    }
}
