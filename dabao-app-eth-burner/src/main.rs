//! Ethereum burner signer for the dabao (no screen, no PIN).
//!
//! The host talks to it over USB HID (a single vendor-defined interface) or,
//! in the emulator, a TCP socket; see `protocol.cddl`. Nothing is shown, but a
//! signing request is only signed once the person holding the device approves
//! it with a touch on pad PB1 (see `confirm`); get-version and get-address are
//! answered at once. A random seed is created on the first boot
//! from the chip TRNG and kept in the keystore's application slots; it is never
//! shown, so the funds on a burner can't be recovered elsewhere.
//!
//! The approval says "sign", not "sign this": the device can't show what it is
//! asked to sign. Use it for test networks and small amounts only.
mod confirm;
#[allow(dead_code)] // error codes and helpers the burner doesn't use
mod protocol;
mod transport;

use confirm::{Confirm, Decision};
use protocol::{ErrorCode, Request, TYPE_CUSTOM, TYPE_ERC4527};
use sha2::{Digest, Sha256};
use signer_core::DerivationPath;
use signer_signing::PublicNode;
use zeroize::Zeroizing;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The burner's seed storage: two 32-byte slots.
/// * slot 0: the entropy (that of a 24-word BIP-39 phrase);
/// * slot 1: `MAGIC` and the first 24 bytes of SHA-256(entropy), written last,
///   so a seed is only used once it is completely stored.
mod store {
    pub const MAGIC: &[u8; 8] = b"ETHBURN1";
    pub type Slot = [u8; 32];

    pub trait Slots {
        fn read(&mut self, i: usize) -> Option<Slot>;
        fn write(&mut self, i: usize, data: &Slot) -> bool;
    }

    /// Keystore application slots (`_Keystore server_`, `AppKeyOp`), as the
    /// keystore crate's client does. Dabao's BIO configuration uses 0..123.
    #[cfg(target_os = "xous")]
    pub struct Keystore(xous::CID);

    #[cfg(target_os = "xous")]
    impl Keystore {
        const BASE: usize = 132;

        pub fn open() -> Self {
            let xns = xous_names::XousNames::new().unwrap();
            Keystore(xns.request_connection_blocking(keystore_api::SERVER_NAME_KEYS).expect("keystore"))
        }

        fn call(&self, op: keystore_api::AppKey) -> keystore_api::AppKey {
            use keystore_api::AppKey;
            let Ok(mut buf) = xous_ipc::Buffer::into_buf(op) else { return AppKey::InternalError };
            if buf.lend_mut(self.0, keystore_api::Opcode::AppKeyOp as u32).is_err() {
                return AppKey::InternalError;
            }
            buf.to_original::<AppKey, _>().unwrap_or(AppKey::InternalError)
        }
    }

    #[cfg(target_os = "xous")]
    impl Slots for Keystore {
        fn read(&mut self, i: usize) -> Option<Slot> {
            use keystore_api::{AppKey, APPKEY_GUARD};
            match self.call(AppKey::ReadRequest { guard: APPKEY_GUARD, index: Self::BASE + i }) {
                AppKey::ReadResponse { data } => Some(data),
                e => {
                    log::error!("keystore read: {e:?}");
                    None
                }
            }
        }

        fn write(&mut self, i: usize, data: &Slot) -> bool {
            use keystore_api::{AppKey, APPKEY_GUARD};
            match self.call(AppKey::Write { guard: APPKEY_GUARD, index: Self::BASE + i, data: *data }) {
                AppKey::Success => true,
                // not `{e:?}`: an unanswered request still holds the data
                e @ (AppKey::AccessDenied | AppKey::WriteError | AppKey::InternalError) => {
                    log::error!("keystore write: {e:?}");
                    false
                }
                _ => {
                    log::error!("keystore write: unexpected reply");
                    false
                }
            }
        }
    }

    /// Emulator: a host file, `ETH_BURNER_SEED` or `target/eth-burner-seed.bin`
    /// in this crate. Not secret.
    #[cfg(not(target_os = "xous"))]
    pub struct HostFile(std::path::PathBuf);

    #[cfg(not(target_os = "xous"))]
    impl HostFile {
        pub fn open() -> Self {
            let path = std::env::var_os("ETH_BURNER_SEED")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eth-burner-seed.bin"));
            log::info!("emulated seed storage: {path:?} (not secret)");
            HostFile(path)
        }
    }

    #[cfg(not(target_os = "xous"))]
    impl Slots for HostFile {
        fn read(&mut self, i: usize) -> Option<Slot> {
            match std::fs::read(&self.0) {
                Ok(d) if d.len() == 64 => d[32 * i..32 * i + 32].try_into().ok(),
                _ => Some([0; 32]),
            }
        }

