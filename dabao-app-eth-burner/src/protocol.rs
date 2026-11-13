//! Wire protocol: `length (u32 BE) || type (u8) || message`, and the custom
//! CBOR messages (type 0x02) described in `protocol.cddl`.
use ciborium::value::{Integer, Value};
use signer_core::DerivationPath;

/// ERC-4527 CBOR: `eth-sign-request` in, `eth-signature` out.
pub const TYPE_ERC4527: u8 = 0x01;
/// Custom CBOR commands (`protocol.cddl`).
pub const TYPE_CUSTOM: u8 = 0x02;
/// Largest `type || message` accepted.
pub const MAX_LEN: usize = 65536;
/// Deepest derivation path accepted.
pub const MAX_PATH_DEPTH: usize = 10;

const KEY_COMMAND: u64 = 1;
const KEY_PATH: u64 = 2;
const KEY_STATUS: u64 = 1;
const KEY_PUBLIC_KEY: u64 = 2;
const KEY_ADDRESS: u64 = 3;
const KEY_CHAIN_CODE: u64 = 4;
const KEY_VERSION: u64 = 5;
const KEY_MESSAGE: u64 = 6;

const CMD_GET_ADDRESS: u64 = 1;
const CMD_GET_VERSION: u64 = 2;
/// Development aid: what the approval sensor reads right now.
const CMD_PROBE_CONFIRM: u64 = 3;
/// Development aid: restart the chip, which lands in the bootloader.
const CMD_REBOOT: u64 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    UserRejected = 1,
    InvalidRequest = 2,
    Unsupported = 3,
    Busy = 4,
    Internal = 5,
    /// The device is not set up, or was reset after too many wrong PINs.
    NotReady = 6,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    GetAddress(DerivationPath),
    GetVersion,
    ProbeConfirm,
    Reboot,
}

/// `length || type || message`.
pub fn encode_frame(ty: u8, message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + message.len());
    out.extend_from_slice(&(message.len() as u32 + 1).to_be_bytes());
    out.push(ty);
    out.extend_from_slice(message);
    out
}

/// Value of the length prefix: the number of bytes (`type || message`) that
/// follow it. Errors if it is 0 or above [`MAX_LEN`].
pub fn frame_len(prefix: [u8; 4]) -> Result<usize, ErrorCode> {
    match u32::from_be_bytes(prefix) as usize {
        0 => Err(ErrorCode::InvalidRequest),
        n if n > MAX_LEN => Err(ErrorCode::InvalidRequest),
        n => Ok(n),
    }
}

/// Split `type || message`.
pub fn split_body(body: &[u8]) -> Result<(u8, &[u8]), ErrorCode> {
    match body.split_first() {
        Some((&ty, msg)) => Ok((ty, msg)),
        None => Err(ErrorCode::InvalidRequest),
    }
}

fn int(n: u64) -> Value {
    Value::Integer(Integer::from(n))
}

fn as_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Integer(i) => u64::try_from(*i).ok(),
        Value::Tag(_, inner) => as_u64(inner),
        _ => None,
    }
}

fn get(map: &[(Value, Value)], key: u64) -> Option<&Value> {
    map.iter().find(|(k, _)| as_u64(k) == Some(key)).map(|(_, v)| v)
}

/// Decode a custom-CBOR request.
pub fn decode_request(message: &[u8]) -> Result<Request, ErrorCode> {
    let value: Value = ciborium::from_reader(message).map_err(|_| ErrorCode::InvalidRequest)?;
    let Value::Map(map) = value else { return Err(ErrorCode::InvalidRequest) };
    match get(&map, KEY_COMMAND).and_then(as_u64) {
        Some(CMD_GET_ADDRESS) => {
            let path = get(&map, KEY_PATH).ok_or(ErrorCode::InvalidRequest)?;
            let path = signer_decoding::decode_keypath(path).map_err(|_| ErrorCode::InvalidRequest)?;
            if path.components.len() > MAX_PATH_DEPTH {
                return Err(ErrorCode::InvalidRequest);
            }
            Ok(Request::GetAddress(path))
        }
        Some(CMD_GET_VERSION) => Ok(Request::GetVersion),
        Some(CMD_PROBE_CONFIRM) => Ok(Request::ProbeConfirm),
        Some(CMD_REBOOT) => Ok(Request::Reboot),
        Some(_) => Err(ErrorCode::Unsupported),
        None => Err(ErrorCode::InvalidRequest),
    }
}

