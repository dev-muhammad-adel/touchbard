//! Hardware discovery, DRM-card discovery, and the flow that drives them.
//!
//! Hardware and DRM-card discovery are separate so hardware-specific
//! preparation cannot be applied to unrelated devices.

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
    fn discover(
        &mut self,
        hardware: &HardwareDevice,
    ) -> Result<DiscoveredDrmCard, Box<dyn Error + Send + Sync>>;
}

/// Maximum time to wait for a card after hardware preparation.
const DRM_CARD_WAIT: Duration = Duration::from_secs(30);
const DRM_CARD_POLL: Duration = Duration::from_millis(250);
/// Reprobe interval for a prepared device whose card is still absent.
const DRM_CARD_REPROBE: Duration = Duration::from_secs(2);

/// Find the hardware, apply a known preparation workaround, and discover its
/// DRM card. Preparation may re-enumerate the hardware, so the sysfs token is
/// refreshed before card discovery.
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

/// Testable form of [`discover_or_prepare`] with injected timing and lookup.
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
    let device = match hardware.find(identity) {
        Ok(device) => device,
        Err(_) => {
            let msg = format!(
                "DRM unavailable: target hardware {:04x}:{:04x} not found",
                identity.vendor_id, identity.product_id
            );
            return Err(io_err(msg).into());
        }
    };

    if let Some(mut preparer) = resolve(identity) {
        preparer.prepare()?;
        let mut device = hardware.find(identity)?;
        let deadline = Instant::now() + wait;
        let mut next_reprobe = Instant::now();
        loop {
            match drm.discover(&device) {
                Ok(card) => return Ok(card),
                Err(error) if Instant::now() < deadline => {
                    if Instant::now() >= next_reprobe {
                        preparer.recover_no_card()?;
                        device = hardware.find(identity)?;
                        next_reprobe = Instant::now() + DRM_CARD_REPROBE;
                    } else {
                        eprintln!("DRM card discovery still pending; retrying: {error}");
                    }
                    thread::sleep(poll);
                }
                Err(error) => return Err(error),
            }
        }
    } else {
        drm.discover(&device)
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
                if let Some(error) = &self.tail_error {
                    return Err(io_err(error.clone()).into());
                }
                return Ok(HardwareDevice {
                    sysfs_path: PathBuf::from("/sys/fake/7-6"),
                });
            }
            self.results
                .drain(..1)
                .next()
                .unwrap()
                .map_err(|msg| io_err(msg).into())
        }
    }

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

        fn recover_no_card(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
            self.log.borrow_mut().push("recover");
            Ok(())
        }
    }

    fn resolver(
        log: Rc<RefCell<Vec<&'static str>>>,
        fail: bool,
    ) -> impl Fn(HardwareId) -> Option<Box<dyn DevicePreparer>> {
        move |id| {
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

        assert_eq!(hardware.calls.borrow().len(), 1);
    }

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
        assert_eq!(log.borrow().as_slice(), ["prepare"]);
        assert_eq!(hardware.calls.borrow().len(), 2);
    }

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

    #[test]
    fn missing_card_with_known_identity_runs_preparer_once_and_retries() {
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let mut hardware = FakeHardware::found();
        let mut drm = FakeDrmCards {
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
            ["prepare", "recover"],
            "prepared once and recovered once"
        );
        assert_eq!(*drm.calls.borrow(), 3, "initial + two retry probes");
    }

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
        assert_eq!(log.borrow().as_slice(), ["prepare", "recover"]);
        assert_eq!(*drm.calls.borrow(), 2, "initial probe + one retry");
    }

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
        assert_eq!(log.borrow().as_slice(), ["prepare", "recover"]);
    }

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
        assert_eq!(*drm.calls.borrow(), 0, "DRM probing waits for preparation");
    }
}
