#![no_std]
#![no_main]
#[cfg(not(any(feature = "rp235xa", feature = "rp235xb")))]
compile_error!("Select rp235xa or rp235xb");
#[cfg(all(feature = "rp235xa", feature = "rp235xb"))]
compile_error!("Select only one RP2350 variant");
mod capture;
mod i2c_bus;
mod usb_audio;
mod usb_vendor;
use astra918_firmware::{
    Error,
    astra::{self as a, Action, Receiver, Settings},
    audio::{AudioQueue, MAX_PACKET_BYTES},
    cat::{Command, Parser, Reply},
    chip::Chip,
    dsp::Demodulator,
    persistence as nv,
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
static REQUESTS: Channel<CriticalSectionRawMutex, [u8; 256], 1> = Channel::new();
static REPLIES: Channel<CriticalSectionRawMutex, [u8; 256], 1> = Channel::new();
static CAT_REQUESTS: Channel<CriticalSectionRawMutex, Command, 1> = Channel::new();
static CAT_REPLIES: Channel<CriticalSectionRawMutex, Reply, 1> = Channel::new();
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
static USB_STALLS: AtomicU32 = AtomicU32::new(0);
static IQ_FAULTS: AtomicU32 = AtomicU32::new(0);
static IQ_DROPS: AtomicU32 = AtomicU32::new(0);
static CAPTURE_ERROR: AtomicU32 = AtomicU32::new(0);
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
struct Events;
impl Handler for Events {
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
async fn iq_task(mut output: In) {
    loop {
        let (frame, generation) = BLOCKS.receive().await;
        if !STREAMING.load(Ordering::Acquire) || generation != GENERATION.load(Ordering::Acquire) {
            continue;
        }
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
    let mut sequence = 0u32;
    let mut sample = 0u64;
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
                        sequence = 0;
                        sample = 0;
                        settling = 12_000;
                        let result = capture.start();
                        running = result.is_ok();
                        READY.store(running, Ordering::Release);
                        result
                    }
                    Engine::Update(r) => {
                        if r.generation != radio.generation {
                            sequence = 0;
                            sample = 0;
                        }
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
            frame[..a::IQ_HEADER].copy_from_slice(&radio.iq_header(sequence, sample));
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
            sequence = sequence.wrapping_add(1);
            sample += 512;
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
) -> Result<(), Error> {
    match action {
        Action::Configure | Action::Retry => {
            r.configured = false;
            engine(Engine::Pause).await?;
            while BLOCKS.try_receive().is_ok() {}
            let configured = with_timeout(
                Duration::from_secs(4),
                chip.configure_recovering(s.hardware(), s.controls),
            )
            .await
            .map_err(|_| Error::Timeout)?;
            configured?;
            with_timeout(Duration::from_millis(100), chip.mute(false))
                .await
                .map_err(|_| Error::Timeout)??;
            let mut next = *r;
            next.commit(s, action);
            GENERATION.store(next.generation, Ordering::Release);
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
            engine(Engine::Update(*r)).await?;
        }
        Action::Start | Action::Stop => {
            if action == Action::Start && !r.configured {
                return Err(Error::NotConfigured);
            }
            r.commit(s, action);
            GENERATION.store(r.generation, Ordering::Release);
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
            GENERATION.store(r.generation, Ordering::Release);
            if r.configured {
                engine(Engine::Run(*r)).await?;
            }
            result.map_err(|_| Error::Readback)?;
        }
        Action::Query => {}
    }
    Ok(())
}
#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
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
    let audio = usb_audio::microphone(&mut builder, AUDIO_CONTROL.init(usb_audio::Control::new()));
    let cdc = CdcAcmClass::new(&mut builder, CDC.init(State::new()), 64);
    let (command, reply, iq) = usb_vendor::add(&mut builder);
    builder.handler(EVENTS.init(Events));
    spawner.spawn(usb_task(builder.build()).unwrap());
    spawner.spawn(audio_task(audio).unwrap());
    spawner.spawn(cat_task(cdc).unwrap());
    spawner.spawn(control_task(command, reply).unwrap());
    spawner.spawn(iq_task(iq).unwrap());
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
    loop {
        radio.streaming = STREAMING.load(Ordering::Acquire);
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
        match select(
            REQUESTS.receive(),
            select(CAT_REQUESTS.receive(), Timer::after_millis(10)),
        )
        .await
        {
            Either::First(raw) => {
                let prepared = a::parse(&raw).and_then(|(cmd, p)| radio.prepare(cmd, p));
                let status = match prepared {
                    Err(e) => e,
                    Ok((s, action)) => {
                        match apply(&mut radio, s, action, &mut chip, &mut flash, &mut save_seq)
                            .await
                        {
                            Ok(()) => a::Status::Ok,
                            Err(e) => {
                                if matches!(
                                    action,
                                    Action::Configure | Action::Retry | Action::Capacitor
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
                REPLIES.send(radio.response(&raw, status)).await;
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
                                Action::Configure | Action::Retry | Action::Capacitor
                            )
                        {
                            let _ = engine(Engine::Pause).await;
                            READY.store(false, Ordering::Release);
                            radio.fault(error as u32);
                        }
                        result
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
        if last_status.elapsed() > Duration::from_secs(1) {
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
    READY.store(false, Ordering::Release);
    STREAMING.store(false, Ordering::Release);
    defmt::error!("panic {}", defmt::Display2Format(info));
    cortex_m::interrupt::disable();
    loop {
        cortex_m::asm::wfi();
    }
}