        fn write(&mut self, i: usize, data: &Slot) -> bool {
            let mut d = std::fs::read(&self.0).ok().filter(|d| d.len() == 64).unwrap_or_else(|| vec![0; 64]);
            d[32 * i..32 * i + 32].copy_from_slice(data);
            if let Some(dir) = self.0.parent() {
                std::fs::create_dir_all(dir).ok();
            }
            std::fs::write(&self.0, d).map_err(|e| log::error!("can't write {:?}: {e}", self.0)).is_ok()
        }
    }
}

use store::Slots;

fn header(entropy: &[u8; 32]) -> store::Slot {
    let mut h = [0u8; 32];
    h[..8].copy_from_slice(store::MAGIC);
    h[8..].copy_from_slice(&Sha256::digest(entropy)[..24]);
    h
}

/// The stored entropy, if a complete seed is stored.
fn load(slots: &mut impl Slots) -> Option<Zeroizing<[u8; 32]>> {
    let h = slots.read(1)?;
    if &h[..8] != store::MAGIC {
        return None;
    }
    let entropy = Zeroizing::new(slots.read(0)?);
    (header(&entropy) == h).then_some(entropy)
}

/// 32 bytes from the chip TRNG: the TRNG service (`getrandom`) and the
/// kernel's hardware entropy pool (server IDs), mixed with SHA-256.
fn fresh_entropy() -> Zeroizing<[u8; 32]> {
    let mut pool = Zeroizing::new([0u8; 64]);
    getrandom::getrandom(&mut pool[..32]).expect("TRNG");
    for chunk in pool[32..].chunks_mut(16) {
        let id = xous::create_server_id().expect("kernel entropy").to_array();
        for (j, w) in id.iter().enumerate() {
            chunk[4 * j..4 * j + 4].copy_from_slice(&w.to_le_bytes());
        }
    }
    let mut h = Sha256::new();
    h.update(b"eth-burner/v1");
    h.update(&pool[..]);
    Zeroizing::new(h.finalize().into())
}

/// The stored seed, or a new one stored on first boot.
fn load_or_create(slots: &mut impl Slots) -> Zeroizing<[u8; 32]> {
    if let Some(e) = load(slots) {
        log::info!("burner seed loaded");
        return e;
    }
    let entropy = fresh_entropy();
    // entropy first, header last: a power cut in between leaves no seed
    if !(slots.write(0, &entropy) && slots.write(1, &header(&entropy))) {
        panic!("can't store the burner seed");
    }
    match load(slots) {
        Some(e) if *e == *entropy => log::info!("new burner seed created and stored"),
        _ => panic!("the burner seed doesn't read back"),
    }
    entropy
}

struct Burner<C: Confirm> {
    seed: Zeroizing<[u8; 64]>,
    confirm: C,
}

fn error(code: ErrorCode, msg: &str) -> Vec<u8> {
    protocol::encode_frame(TYPE_CUSTOM, &protocol::error_response(code, msg))
}

/// Development aid: restart the whole chip shortly after the reply is sent, so
/// new firmware can be loaded without touching the board.
#[cfg(target_os = "xous")]
fn reboot_soon() -> Result<Vec<u8>, Vec<u8>> {
    let xns = xous_names::XousNames::new().map_err(|_| error(ErrorCode::Internal, "names"))?;
    let susres = xous_api_susres::Susres::new_without_hook(&xns)
        .map_err(|e| error(ErrorCode::Internal, &format!("no reboot service: {e:?}")))?;
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        log::info!("rebooting on request");
        susres.reboot(true).ok();
    });
    Ok(protocol::encode_frame(TYPE_CUSTOM, &protocol::message_response("rebooting")))
}

#[cfg(not(target_os = "xous"))]
fn reboot_soon() -> Result<Vec<u8>, Vec<u8>> {
    Err(error(ErrorCode::Unsupported, "no reboot in the emulator"))
}

impl<C: Confirm> Burner<C> {
    fn node(&self, path: &DerivationPath) -> Result<PublicNode, Vec<u8>> {
        PublicNode::from_seed(&self.seed, path).map_err(|_| error(ErrorCode::InvalidRequest, "invalid path"))
    }

    /// The custom commands `transport` doesn't answer itself.
    fn custom(&mut self, message: &[u8]) -> Result<Vec<u8>, Vec<u8>> {
        let path = match protocol::decode_request(message) {
            Ok(Request::GetAddress(path)) => path,
            Ok(Request::ProbeConfirm) => {
                let seen = self.confirm.probe();
                return Ok(protocol::encode_frame(TYPE_CUSTOM, &protocol::message_response(&seen)));
            }
            Ok(Request::Reboot) => return reboot_soon(),
            _ => return Err(error(ErrorCode::InvalidRequest, "")),
        };
        let node = self.node(&path)?;
        let address = node.address().to_checksum(None);
        log::info!("get-address {path}: {address}");
        let reply = protocol::address_response(&node.uncompressed(), &address, &node.chain_code());
        Ok(protocol::encode_frame(TYPE_CUSTOM, &reply))
    }

