//! SPI slave implemented in PIO on the required GP8/9/10 wiring.
//! DMA0 runs continuously into an aligned ring; no per-sample CPU interrupts.
//! PIO0/SM0 and DMA0 are owned exclusively by this object for its full lifetime.

use astra918_firmware::{
    Error, RingCursor,
    protocol::{BLOCK_BYTES, SAMPLES},
};
use core::{
    cell::UnsafeCell,
    ptr,
    sync::atomic::{Ordering, compiler_fence},
};
use embassy_rp::{
    Peri, pac,
    peripherals::{DMA_CH0, PIN_8, PIN_9, PIN_10, PIO0},
    pio,
};
use embassy_time::Instant;

const WORDS: usize = 8192;
// Conservative upper bound for unexpected chip frame rates, not advertised USB rate.
const MAX_INPUT_RATE: u32 = 1_000_000;
#[repr(C, align(32768))]
struct Memory(UnsafeCell<[u32; WORDS]>);
// SAFETY: no Rust references to the contents are exposed. DMA0 is the sole
// writer, and Capture reads individual u32 values with volatile operations.
// Capture is uniquely constructed from PIO0/DMA0 singleton tokens.
unsafe impl Sync for Memory {}
static MEMORY: Memory = Memory(UnsafeCell::new([0; WORDS]));

pub struct Capture {
    sm: pio::StateMachine<'static, PIO0, 0>,
    _common: pio::Common<'static, PIO0>,
    _program: pio::LoadedProgram<'static, PIO0>,
    _pins: [pio::Pin<'static, PIO0>; 3],
    _dma: Peri<'static, DMA_CH0>,
    config: pio::Config<'static, PIO0>,
    cursor: RingCursor,
    last_poll: Instant,
    last_progress: Instant,
}

impl Capture {
    pub fn new(
        pio: Peri<'static, PIO0>,
        dma: Peri<'static, DMA_CH0>,
        rx: Peri<'static, PIN_8>,
        cs: Peri<'static, PIN_9>,
        clk: Peri<'static, PIN_10>,
        irqs: crate::Irqs,
    ) -> Self {
        let pio::Pio {
            mut common,
            mut sm0,
            ..
        } = pio::Pio::new(pio, irqs);
        let mut rx = common.make_pio_pin(rx);
        let mut cs = common.make_pio_pin(cs);
        let mut clk = common.make_pio_pin(clk);
        rx.set_pull(embassy_rp::gpio::Pull::Down);
        cs.set_pull(embassy_rp::gpio::Pull::Up);
        clk.set_pull(embassy_rp::gpio::Pull::Down);
        // Synchronizers stay enabled. At max selected 9.6 MHz SCLK there are
        // ~15.6 system cycles/bit at 150 MHz; worst bit loop is seven instructions
        // plus external-edge waits. Electrical timing still requires validation.
        let program = ::pio::pio_asm!(
            "wait 1 gpio 9",
            ".wrap_target",
            "frame:",
            "wait 0 gpio 9",
            "set x, 31",
            "bitloop:",
            "wait 0 gpio 10",
            "jmp pin badframe",
            "wait 1 gpio 10",
            "jmp pin badframe",
            "in pins, 1",
            "jmp x-- bitloop",
            "push block",
            "wait 1 gpio 9",
            "jmp frame",
            "badframe:",
            "irq 0",
            "mov isr, null",
            "wait 1 gpio 9",
            ".wrap",
        );
        let loaded = common.load_program(&program.program);
        let mut config = pio::Config::default();
        config.use_program(&loaded, &[]);
        config.set_in_pins(&[&rx]);
        config.set_jmp_pin(&cs);
        config.shift_in = pio::ShiftConfig {
            auto_fill: false,
            threshold: 32,
            direction: pio::ShiftDirection::Left,
        };
        config.fifo_join = pio::FifoJoin::RxOnly;
        sm0.set_config(&config);
        sm0.set_pin_dirs(pio::Direction::In, &[&rx, &cs, &clk]);
        Self {
            sm: sm0,
            _common: common,
            _program: loaded,
            _pins: [rx, cs, clk],
            _dma: dma,
            config,
            cursor: RingCursor::new(WORDS),
            last_poll: Instant::now(),
            last_progress: Instant::now(),
        }
    }

    fn base() -> *mut u32 {
        MEMORY.0.get().cast::<u32>()
    }

