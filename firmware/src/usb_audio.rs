//! UAC1 mono capture, fixed nominal 12 kHz / signed 16-bit PCM.
//! USB Audio 1.0 sections 4.3, 4.5, 4.6 and 5.2.3; no feature unit controls.
use core::sync::atomic::Ordering;
use embassy_usb::{
    Builder, Handler,
    control::{InResponse, OutResponse, Recipient, Request, RequestType},
    descriptor::{SynchronizationType, UsageType},
    driver::{Driver, Endpoint},
    types::InterfaceNumber,
};

pub struct Control {
    endpoint: u8,
    streaming_interface: Option<InterfaceNumber>,
    alternate: u8,
}
impl Control {
    pub const fn new() -> Self {
        Self {
            endpoint: 0,
            streaming_interface: None,
            alternate: 0,
        }
    }
    #[cfg(test)]
    pub fn interface_number(&self) -> InterfaceNumber {
        self.streaming_interface.unwrap()
    }
    fn stop(&mut self) {
        self.alternate = 0;
        crate::audio_active(false);
    }
    fn matches(&self, req: Request) -> bool {
        req.request_type == RequestType::Class
            && req.recipient == Recipient::Endpoint
            && req.index == u16::from(self.endpoint)
    }
}
impl Handler for Control {
    fn reset(&mut self) {
        self.stop();
        crate::USB_EPOCH.fetch_add(1, Ordering::AcqRel);
    }
    fn configured(&mut self, configured: bool) {
        if !configured {
            self.stop();
        }
    }
    fn enabled(&mut self, enabled: bool) {
        if !enabled {
            self.stop();
        }
    }
    fn suspended(&mut self, suspended: bool) {
        crate::audio_active(!suspended && self.alternate == 1);
    }
    fn set_alternate_setting(&mut self, iface: InterfaceNumber, alt: u8) {
        if Some(iface) == self.streaming_interface {
            self.alternate = alt;
            crate::audio_active(alt == 1);
        }
    }
    fn control_out(&mut self, req: Request, data: &[u8]) -> Option<OutResponse> {
        if !self.matches(req) {
            return None;
        }
        Some(
            if req.request == 1 && req.value == 0x0100 && req.length == 3 && data == [0xe0, 0x2e, 0]
            {
                OutResponse::Accepted
            } else {
                OutResponse::Rejected
            },
        )
    }
    fn control_in<'a>(&'a mut self, req: Request, buf: &'a mut [u8]) -> Option<InResponse<'a>> {
        if !self.matches(req) {
            return None;
        }
        if req.value != 0x0100
            || req.length != 3
            || buf.len() < 3
            || !matches!(req.request, 0x81..=0x84)
        {
            return Some(InResponse::Rejected);
        }
        // GET_CUR/MIN/MAX = fixed rate; GET_RES = zero, no variable range.
        buf[..3].copy_from_slice(if req.request == 0x84 {
            &[0, 0, 0]
        } else {
            &[0xe0, 0x2e, 0]
        });
        Some(InResponse::Accepted(&buf[..3]))
    }
}

pub fn microphone<'d, D: Driver<'d>>(
    builder: &mut Builder<'d, D>,
    control: &'d mut Control,
) -> D::EndpointIn {
    let mut function = builder.function(1, 1, 0);
    let mut interface = function.interface();
    let streaming = u8::from(interface.interface_number()) + 1;
    let mut alt = interface.alt_setting(1, 1, 0, None);
    // AC header (9) + line input terminal (12) + USB output terminal (9).
    alt.descriptor(0x24, &[1, 0, 1, 30, 0, 1, streaming]);
    // Present demodulated radio audio as Line Connector (USB terminal 0x0603).
    // Windows creates Radio Receiver (0x0710) endpoints disabled and hidden;
    // Line Connector is exempt from that policy. This is a host audio category,
    // not an extra physical line input. See docs/WINDOWS-AUDIO.md for sources.
    alt.descriptor(0x24, &[2, 1, 0x03, 0x06, 0, 1, 0, 0, 0, 0]);
    alt.descriptor(0x24, &[3, 2, 0x01, 0x01, 0, 1, 0]);
    let mut interface = function.interface();
    let streaming_number = interface.interface_number();
    interface.alt_setting(1, 2, 0, None); // alt 0: zero bandwidth
    let mut alt = interface.alt_setting(1, 2, 0, None); // alt 1: recording
    alt.descriptor(0x24, &[1, 2, 0, 1, 0]); // terminal 2, PCM
    alt.descriptor(0x24, &[2, 1, 1, 2, 16, 1, 0xe0, 0x2e, 0]); // Type I, one discrete rate
    let endpoint = alt.endpoint_isochronous_in(
        None,
        26,
        1,
        SynchronizationType::Asynchronous,
        UsageType::DataEndpoint,
        &[0, 0],
    ); // no explicit feedback needed for an IN source
    alt.descriptor(0x25, &[1, 1, 0, 0, 0]); // sampling frequency control only
    control.endpoint = endpoint.info().addr.into();
    control.streaming_interface = Some(streaming_number);
    drop(function);
    builder.handler(control);
    endpoint
}
