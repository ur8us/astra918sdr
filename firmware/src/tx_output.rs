//! RP2350 GP20 clock input and selectable polarity GP21/GP13 divided TX outputs.
use astra918_firmware::tx_pins;
use embassy_rp::pac;

/// One setting updates both pad drivers without touching clocks or CMX918.
pub(crate) fn set_output_config(ma: u8, fast: bool) -> Result<(), astra918_firmware::Error> {
    let mut expected = [0; 2];
    let pins = [tx_pins::NORMAL_PIN, tx_pins::INVERTED_PIN];
    for (index, pin) in pins.into_iter().enumerate() {
        expected[index] = tx_pins::configure_pad(pac::PADS_BANK0.gpio(pin).read().0, ma, fast)?;
    }
    critical_section::with(|_| {
        for (index, pin) in pins.into_iter().enumerate() {
            pac::PADS_BANK0.gpio(pin).write(|w| w.0 = expected[index]);
        }
        for pin in pins {
            let pad = pac::PADS_BANK0.gpio(pin).read().0;
            if tx_pins::configure_pad(pad, ma, fast)? != pad {
                return Err(astra918_firmware::Error::Readback);
            }
        }
        Ok(())
    })
}
const CLK_GPOUT0_CTRL: *mut u32 = 0x4001_0000 as *mut u32;
const CLK_GPOUT0_DIV: *mut u32 = 0x4001_0004 as *mut u32;
const GP20_CTRL: *mut u32 = 0x4002_80A4 as *mut u32;
const GP20_PAD: *mut u32 = 0x4003_8054 as *mut u32;

/// Update only GP13's live output override. RX and LDIV-only tests stay low.
pub(crate) fn set_polarity(differential: bool) -> Result<(), astra918_firmware::Error> {
    critical_section::with(|_| {
        let enabled = unsafe { core::ptr::read_volatile(CLK_GPOUT0_CTRL) } & (1 << 11) != 0;
        let ctrl = pac::IO_BANK0.gpio(tx_pins::INVERTED_PIN).ctrl();
        let original = ctrl.read().0;
        let expected = tx_pins::polarity_control(original, differential, enabled);
        if expected != original {
            ctrl.write(|w| w.0 = expected);
            if ctrl.read().0 != expected {
                return Err(astra918_firmware::Error::Readback);
            }
        }
        Ok(())
    })
}

/// GP20 is the inverted clock input; GPOUT0 divides it to GP21 (normal)
/// and GP13 (OUTOVER selected by TX outputs). Both pins share the same divider.
/// The input/divider settings follow the previously qualified
/// `gp20-invert-div2-gp21.py` experiment helper.
pub(crate) unsafe fn tx_divider_enable(divider: u16, differential: bool) {
    unsafe {
        let ctrl = core::ptr::read_volatile(CLK_GPOUT0_CTRL);
        core::ptr::write_volatile(CLK_GPOUT0_CTRL, ctrl & !(1 << 11));
        let gp20 = core::ptr::read_volatile(GP20_CTRL);
        core::ptr::write_volatile(GP20_CTRL, (gp20 & !(0x1f | (3 << 16))) | 9 | (1 << 16));
        let p20 = core::ptr::read_volatile(GP20_PAD);
        core::ptr::write_volatile(
            GP20_PAD,
            (p20 & !((1 << 2) | (1 << 3) | (1 << 8))) | (1 << 6),
        );
        for pin in [tx_pins::NORMAL_PIN, tx_pins::INVERTED_PIN] {
            let original = pac::IO_BANK0.gpio(pin).ctrl().read().0;
            pac::IO_BANK0.gpio(pin).ctrl().write(|w| {
                w.0 = tx_pins::tx_control(original, pin == tx_pins::INVERTED_PIN && differential)
            });
            let pad = pac::PADS_BANK0.gpio(pin).read().0;
            pac::PADS_BANK0
                .gpio(pin)
                .write(|w| w.0 = tx_pins::output_pad(pad));
        }
        core::ptr::write_volatile(CLK_GPOUT0_DIV, u32::from(divider) << 16);
        let ctrl = core::ptr::read_volatile(CLK_GPOUT0_CTRL);
        core::ptr::write_volatile(
            CLK_GPOUT0_CTRL,
            (ctrl & !((0xf << 5) | (1 << 10) | (1 << 11))) | (1 << 5) | (1 << 11),
        );
    }
}

pub(crate) unsafe fn tx_divider_disable() {
    unsafe {
        for pin in [tx_pins::NORMAL_PIN, tx_pins::INVERTED_PIN] {
            // Override LOW first: an inverted stopped clock must not leave
            // GP13 HIGH while dekeying or recovering the receiver.
            pac::IO_BANK0
                .gpio(pin)
                .ctrl()
                .modify(|w| w.set_outover(pac::io::vals::Outover::LOW));
        }
        let ctrl = core::ptr::read_volatile(CLK_GPOUT0_CTRL);
        core::ptr::write_volatile(CLK_GPOUT0_CTRL, ctrl & !(1 << 11));
        // RP2350 SIO layout differs from RP2040. Use PAC aliases so
        // dekey cannot accidentally SET an output instead of clearing it.
        embassy_rp::pac::SIO
            .gpio_out(0)
            .value_clr()
            .write_value(tx_pins::OUTPUT_MASK);
        embassy_rp::pac::SIO
            .gpio_oe(0)
            .value_set()
            .write_value(tx_pins::OUTPUT_MASK);
        for pin in [tx_pins::NORMAL_PIN, tx_pins::INVERTED_PIN] {
            let original = pac::IO_BANK0.gpio(pin).ctrl().read().0;
            pac::IO_BANK0
                .gpio(pin)
                .ctrl()
                .write(|w| w.0 = tx_pins::rx_control(original));
            let pad = pac::PADS_BANK0.gpio(pin).read().0;
            pac::PADS_BANK0
                .gpio(pin)
                .write(|w| w.0 = tx_pins::output_pad(pad));
        }
    }
}