fn encode(entries: Vec<(Value, Value)>) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(&Value::Map(entries), &mut out).expect("CBOR into a Vec cannot fail");
    out
}

pub fn address_response(public_key: &[u8; 65], address: &str, chain_code: &[u8; 32]) -> Vec<u8> {
    encode(vec![
        (int(KEY_STATUS), int(0)),
        (int(KEY_PUBLIC_KEY), Value::Bytes(public_key.to_vec())),
        (int(KEY_ADDRESS), Value::Text(address.into())),
        (int(KEY_CHAIN_CODE), Value::Bytes(chain_code.to_vec())),
    ])
}

pub fn version_response(version: &str) -> Vec<u8> {
    encode(vec![(int(KEY_STATUS), int(0)), (int(KEY_VERSION), Value::Text(version.into()))])
}

/// A successful reply that is only text (probe-confirm).
pub fn message_response(message: &str) -> Vec<u8> {
    encode(vec![(int(KEY_STATUS), int(0)), (int(KEY_MESSAGE), Value::Text(message.into()))])
}

pub fn error_response(code: ErrorCode, message: &str) -> Vec<u8> {
    let mut entries = vec![(int(KEY_STATUS), int(code as u64))];
    if !message.is_empty() {
        entries.push((int(KEY_MESSAGE), Value::Text(message.into())));
    }
    encode(entries)
}

#[cfg_attr(not(target_os = "xous"), allow(dead_code))] // HID: device builds
/// Cut a frame into 64-byte HID reports, zero-padding the last one.
pub fn to_reports(frame: &[u8]) -> Vec<[u8; 64]> {
    frame
        .chunks(64)
        .map(|c| {
            let mut r = [0u8; 64];
            r[..c.len()].copy_from_slice(c);
            r
        })
        .collect()
}

#[cfg_attr(not(target_os = "xous"), allow(dead_code))] // HID: device builds
/// Reassembles a frame from 64-byte HID reports.
#[derive(Default)]
pub struct Reassembler {
    buf: Vec<u8>,
    need: Option<usize>,
}

#[cfg_attr(not(target_os = "xous"), allow(dead_code))] // HID: device builds
pub enum Assembled {
    /// More reports are needed.
    Pending,
    /// A whole `type || message` body.
    Body(Vec<u8>),
    /// The length prefix was invalid; the rest of the transfer can't be
    /// delimited, so the reassembler starts over with the next report.
    Invalid,
}

