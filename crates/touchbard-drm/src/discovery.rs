//! Hardware discovery, DRM-card discovery, and the flow that drives them.
//!
//! There are two *separate* discovery operations:
//!
//! - [`HardwareDiscovery`] answers "does the requested hardware, identified by
//!   [`HardwareId`], actually exist?"
//! - [`DrmCardDiscovery`] answers "has the kernel exposed a DRM card for that
//!   already-discovered hardware?"
//!
//! [`discover_or_prepare`] runs them in that order. A missing DRM card only
//! ever triggers a workaround when the hardware itself was found and that
//! [`HardwareId`] has a known workaround ([`workaround_for`]). If the hardware
//! is missing, no workaround is attempted and an unavailable error is returned.

use std::error::Error;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use crate::preparer::{workaround_for, DevicePreparer, HardwareId};

/// A DRM card discovered for already-identified hardware.
///
/// Discovery yields only the device node the display is exposed at. The path
/// is always *found* at runtime - never hardcoded (a hardcoded `/dev/dri/cardN`
/// does not exist here). Opening the node, connector discovery, and modesetting
/// are the caller's next operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDrmCard {
    /// Absolute path to the DRM device node (e.g. `/dev/dri/card0`).
    pub device_path: PathBuf,
}

/// Finds the *hardware* of interest by its [`HardwareId`].
///
/// Answers: "does the requested hardware exist?" The returned token
/// proves the hardware is present; what to do with it is up to the caller.
///
/// The sysfs-backed implementation lives in [`crate::sysfs::SysfsHardwareDiscovery`],
/// which scans the USB device tree for the id.
pub trait HardwareDiscovery {
    /// Locate the hardware with `id`, returning a token carrying its sysfs
    /// path.
    fn find(&mut self, id: HardwareId) -> Result<HardwareDevice, Box<dyn Error + Send + Sync>>;
}

/// A token proving the target hardware exists.
///
/// Carries the stable sysfs directory of the hardware: the DRM-card discovery
/// (see [`DrmCardDiscovery`] and
/// [`crate::sysfs::SysfsDrmCardDiscovery`]) walks this subtree to find the
/// card the kernel exposed. A [`HardwareId`] is not itself a DRM card; this
/// device is the connection between the two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareDevice {
    /// The hardware's stable sysfs directory (e.g. `/sys/bus/usb/devices/7-6`),
    /// under which the kernel exposes the DRM card.
    pub sysfs_path: PathBuf,
}

/// Finds the DRM card *for hardware already known to exist*.
///
/// Answers: "has the kernel exposed a DRM card for this hardware?" A failure
/// here means the DRM card is not (yet) exposed - not that the hardware is
/// absent; that is [`HardwareDiscovery`]'s question.
///
/// The sysfs-backed implementation lives in
/// [`crate::sysfs::SysfsDrmCardDiscovery`] and associates the card with the
/// hardware purely through the sysfs device tree - never a hardcoded card and
/// never a driver name.
pub trait DrmCardDiscovery {
    /// Discover the DRM card associated with `hardware`.
    fn discover(
        &mut self,
        hardware: &HardwareDevice,
    ) -> Result<DiscoveredDrmCard, Box<dyn Error + Send + Sync>>;
}

/// How long to keep re-probing for the DRM card after a known workaround
/// prepared the hardware. The workaround (e.g. a USB configuration switch)
/// exposes the display interface asynchronously; discovery needs time to catch
/// it.
const DRM_CARD_WAIT: Duration = Duration::from_secs(30);
/// Poll interval while re-probing for the DRM card after a workaround.
const DRM_CARD_POLL: Duration = Duration::from_millis(250);

/// Run the discovery flow:
///
/// ```text
/// 1. find the target hardware by VID/PID
///      └─ not found      → DRM unavailable (hardware not found); no workaround
///      └─ found          → 2. find the DRM card for that hardware
///                              └─ found    → DONE
///                              └─ not found → 3. known workaround for this
///                                                    HardwareId?
///                                                   ├─ no  → DRM discovery failure
///                                                   └─ yes → prepare, then find the
///                                                            DRM card again
///                                                               ├─ found    → DONE
///                                                               └─ not found → DRM unavailable
/// ```
///
/// A failed DRM-card discovery never *automatically* triggers a workaround:
/// the target hardware must first be discovered, and only a [`HardwareId`]
/// with a known workaround ([`workaround_for`]) gets one. For hardware without
/// a known identity, only plain discovery runs. Unknown [`HardwareId`]s never
/// trigger guessed behavior; the DRM discovery failure is returned.
pub fn discover_or_prepare<H, D>(
    hardware: &mut H,
    drm: &mut D,
    identity: HardwareId,
) -> Result<DiscoveredDrmCard, Box<dyn Error + Send + Sync>>
where
    H: HardwareDiscovery,
    D: DrmCardDiscovery,
{
    discover_or_prepare_with(
        hardware,
        drm,
        identity,
        workaround_for,
        DRM_CARD_WAIT,
        DRM_CARD_POLL,
    )
}

