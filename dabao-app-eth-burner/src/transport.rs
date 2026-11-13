//! Host transport: reads framed requests, answers get-version and malformed
//! requests directly, and hands the rest to the app (`Handler`).
//!
//! * device: the dabao vendor HID interface (`usb-bao1x/generic-hid`, 64-byte
//!   reports, plain chunking, see `protocol::to_reports`);
//! * emulator: a TCP socket carrying the raw frames, on 127.0.0.1 port
//!   `ETH_BURNER_PORT` (default 7600).
use crate::protocol::{self, ErrorCode, Request, TYPE_CUSTOM, TYPE_ERC4527};

/// Answers a request that `handle` doesn't answer itself (`ty`, message);
/// returns the reply frame.
pub trait Handler {
    fn request(&mut self, ty: u8, message: &[u8]) -> Vec<u8>;
}

pub fn error_frame(code: ErrorCode, msg: &str) -> Vec<u8> {
    protocol::encode_frame(TYPE_CUSTOM, &protocol::error_response(code, msg))
}

/// Turn one `type || message` body into a response frame.
pub fn handle(body: &[u8], app: &mut impl Handler) -> Vec<u8> {
    let (ty, message) = match protocol::split_body(body) {
        Ok(x) => x,
        Err(e) => return error_frame(e, "empty message"),
    };
    match ty {
        TYPE_ERC4527 => app.request(ty, message),
        TYPE_CUSTOM => match protocol::decode_request(message) {
            Ok(Request::GetVersion) => {
                protocol::encode_frame(TYPE_CUSTOM, &protocol::version_response(crate::VERSION))
            }
            Ok(Request::GetAddress(_) | Request::ProbeConfirm | Request::Reboot) => app.request(ty, message),
            Err(e) => error_frame(e, "bad request"),
        },
        _ => error_frame(ErrorCode::Unsupported, "unknown message type"),
    }
}

#[cfg(not(target_os = "xous"))]
pub fn serve(mut app: impl Handler) -> ! {
    use std::io::{Read, Write};
    let port: u16 = std::env::var("ETH_BURNER_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(7600);
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("can't bind the emulator socket");
    log::info!("emulator transport: listening on 127.0.0.1:{port}");
    loop {
        let Ok((mut stream, peer)) = listener.accept() else { continue };
        log::info!("host connected from {peer}");
        loop {
            let mut prefix = [0u8; 4];
            if stream.read_exact(&mut prefix).is_err() {
                break;
            }
            let reply = match protocol::frame_len(prefix) {
                Ok(n) => {
                    let mut body = vec![0u8; n];
                    if stream.read_exact(&mut body).is_err() {
                        break;
                    }
                    handle(&body, &mut app)
                }
                Err(e) => {
                    // the stream can't be delimited any more: answer and drop it
                    stream.write_all(&error_frame(e, "bad length")).ok();
                    break;
                }
            };
            if stream.write_all(&reply).is_err() {
                break;
            }
        }
        log::info!("host disconnected");
    }
}

#[cfg(target_os = "xous")]
pub fn serve(mut app: impl Handler) -> ! {
    use protocol::{Assembled, Reassembler};
    let usb = usb_bao1x::UsbHid::new();
    let mut rx = Reassembler::default();
    loop {
        let report = match usb.hid_wait_incoming() {
            Ok(r) => r,
            Err(e) => {
                log::warn!("HID receive: {:?}", e);
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        };
        let reply = match rx.push(&report) {
            Assembled::Pending => continue,
            Assembled::Body(body) => handle(&body, &mut app),
            Assembled::Invalid => error_frame(ErrorCode::InvalidRequest, "bad length"),
        };
        for r in protocol::to_reports(&reply) {
            if let Err(e) = usb.hid_send(r) {
                log::warn!("HID send: {:?}", e);
                break;
            }
        }
    }
}