#[cfg_attr(not(target_os = "xous"), allow(dead_code))] // HID: device builds
impl Reassembler {
    pub fn push(&mut self, report: &[u8; 64]) -> Assembled {
        self.buf.extend_from_slice(report);
        if self.need.is_none() {
            let prefix = [self.buf[0], self.buf[1], self.buf[2], self.buf[3]];
            match frame_len(prefix) {
                Ok(n) => self.need = Some(n),
                Err(_) => {
                    self.buf.clear();
                    return Assembled::Invalid;
                }
            }
        }
        let n = self.need.unwrap();
        if self.buf.len() < 4 + n {
            return Assembled::Pending;
        }
        // padding after the frame is discarded
        let body = self.buf[4..4 + n].to_vec();
        self.buf.clear();
        self.need = None;
        Assembled::Body(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypath(components: &[(u64, bool)], tagged: bool) -> Value {
        let comps = components.iter().flat_map(|&(i, h)| [int(i), Value::Bool(h)]).collect();
        let map = Value::Map(vec![(int(1), Value::Array(comps))]);
        if tagged { Value::Tag(304, Box::new(map)) } else { map }
    }

    fn request(entries: Vec<(Value, Value)>) -> Vec<u8> {
        encode(entries)
    }

    #[test]
    fn frames_round_trip() {
        let f = encode_frame(TYPE_CUSTOM, &[0xA1, 0x01, 0x02]);
        assert_eq!(f, [0, 0, 0, 4, 2, 0xA1, 0x01, 0x02]);
        assert_eq!(frame_len([0, 0, 0, 4]), Ok(4));
        assert_eq!(split_body(&f[4..]), Ok((TYPE_CUSTOM, &[0xA1, 0x01, 0x02][..])));
        assert!(frame_len([0, 0, 0, 0]).is_err());
        assert!(frame_len((MAX_LEN as u32 + 1).to_be_bytes()).is_err());
        assert!(split_body(&[]).is_err());
    }

    #[test]
    fn decodes_requests() {
        let path = [(44, true), (60, true), (0, true), (0, false), (0, false)];
        for tagged in [true, false] {
            let r = request(vec![(int(1), int(1)), (int(2), keypath(&path, tagged))]);
            match decode_request(&r).unwrap() {
                Request::GetAddress(p) => assert_eq!(p.to_string(), "m/44'/60'/0'/0/0"),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(decode_request(&request(vec![(int(1), int(2))])), Ok(Request::GetVersion));
        assert_eq!(decode_request(&request(vec![(int(1), int(9))])), Err(ErrorCode::Unsupported));
        assert_eq!(decode_request(&request(vec![(int(1), int(1))])), Err(ErrorCode::InvalidRequest));
        assert_eq!(decode_request(&[0xff, 0x00]), Err(ErrorCode::InvalidRequest));
        let deep: Vec<(u64, bool)> = (0..11).map(|i| (i, false)).collect();
        let r = request(vec![(int(1), int(1)), (int(2), keypath(&deep, true))]);
        assert_eq!(decode_request(&r), Err(ErrorCode::InvalidRequest));
    }

    #[test]
    fn encodes_responses() {
        let r: Value = ciborium::from_reader(&version_response("0.1.0")[..]).unwrap();
        assert_eq!(r, Value::Map(vec![(int(1), int(0)), (int(5), Value::Text("0.1.0".into()))]));
        let r: Value = ciborium::from_reader(&error_response(ErrorCode::UserRejected, "")[..]).unwrap();
        assert_eq!(r, Value::Map(vec![(int(1), int(1))]));
        let a = address_response(&[4u8; 65], "0xAb", &[7u8; 32]);
        let Value::Map(m) = ciborium::from_reader(&a[..]).unwrap() else { panic!() };
        assert_eq!(get(&m, 2), Some(&Value::Bytes(vec![4u8; 65])));
        assert_eq!(get(&m, 3), Some(&Value::Text("0xAb".into())));
        assert_eq!(get(&m, 4), Some(&Value::Bytes(vec![7u8; 32])));
    }

    #[test]
    fn hid_reports_round_trip() {
        for len in [1usize, 59, 60, 61, 200] {
            let msg: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let frame = encode_frame(TYPE_CUSTOM, &msg);
            let reports = to_reports(&frame);
            assert_eq!(reports.len(), frame.len().div_ceil(64));
            let mut r = Reassembler::default();
            let mut out = None;
            for (i, rep) in reports.iter().enumerate() {
                match r.push(rep) {
                    Assembled::Pending => assert!(i + 1 < reports.len()),
                    Assembled::Body(b) => out = Some(b),
                    Assembled::Invalid => panic!(),
                }
            }
            let body = out.unwrap();
            assert_eq!(split_body(&body).unwrap(), (TYPE_CUSTOM, &msg[..]));
        }
        let mut r = Reassembler::default();
        assert!(matches!(r.push(&[0xffu8; 64]), Assembled::Invalid));
    }
}