/// [`discover_or_prepare`] with the wait/poll and workaround lookup injected,
/// so the orchestration is testable without DRM hardware and with short waits.
///
/// `pub(crate)` so the real sysfs discovery can be driven end-to-end against a
/// fixture tree in [`crate::sysfs`] tests.
pub(crate) fn discover_or_prepare_with<H, D, R>(
    hardware: &mut H,
    drm: &mut D,
    identity: HardwareId,
    mut resolve: R,
    wait: Duration,
    poll: Duration,
) -> Result<DiscoveredDrmCard, Box<dyn Error + Send + Sync>>
where
    H: HardwareDiscovery,
    D: DrmCardDiscovery,
    R: FnMut(HardwareId) -> Option<Box<dyn DevicePreparer>>,
{
    // Does the requested hardware exist?
    let device = match hardware.find(identity) {
        Ok(device) => device,
        Err(_) => {
            // The workaround must NOT run: without the hardware, there is
            // nothing to prepare. Report the hardware as unavailable.
            let msg = format!(
                "DRM unavailable: target hardware {:04x}:{:04x} not found",
                identity.vendor_id, identity.product_id
            );
            return Err(io_err(msg).into());
        }
    };

    // Does the kernel expose a DRM card for that hardware?
    match drm.discover(&device) {
        Ok(card) => return Ok(card),
        Err(card_err) => {
            // The hardware exists but its DRM card is missing. A workaround is
            // only considered for a *known* HardwareId; otherwise the failure
            // is returned untouched.
            let mut preparer = match resolve(identity) {
                Some(preparer) => preparer,
                None => return Err(card_err),
            };

            // Prepare the hardware, then re-probe for the DRM card until it
            // appears or the bounded wait expires.
            preparer.prepare()?;
            let deadline = Instant::now() + wait;
            loop {
                match drm.discover(&device) {
                    Ok(card) => return Ok(card),
                    Err(_) if Instant::now() < deadline => {
                        thread::sleep(poll);
                    }
                    Err(retry_err) => return Err(retry_err),
                }
            }
        }
    }
}