    pub fn stop(&mut self) -> Result<(), Error> {
        self.sm.set_enable(false);
        pac::DMA.ch(0).ctrl_trig().modify(|w| w.set_en(false));
        pac::DMA.chan_abort().write(|w| w.set_chan_abort(1));
        for _ in 0..10000 {
            if pac::DMA.chan_abort().read().chan_abort() & 1 == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        defmt::error!("DMA0 abort did not complete");
        Err(Error::Capture)
    }

    pub fn start(&mut self) -> Result<(), Error> {
        self.stop()?;
        self.sm.clear_fifos();
        self.sm.restart();
        self.sm.set_config(&self.config);
        pac::PIO0.fdebug().write(|w| w.set_rxstall(1));
        pac::PIO0.irq().write(|w| w.set_irq(1));
        let dma = pac::DMA.ch(0);
        dma.read_addr().write_value(self.sm.rx_fifo_ptr() as u32);
        dma.write_addr().write_value(Self::base() as u32);
        dma.trans_count().write(|w| {
            // Match pico-sdk dma_encode_endless_transfer_count(): 0xffffffff.
            // ENDLESS disables decrementing; a zero COUNT still prevents progress.
            // https://github.com/raspberrypi/pico-sdk/blob/2.2.0/src/rp2_common/hardware_dma/include/hardware/dma.h
            w.set_count(0x0fff_ffff);
            w.set_mode(pac::dma::vals::TransCountMode::ENDLESS);
        });
        compiler_fence(Ordering::SeqCst);
        dma.ctrl_trig().write(|w| {
            w.set_data_size(pac::dma::vals::DataSize::SIZE_WORD);
            w.set_incr_write(true);
            w.set_ring_sel(true);
            w.set_ring_size(15);
            w.set_chain_to(0);
            w.set_treq_sel(self.sm.rx_treq());
            w.set_high_priority(true);
            w.set_irq_quiet(true);
            w.set_en(true);
        });
        self.cursor = RingCursor::new(WORDS);
        self.last_poll = Instant::now();
        self.last_progress = self.last_poll;
        self.sm.set_enable(true);
        Ok(())
    }

    fn poll(&mut self) -> Result<(), Error> {
        let now = Instant::now();
        let stalled = pac::PIO0.fdebug().read().rxstall() & 1 != 0;
        let early_cs = pac::PIO0.irq().read().irq() & 1 != 0;
        let dma_error = pac::DMA.ch(0).ctrl_trig().read().ahb_error();
        if stalled || early_cs || dma_error {
            defmt::error!(
                "capture hardware fault: FIFO stall={} early CS={} DMA error={}",
                stalled,
                early_cs,
                dma_error
            );
            return Err(Error::Capture);
        }
        let address = pac::DMA.ch(0).write_addr().read() as usize;
        let position = address.wrapping_sub(Self::base() as usize) / 4;
        let before = self.cursor.produced;
        let elapsed = now.duration_since(self.last_poll).as_micros();
        if let Err(error) = self.cursor.update(position, elapsed, MAX_INPUT_RATE) {
            defmt::error!(
                "capture ring fault: position={} elapsed={} us produced={} consumed={}",
                position,
                elapsed,
                self.cursor.produced,
                self.cursor.consumed
            );
            return Err(error);
        }
        self.last_poll = now;
        if self.cursor.produced != before {
            self.last_progress = now;
        }
        if now.duration_since(self.last_progress).as_millis() > 500 {
            defmt::error!("SPI capture made no progress for 500 ms");
            return Err(Error::Capture);
        }
        Ok(())
    }

    /// Copy a USB frame from the DMA ring, retaining one in every
    /// `decimation` CMX918 samples. Only x1 and explicit x2 are supported.
    pub fn read_block(
        &mut self,
        data: &mut [u8; BLOCK_BYTES],
        decimation: usize,
    ) -> Result<Option<u64>, Error> {
        if decimation != 1 && decimation != 2 {
            return Err(Error::Profile);
        }
        self.poll()?;
        // Leave eight completed words of margin behind the DMA write pointer.
        let input_samples = SAMPLES * decimation;
        if self.cursor.available() < input_samples + 8 {
            return Ok(None);
        }
        let first = self.cursor.consumed;
        cortex_m::asm::dmb();
        for (i, out) in data.chunks_exact_mut(4).enumerate() {
            let index = (first as usize + i * decimation) & (WORDS - 1);
            // SAFETY: static aligned ring, index in range, u32 has no invalid
            // bit patterns. DMA writes only this ring. No borrowed alias exists.
            // A second producer check below rejects possible ring overwrite.
            let word = unsafe { ptr::read_volatile(Self::base().add(index)) };
            out.copy_from_slice(&astra918_firmware::spi_word_to_iq(word));
        }
        compiler_fence(Ordering::SeqCst);
        self.poll()?;
        self.cursor.consumed += input_samples as u64;
        Ok(Some(first / decimation as u64))
    }
}