    /// ERC-4527 `eth-sign-request`: nothing is shown, but it is only signed
    /// once the person holding the device approves it (`confirm`).
    fn sign(&mut self, message: &[u8]) -> Result<Vec<u8>, Vec<u8>> {
        let req = signer_decoding::decode_sign_request(message)
            .map_err(|e| error(ErrorCode::InvalidRequest, &e.to_string()))?;
        let key = signer_signing::key_from_seed(&self.seed, &req.derivation_path)
            .map_err(|_| error(ErrorCode::InvalidRequest, "invalid path"))?;
        if req.address.is_some_and(|a| a != signer_signing::address_of(&key)) {
            return Err(error(ErrorCode::InvalidRequest, "address mismatch: not this device's key"));
        }
        let (how, secs) = (self.confirm.how(), self.confirm.timeout().as_secs());
        log::info!("sign-request {}: {how} within {secs} s to approve", req.derivation_path);
        match self.confirm.confirm() {
            Decision::Approved => {}
            Decision::TimedOut(seen) => {
                log::info!("sign-request {}: not approved ({seen})", req.derivation_path);
                return Err(error(ErrorCode::UserRejected, &format!("not approved: {how} within {secs} s ({seen})")));
            }
            Decision::Unavailable(why) => {
                log::error!("sign-request {}: can't ask for approval ({why})", req.derivation_path);
                return Err(error(ErrorCode::NotReady, &format!("can't ask for approval: {why}")));
            }
        }
        let sig = signer_signing::sign_request(&req, &key).map_err(|e| {
            log::error!("signing failed: {e}");
            error(ErrorCode::Internal, &e.to_string())
        })?;
        log::info!("sign-request {}: signed", req.derivation_path);
        Ok(protocol::encode_frame(TYPE_ERC4527, &signer_decoding::encode_eth_signature(req.request_id, &sig, None)))
    }
}

impl<C: Confirm> transport::Handler for Burner<C> {
    fn request(&mut self, ty: u8, message: &[u8]) -> Vec<u8> {
        let r = match ty {
            TYPE_CUSTOM => self.custom(message),
            TYPE_ERC4527 => self.sign(message),
            _ => Err(error(ErrorCode::Unsupported, "")),
        };
        r.unwrap_or_else(|e| e)
    }
}

fn main() -> ! {
    log_server::init_wait().unwrap();
    log::set_max_level(log::LevelFilter::Info);
    log::info!("eth-burner {VERSION}, PID {}", xous::process::id());

    #[cfg(target_os = "xous")]
    let mut slots = store::Keystore::open();
    #[cfg(not(target_os = "xous"))]
    let mut slots = store::HostFile::open();
    let entropy = load_or_create(&mut slots);
    let seed = Zeroizing::new(signer_signing::seed_from_entropy(&entropy[..], "").expect("seed"));
    drop(entropy);
    #[cfg(target_os = "xous")]
    let confirm = confirm::Touch::new();
    #[cfg(not(target_os = "xous"))]
    let confirm = confirm::HostFile::new();
    let burner = Burner { seed, confirm };
    let first = [(44, true), (60, true), (0, true), (0, false), (0, false)];
    let path = DerivationPath {
        components: first.iter().map(|&(index, hardened)| signer_core::ChildNumber { index, hardened }).collect(),
    };
    if let Ok(node) = burner.node(&path) {
        log::info!("burner address m/44'/60'/0'/0/0: {}", node.address().to_checksum(None));
    }
    transport::serve(burner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Mem([store::Slot; 2]);

    impl Slots for Mem {
        fn read(&mut self, i: usize) -> Option<store::Slot> {
            Some(self.0[i])
        }

        fn write(&mut self, i: usize, data: &store::Slot) -> bool {
            self.0[i] = *data;
            true
        }
    }

    #[test]
    fn a_seed_is_only_used_once_its_header_is_stored() {
        let mut m = Mem::default();
        assert!(load(&mut m).is_none());
        m.0[0] = [7; 32];
        assert!(load(&mut m).is_none(), "entropy without header");
        m.0[1] = header(&[7; 32]);
        assert_eq!(*load(&mut m).unwrap(), [7; 32]);
        m.0[0][5] ^= 1;
        assert!(load(&mut m).is_none(), "header of another seed");
    }
}
