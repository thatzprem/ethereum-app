//! Approval of a signing request by the person holding the device.
//!
//! * device: a capacitive touch on one bare pad, sensed by a BIO core
//!   (`bio-lib` captouch). No extra parts: a fingertip on the pad is enough.
//! * emulator: a file created by the tester.
//!
//! Both only accept an approval given after the request arrived: a finger
//! resting on the pad (or a stale file) approves nothing. Anything that keeps
//! the sensor from working refuses the request.
use std::time::{Duration, Instant};

/// How long a request waits for its approval.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

pub enum Decision {
    Approved,
    /// Nobody approved in time; the text says what the sensor saw.
    TimedOut(String),
    /// The sensor can't be used: the request is refused.
    #[cfg_attr(not(target_os = "xous"), allow(dead_code))] // the emulator's file always works
    Unavailable(String),
}

pub trait Confirm {
    /// Human-readable name of what to do, for the log and error messages.
    fn how(&self) -> &'static str;
    fn timeout(&self) -> Duration;
    fn confirm(&mut self) -> Decision;
    /// Development aid: what the sensor reads over the next half second, as
    /// text. Approves nothing.
    fn probe(&mut self) -> String;
}

/// Capacitive touch on BIO pin 1, which is pad PB1 on the dabao.
#[cfg(target_os = "xous")]
pub struct Touch {
    pad: Option<bio_lib::captouch::Captouch>,
    why_not: String,
}

#[cfg(target_os = "xous")]
impl Touch {
    const BIO_PIN: u8 = 1;
    const POLL: Duration = Duration::from_millis(20);
    /// Readings taken at the start of a request to learn the untouched level.
    const IDLE_SAMPLES: u32 = 15;
    /// Consecutive readings above the touch level that make a touch.
    const HITS: u32 = 3;

    pub fn new() -> Self {
        match bio_lib::captouch::Captouch::new(arbitrary_int::u5::new(Self::BIO_PIN), None) {
            Ok(pad) => {
                log::info!("touch-to-confirm on pad PB1 (BIO pin {})", Self::BIO_PIN);
                Touch { pad: Some(pad), why_not: String::new() }
            }
            Err(e) => {
                log::error!("no touch sensor, signing is disabled: {e:?}");
                Touch { pad: None, why_not: format!("{e:?}") }
            }
        }
    }
}

#[cfg(target_os = "xous")]
impl Confirm for Touch {
    fn how(&self) -> &'static str { "touch pad PB1" }

    fn timeout(&self) -> Duration { DEFAULT_TIMEOUT }

    fn confirm(&mut self) -> Decision {
        let Some(pad) = self.pad.as_ref() else {
            return Decision::Unavailable(format!("touch sensor: {}", self.why_not));
        };
        // The untouched level, measured now: the reading is the time the pad
        // takes to charge, which a finger makes longer. A finger already on
        // the pad raises this level, so it has to leave and come back.
        let (mut lo, mut hi) = (u32::MAX, 0u32);
        for _ in 0..Self::IDLE_SAMPLES {
            let r = pad.raw_status();
            lo = lo.min(r);
            hi = hi.max(r);
            std::thread::sleep(Self::POLL);
        }
        if hi == 0 {
            return Decision::Unavailable("touch sensor reads 0".into());
        }
        // On a dabao the bare pad idles at a steady 49 (51 at most) and a
        // fingertip reads 55 to 200, rising while it stays.
        let level = hi.saturating_add((hi / 10).max(5));
        let start = Instant::now();
        let (mut hits, mut peak) = (0, 0u32);
        while start.elapsed() < self.timeout() {
            let r = pad.raw_status();
            peak = peak.max(r);
            if r > level {
                hits += 1;
                if hits >= Self::HITS {
                    return Decision::Approved;
                }
            } else {
                hits = 0;
            }
            std::thread::sleep(Self::POLL);
        }
        Decision::TimedOut(format!("idle {lo}..{hi}, peak {peak}, a touch is above {level}"))
    }

    fn probe(&mut self) -> String {
        let Some(pad) = self.pad.as_ref() else {
            return format!("no sensor: {}", self.why_not);
        };
        let (mut lo, mut hi) = (u32::MAX, 0u32);
        for _ in 0..25 {
            let r = pad.raw_status();
            lo = lo.min(r);
            hi = hi.max(r);
            std::thread::sleep(Self::POLL);
        }
        format!("{lo} {hi}")
    }
}

/// Emulator: the tester creates `$ETH_BURNER_APPROVE` (default
/// `target/eth-burner-approve` in this crate) to approve one request; the file
/// is removed when used. `ETH_BURNER_CONFIRM_MS` changes the timeout.
#[cfg(not(target_os = "xous"))]
pub struct HostFile {
    path: std::path::PathBuf,
    timeout: Duration,
}

#[cfg(not(target_os = "xous"))]
impl HostFile {
    pub fn new() -> Self {
        let path = std::env::var_os("ETH_BURNER_APPROVE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eth-burner-approve"));
        let timeout = std::env::var("ETH_BURNER_CONFIRM_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_TIMEOUT);
        log::info!("emulated approval: create {path:?} within {timeout:?} of a request");
        HostFile { path, timeout }
    }
}

#[cfg(not(target_os = "xous"))]
impl Confirm for HostFile {
    fn how(&self) -> &'static str { "create the approval file" }

    fn timeout(&self) -> Duration { self.timeout }

    fn confirm(&mut self) -> Decision {
        // an approval left over from before this request doesn't count
        std::fs::remove_file(&self.path).ok();
        let start = Instant::now();
        while start.elapsed() < self.timeout {
            if self.path.exists() {
                std::fs::remove_file(&self.path).ok();
                return Decision::Approved;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Decision::TimedOut("no approval file".into())
    }

    fn probe(&mut self) -> String {
        format!("approval file present: {}", self.path.exists())
    }
}