fn io_err(msg: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A scripted hardware discovery: each `find` pops the next canned result,
    /// and falls back to `tail_error` (if set) once the list is exhausted.
    struct FakeHardware {
        results: Vec<Result<HardwareDevice, String>>,
        tail_error: Option<String>,
        calls: Rc<RefCell<Vec<HardwareId>>>,
    }

    impl FakeHardware {
        fn found() -> Self {
            Self {
                results: vec![Ok(HardwareDevice {
                    sysfs_path: PathBuf::from("/sys/fake/7-6"),
                })],
                tail_error: None,
                calls: Rc::new(RefCell::new(Vec::new())),
            }
        }

        fn missing(msg: &str) -> Self {
            Self {
                results: Vec::new(),
                tail_error: Some(msg.to_string()),
                calls: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl HardwareDiscovery for FakeHardware {
        fn find(&mut self, id: HardwareId) -> Result<HardwareDevice, Box<dyn Error + Send + Sync>> {
            self.calls.borrow_mut().push(id);
            if self.results.is_empty() {
                return Err(io_err(
                    self.tail_error
                        .clone()
                        .unwrap_or_else(|| "unexpected extra find call".to_string()),
                )
                .into());
            }
            self.results
                .drain(..1)
                .next()
                .unwrap()
                .map_err(|msg| io_err(msg).into())
        }
    }

    /// A scripted DRM-card discovery: each `discover` pops the next canned
    /// result, falling back to `tail_error` once the list is exhausted, so
    /// re-probing loops keep failing like real hardware.
    struct FakeDrmCards {
        results: Vec<Result<DiscoveredDrmCard, String>>,
        tail_error: Option<String>,
        calls: Rc<RefCell<usize>>,
    }

    impl FakeDrmCards {
        fn found() -> Self {
            Self {
                results: vec![Ok(DiscoveredDrmCard {
                    device_path: PathBuf::from("/dev/dri/card0"),
                })],
                tail_error: None,
                calls: Rc::new(RefCell::new(0)),
            }
        }

        fn missing(msg: &str) -> Self {
            Self {
                results: Vec::new(),
                tail_error: Some(msg.to_string()),
                calls: Rc::new(RefCell::new(0)),
            }
        }

        fn found_after(n_missing: usize) -> Self {
            let mut results = Vec::new();
            for _ in 0..n_missing {
                results.push(Err("card not exposed yet".to_string()));
            }
            results.push(Ok(DiscoveredDrmCard {
                device_path: PathBuf::from("/dev/dri/card1"),
            }));
            Self {
                results,
                tail_error: None,
                calls: Rc::new(RefCell::new(0)),
            }
        }
    }

    impl DrmCardDiscovery for FakeDrmCards {
        fn discover(
            &mut self,
            _hardware: &HardwareDevice,
        ) -> Result<DiscoveredDrmCard, Box<dyn Error + Send + Sync>> {
            *self.calls.borrow_mut() += 1;
            if self.results.is_empty() {
                return Err(io_err(
                    self.tail_error
                        .clone()
                        .unwrap_or_else(|| "unexpected extra discover call".to_string()),
                )
                .into());
            }
            self.results
                .drain(..1)
                .next()
                .unwrap()
                .map_err(|msg| io_err(msg).into())
        }
    }

    /// A scripted preparer that records its runs in a shared log.
    struct FakePreparer {
        log: Rc<RefCell<Vec<&'static str>>>,
        fail: bool,
    }

    impl DevicePreparer for FakePreparer {
        fn prepare(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
            if self.fail {
                Err(io_err("preparer exploded".to_string()).into())
            } else {
                self.log.borrow_mut().push("prepare");
                Ok(())
            }
        }
    }

    fn resolver(
        log: Rc<RefCell<Vec<&'static str>>>,
        fail: bool,
    ) -> impl Fn(HardwareId) -> Option<Box<dyn DevicePreparer>> {
        move |id| {
            // Model the real workaround table: only the known Touch Bar
            // identity has a preparer; anything else resolves to None.
            if id == crate::touchbar::TOUCHBAR_ID {
                Some(Box::new(FakePreparer {
                    log: Rc::clone(&log),
                    fail,
                }))
            } else {
                None
            }
        }
    }

    fn fast_args() -> (Duration, Duration) {
        (Duration::from_millis(30), Duration::from_millis(1))
    }

    fn assert_never_ran(log: &Rc<RefCell<Vec<&'static str>>>) {
        assert!(
            log.borrow().is_empty(),
            "DevicePreparer must not run: {}",
            log.borrow().join(", ")
        );
    }

    /// Hardware not found: DRM-card discovery is never attempted, the preparer
    /// is never called, and the flow reports DRM unavailable.
    #[test]
    fn hardware_not_found_returns_unavailable_without_any_work() {
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let mut hardware = FakeHardware::missing("no such hardware");
        let mut drm = FakeDrmCards::found();

        let err = discover_or_prepare_with(
            &mut hardware,
            &mut drm,
            crate::touchbar::TOUCHBAR_ID,
            resolver(Rc::clone(&log), false),
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .expect_err("missing hardware must fail");

        assert!(
            err.to_string().contains("hardware"),
            "unexpected error: {err}"
        );
        assert!(
            err.to_string().contains("not found"),
            "must report the hardware was not found: {err}"
        );
        assert_eq!(*drm.calls.borrow(), 0, "DRM-card discovery must not run");
        assert_never_ran(&log);

        // The hardware discovery itself was asked once.
        assert_eq!(hardware.calls.borrow().len(), 1);
    }

    /// Hardware found + DRM card found: the preparer is never called and the
    /// discovered DRM card is returned.
    #[test]
    fn hardware_and_drm_card_found_needs_no_workaround() {
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let mut hardware = FakeHardware::found();
        let mut drm = FakeDrmCards::found();

        let card = discover_or_prepare_with(
            &mut hardware,
            &mut drm,
            crate::touchbar::TOUCHBAR_ID,
            resolver(Rc::clone(&log), false),
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .expect("hardware + card present");
        assert_eq!(card.device_path, PathBuf::from("/dev/dri/card0"));
        assert_eq!(*drm.calls.borrow(), 1);
        assert_never_ran(&log);
    }

    /// Hardware found + DRM card missing + unknown HardwareId: no workaround
    /// (the flow never guesses one) and the DRM discovery failure is returned.
    #[test]
    fn missing_card_with_unknown_identity_returns_drm_failure() {
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let mut hardware = FakeHardware::found();
        let mut drm = FakeDrmCards::missing("no DRM card for this hardware");
        let unknown = HardwareId {
            vendor_id: 0x1234,
            product_id: 0x5678,
        };

        let err = discover_or_prepare_with(
            &mut hardware,
            &mut drm,
            unknown,
            resolver(Rc::clone(&log), false),
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .expect_err("unknown identity must fail");
        assert_eq!(err.to_string(), "no DRM card for this hardware");
        assert_eq!(*drm.calls.borrow(), 1);
        assert_never_ran(&log);
    }

    /// Hardware found + DRM card missing + known HardwareId: the preparer is
    /// called exactly once and DRM-card discovery is retried.
    #[test]
    fn missing_card_with_known_identity_runs_preparer_once_and_retries() {
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let mut hardware = FakeHardware::found();
        let mut drm = FakeDrmCards {
            // 1 initial miss + 1 retry miss + success.
            results: vec![
                Err("card not exposed yet".to_string()),
                Err("card not exposed yet".to_string()),
                Ok(DiscoveredDrmCard {
                    device_path: PathBuf::from("/dev/dri/card3"),
                }),
            ],
            tail_error: None,
            calls: Rc::new(RefCell::new(0)),
        };

        let wait;
        let poll;
        (wait, poll) = fast_args();

        let card = discover_or_prepare_with(
            &mut hardware,
            &mut drm,
            crate::touchbar::TOUCHBAR_ID,
            resolver(Rc::clone(&log), false),
            wait,
            poll,
        )
        .expect("retry must find the card");
        assert_eq!(card.device_path, PathBuf::from("/dev/dri/card3"));
        assert_eq!(
            log.borrow().as_slice(),
            ["prepare"],
            "prepared exactly once"
        );
        assert_eq!(*drm.calls.borrow(), 3, "initial + two retry probes");
    }

    /// Hardware found + DRM card missing + known workaround + retry succeeds on
    /// the first re-probe: return the discovered card.
    #[test]
    fn workaround_then_first_retry_succeeds() {
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let mut hardware = FakeHardware::found();
        let mut drm = FakeDrmCards::found_after(1);

        let card = discover_or_prepare_with(
            &mut hardware,
            &mut drm,
            crate::touchbar::TOUCHBAR_ID,
            resolver(Rc::clone(&log), false),
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .expect("retry must find the card");
        assert_eq!(card.device_path, PathBuf::from("/dev/dri/card1"));
        assert_eq!(log.borrow().as_slice(), ["prepare"]);
        assert_eq!(*drm.calls.borrow(), 2, "initial probe + one retry");
    }

    /// Hardware found + DRM card missing + known workaround + retry always
    /// fails: DRM unavailable (the last discovery failure is returned).
    #[test]
    fn workaround_then_retry_never_succeeds() {
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let mut hardware = FakeHardware::found();
        let mut drm = FakeDrmCards::missing("no card after preparation");

        let wait;
        let poll;
        (wait, poll) = fast_args();

        let err = discover_or_prepare_with(
            &mut hardware,
            &mut drm,
            crate::touchbar::TOUCHBAR_ID,
            resolver(Rc::clone(&log), false),
            wait,
            poll,
        )
        .expect_err("retry must fail");
        assert_eq!(err.to_string(), "no card after preparation");
        assert_eq!(log.borrow().as_slice(), ["prepare"]);
    }

    /// A failing workaround fails the whole flow: the prep failure explains why
    /// the hardware stayed without a DRM card.
    #[test]
    fn preparer_failure_is_returned() {
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let mut hardware = FakeHardware::found();
        let mut drm = FakeDrmCards::missing("card not exposed yet");

        let err = discover_or_prepare_with(
            &mut hardware,
            &mut drm,
            crate::touchbar::TOUCHBAR_ID,
            resolver(Rc::clone(&log), true),
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .expect_err("failed prep must propagate");
        assert_eq!(err.to_string(), "preparer exploded");
        assert_eq!(*drm.calls.borrow(), 1, "only the initial probe ran");
    }
}
