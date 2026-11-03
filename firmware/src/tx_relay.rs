//! Relay sequencing independent of RF hardware. RF remains muted while keying.
use embedded_hal_async::delay::DelayNs;

pub async fn key(delay: &mut impl DelayNs, ms: u16, mut ptt: impl FnMut(bool)) {
    ptt(true);
    delay.delay_ms(u32::from(ms)).await;
}

pub async fn unkey(
    delay: &mut impl DelayNs,
    ms: u16,
    keyed: bool,
    mute_rf: impl FnOnce(),
    mut ptt: impl FnMut(bool),
) {
    mute_rf();
    if keyed {
        delay.delay_ms(u32::from(ms)).await;
    }
    ptt(false);
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{cell::RefCell, vec, vec::Vec};

    struct Delay<'a>(&'a RefCell<Vec<(&'static str, u32)>>);
    impl DelayNs for Delay<'_> {
        async fn delay_ns(&mut self, ns: u32) {
            self.0.borrow_mut().push(("wait_ns", ns));
        }
    }

    #[test]
    fn ptt_precedes_key_wait_and_rf_mute_precedes_unkey_wait() {
        for ms in [0, 50, 1000] {
            let log = RefCell::new(Vec::new());
            futures::executor::block_on(async {
                key(&mut Delay(&log), ms, |on| {
                    log.borrow_mut().push(("ptt", u32::from(on)));
                })
                .await;
                log.borrow_mut().push(("rf_on", 1));
                unkey(
                    &mut Delay(&log),
                    ms,
                    true,
                    || log.borrow_mut().push(("rf_off", 0)),
                    |on| log.borrow_mut().push(("ptt", u32::from(on))),
                )
                .await;
            });
            assert_eq!(
                *log.borrow(),
                vec![
                    ("ptt", 1),
                    ("wait_ns", u32::from(ms) * 1_000_000),
                    ("rf_on", 1),
                    ("rf_off", 0),
                    ("wait_ns", u32::from(ms) * 1_000_000),
                    ("ptt", 0),
                ]
            );
        }
    }

    #[test]
    fn recovery_of_unkeyed_setup_does_not_wait_again() {
        let log = RefCell::new(Vec::new());
        futures::executor::block_on(unkey(
            &mut Delay(&log),
            50,
            false,
            || log.borrow_mut().push(("rf_off", 0)),
            |on| log.borrow_mut().push(("ptt", u32::from(on))),
        ));
        assert_eq!(*log.borrow(), vec![("rf_off", 0), ("ptt", 0)]);
    }
}
