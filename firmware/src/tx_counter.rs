//! USB-requested FC0 measurement of the raw GP20 clock, with pad restoration.
use astra918_firmware::Error;
use core::sync::atomic::{AtomicBool, Ordering};
use embassy_rp::pac;
use embassy_time::Timer;

static PAD_BUSY: AtomicBool = AtomicBool::new(false);

struct Restore {
    ctrl: u32,
    pad: u32,
    gpout: u32,
    reference: u32,
    minimum: u32,
    maximum: u32,
    interval: u32,
}
impl Drop for Restore {
    fn drop(&mut self) {
        pac::CLOCKS
            .fc0_src()
            .write(|w| w.set_fc0_src(pac::clocks::vals::Fc0src::NULL));
        pac::CLOCKS.fc0_ref_khz().write(|w| w.0 = self.reference);
        pac::CLOCKS.fc0_min_khz().write(|w| w.0 = self.minimum);
        pac::CLOCKS.fc0_max_khz().write(|w| w.0 = self.maximum);
        pac::CLOCKS.fc0_interval().write(|w| w.0 = self.interval);
        pac::IO_BANK0.gpio(20).ctrl().write(|w| w.0 = self.ctrl);
        pac::PADS_BANK0.gpio(20).write(|w| w.0 = self.pad);
        pac::CLOCKS.clk_gpout_ctrl(0).write(|w| w.0 = self.gpout);
        PAD_BUSY.store(false, Ordering::Release);
    }
}

pub async fn measure() -> Result<[u8; 64], Error> {
    if PAD_BUSY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(Error::Protocol);
    }
    let _restore = Restore {
        ctrl: pac::IO_BANK0.gpio(20).ctrl().read().0,
        pad: pac::PADS_BANK0.gpio(20).read().0,
        gpout: pac::CLOCKS.clk_gpout_ctrl(0).read().0,
        reference: pac::CLOCKS.fc0_ref_khz().read().0,
        minimum: pac::CLOCKS.fc0_min_khz().read().0,
        maximum: pac::CLOCKS.fc0_max_khz().read().0,
        interval: pac::CLOCKS.fc0_interval().read().0,
    };
    // Count the non-inverted pin-8 input independently of GPOUT0. This
    // temporarily pauses the external divider without changing the CMX918.
    pac::CLOCKS
        .clk_gpout_ctrl(0)
        .modify(|w| w.set_enable(false));
    pac::IO_BANK0
        .gpio(20)
        .ctrl()
        .modify(|w| w.0 = (w.0 & !(0x1f | (3 << 16))) | 9);
    pac::PADS_BANK0.gpio(20).modify(|w| {
        w.set_ie(true);
        w.set_iso(false);
        w.set_pue(false);
        w.set_pde(false);
    });
    pac::CLOCKS
        .fc0_ref_khz()
        .write(|w| w.set_fc0_ref_khz(embassy_rp::clocks::clk_ref_freq() / 1000));
    pac::CLOCKS.fc0_min_khz().write(|w| w.set_fc0_min_khz(0));
    pac::CLOCKS
        .fc0_max_khz()
        .write(|w| w.set_fc0_max_khz(0x01ff_ffff));
    pac::CLOCKS.fc0_interval().write(|w| w.set_fc0_interval(15)); // 32.768 ms gate
    let mut bytes = [0; 64];
    for sample in bytes.chunks_exact_mut(8) {
        pac::CLOCKS
            .fc0_src()
            .write(|w| w.set_fc0_src(pac::clocks::vals::Fc0src::NULL));
        Timer::after_micros(10).await;
        pac::CLOCKS
            .fc0_src()
            .write(|w| w.set_fc0_src(pac::clocks::vals::Fc0src::CLKSRC_GPIN0));
        // Allow the reference-clock domain to clear the previous DONE flag
        // before polling this gate, rather than reusing a stale result.
        Timer::after_micros(10).await;
        let mut status = pac::CLOCKS.fc0_status().read();
        for _ in 0..100 {
            if status.done() {
                break;
            }
            Timer::after_millis(1).await;
            status = pac::CLOCKS.fc0_status().read();
        }
        sample[..4].copy_from_slice(&status.0.to_le_bytes());
        let value = if status.done() {
            pac::CLOCKS.fc0_result().read().0
        } else {
            0
        };
        sample[4..].copy_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}
