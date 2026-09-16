//! Linux sysfs-backed discovery: find USB hardware by id, then find the DRM
//! card the kernel exposed for it.
//!
//! The contract in [`crate::discovery`] keeps the two operations separate:
//!
//! - [`SysfsHardwareDiscovery`] answers "does the hardware with this
//!   [`HardwareId`] exist?" by scanning the USB device tree
//!   (`/sys/bus/usb/devices`) for a directory whose `idVendor`/`idProduct`
//!   match, and returns that stable sysfs path in a [`HardwareDevice`].
//! - [`SysfsDrmCardDiscovery`] answers "which DRM card does this hardware
//!   have?" by walking the already-found hardware's sysfs subtree, purely from
//!   the tree structure - no card number, no by-path symlink, no driver name.
//!
//! Everything here identifies hardware and cards. Nothing here opens a device,
//! runs an ioctl, or touches framebuffer/modeset/rendering, and neither
//! implementation knows anything about a specific vendor: all ids are supplied
//! by the caller.
//!
//! Discovery roots are injectable so tests run against a scratch tree instead
//! of the real sysfs.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use crate::discovery::{DiscoveredDrmCard, DrmCardDiscovery, HardwareDevice, HardwareDiscovery};
use crate::preparer::HardwareId;

/// The sysfs directory that contains one subdirectory per USB device, each
/// named by its bus address (e.g. `7-6`).
const SYSFS_USB_DEVICES: &str = "/sys/bus/usb/devices";

/// The sysfs-backed [`HardwareDiscovery`].
///
/// Scans `usb_devices_root` for a USB device directory whose `idVendor` and
/// `idProduct` match [the target id], returning that directory as the
/// [`HardwareDevice`]. It does no DRM discovery and no USB reconfiguration:
/// it only proves the hardware is present and gives its stable sysfs path.
///
/// [the target id]: crate::preparer::HardwareId
#[derive(Debug, Clone)]
pub struct SysfsHardwareDiscovery {
    usb_devices_root: PathBuf,
}

impl Default for SysfsHardwareDiscovery {
    fn default() -> Self {
        Self::new(SYSFS_USB_DEVICES)
    }
}

impl SysfsHardwareDiscovery {
    /// A discovery scanning `usb_devices_root` instead of the real sysfs
    /// (tests use a scratch tree; production uses [`Default`]).
    pub fn new(usb_devices_root: impl Into<PathBuf>) -> Self {
        Self {
            usb_devices_root: usb_devices_root.into(),
        }
    }

    /// The sysfs directory this discovery scans for USB devices.
    pub fn usb_devices_root(&self) -> &Path {
        &self.usb_devices_root
    }
}

impl HardwareDiscovery for SysfsHardwareDiscovery {
    fn find(&mut self, id: HardwareId) -> Result<HardwareDevice, Box<dyn Error + Send + Sync>> {
        let dir = find_usb_device(&self.usb_devices_root, id).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "no USB device with vendor {:04x} product {:04x} under {}",
                    id.vendor_id,
                    id.product_id,
                    self.usb_devices_root.display()
                ),
            )
        })?;
        Ok(HardwareDevice { sysfs_path: dir })
    }
}

/// The sysfs-backed [`DrmCardDiscovery`] for hardware already found.
///
/// Stateless by design: everything the lookup needs comes from the
/// [`HardwareDevice`]'s sysfs path.
#[derive(Debug, Clone, Copy, Default)]
pub struct SysfsDrmCardDiscovery;

impl SysfsDrmCardDiscovery {
    /// A DRM-card discovery walking the hardware's sysfs subtree.
    pub fn new() -> Self {
        Self
    }
}

impl DrmCardDiscovery for SysfsDrmCardDiscovery {
    fn discover(
        &mut self,
        hardware: &HardwareDevice,
    ) -> Result<DiscoveredDrmCard, Box<dyn Error + Send + Sync>> {
        let card_name = find_card_name(&hardware.sysfs_path).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "no DRM card exposed under {}",
                    hardware.sysfs_path.display()
                ),
            )
        })?;
        Ok(DiscoveredDrmCard {
            device_path: PathBuf::from("/dev/dri").join(card_name),
        })
    }
}

/// Finds the USB device directory under `root` whose ids match `id`.
fn find_usb_device(root: &Path, id: HardwareId) -> Option<PathBuf> {
    let entries = fs::read_dir(root).ok()?;
    for entry in entries {
        let entry = entry.ok()?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        if id_matches(&dir, id) {
            return Some(dir);
        }
    }
    None
}

/// Whether `dir` is a sysfs USB device directory with the given ids.
///
/// A plain subdirectory with no vendor/product attributes (e.g. a USB
/// interface) simply does not match.
fn id_matches(dir: &Path, id: HardwareId) -> bool {
    read_u16_hex(&dir.join("idVendor")) == Some(id.vendor_id)
        && read_u16_hex(&dir.join("idProduct")) == Some(id.product_id)
}

/// Reads a sysfs attribute encoded as a hexadecimal number (e.g. idVendor =
/// `12a4`).
fn read_u16_hex(path: &Path) -> Option<u16> {
    let raw = fs::read_to_string(path).ok()?;
    u16::from_str_radix(raw.trim(), 16).ok()
}

/// Finds the `cardN` directory the kernel exposed for a USB device's display
/// interface, following the sysfs tree (never a hardcoded card, by-path link,
/// or driver name).
///
/// The card lives under a `drm/` subdirectory of a device directory: either
/// directly in the hardware's own directory, or in one of its child interface
/// directories. For e.g. USB hardware the topology is
/// `<device>/<interface>/drm/cardN`: a USB device directory contains its
/// interface directories, an interface contains a `drm` directory, and the
/// `drm` directory contains the card.
///
/// `cardN` itself is a real directory; the `cardN-*` connector symlinks
/// (e.g. `cardN-DP-1`) are not. `entry.file_type()` does not follow symlinks,
/// so only genuine directories named `card<digits>` count.
fn find_card_name(usb_device: &Path) -> Option<String> {
    let mut candidates = vec![usb_device.to_path_buf()];
    if let Ok(children) = fs::read_dir(usb_device) {
        for child in children.flatten() {
            if child.path().is_dir() {
                candidates.push(child.path());
            }
        }
    }

    for dir in candidates {
        let Ok(entries) = fs::read_dir(dir.join("drm")) else {
            continue;
        };
        for entry in entries.flatten() {
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            if is_dir && is_card_name(&entry.file_name().to_string_lossy()) {
                return Some(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Whether `name` looks like a DRM card directory: `card` followed by at least
/// one digit. Connector links like `cardN-DP-1` do not.
fn is_card_name(name: &str) -> bool {
    let Some(digits) = name.strip_prefix("card") else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::time::SystemTime;

    /// Unique scratch dir for one test run; removed (best-effort) afterwards.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "touchbard_drm_sysfs_test_{}_{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0)
            ));
            fs::create_dir_all(&dir).expect("create scratch dir");
            Scratch(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_text(dir: &Path, name: &str, text: &str) {
        fs::write(dir.join(name), text).expect("write sysfs attr");
    }

    /// Builds a USB device directory `name` with the given hex ids.
    fn usb_device(root: &Path, name: &str, vendor: &str, product: &str) -> PathBuf {
        let dev = root.join(name);
        fs::create_dir_all(&dev).unwrap();
        write_text(&dev, "idVendor", &format!("{vendor}\n"));
        write_text(&dev, "idProduct", &format!("{product}\n"));
        dev
    }

    /// Builds a real DRM card directory `cardN` (plus a `cardN-DP-1` connector
    /// symlink, as on the real machine) under `parent/drm/`.
    fn drm_card(parent: &Path, card: &str) {
        let drm = parent.join("drm");
        fs::create_dir_all(&drm).unwrap();
        fs::create_dir(drm.join(card)).unwrap();
        symlink("nonexistent", drm.join(format!("{card}-DP-1"))).unwrap();
    }

    const TOUCH_BAR: HardwareId = HardwareId {
        vendor_id: 0x05ac,
        product_id: 0x8302,
    };
    const UNRELATED: HardwareId = HardwareId {
        vendor_id: 0x1d6b,
        product_id: 0x0002,
    };
    const MISSING: HardwareId = HardwareId {
        vendor_id: 0x1234,
        product_id: 0x5678,
    };

    /// Hardware discovery finds a matching VID/PID and returns the device's
    /// stable sysfs path.
    #[test]
    fn hardware_discovery_finds_a_matching_device() {
        let scratch = Scratch::new();
        let dev = usb_device(&scratch.0, "7-6", "05ac", "8302");

        let mut discovery = SysfsHardwareDiscovery::new(&scratch.0);
        let hardware = discovery.find(TOUCH_BAR).expect("device found");
        assert_eq!(hardware.sysfs_path, dev);
    }

    /// Hardware discovery ignores unrelated USB devices.
    #[test]
    fn hardware_discovery_ignores_unrelated_devices() {
        let scratch = Scratch::new();
        let decoy = usb_device(&scratch.0, "1-2", "1d6b", "0002");
        // A file (not a directory) in the root must be skipped too.
        write_text(&scratch.0, "1-3", "05ac\n");

        let mut discovery = SysfsHardwareDiscovery::new(&scratch.0);
        // The unrelated decoy is found for its own id...
        assert_eq!(discovery.find(UNRELATED).unwrap().sysfs_path, decoy);
        // ...but the bar's id does not match the decoy, so it is not found.
        let err = discovery.find(TOUCH_BAR).expect_err("no matching device");
        assert!(
            err.to_string().contains("no USB device"),
            "unexpected error: {err}"
        );
    }

    /// Hardware discovery reports a clean not-found error for an absent id.
    #[test]
    fn hardware_discovery_reports_missing_hardware() {
        let scratch = Scratch::new();
        usb_device(&scratch.0, "1-2", "1d6b", "0002");

        let mut discovery = SysfsHardwareDiscovery::new(&scratch.0);
        let err = discovery.find(MISSING).expect_err("absent id must fail");
        assert!(
            err.to_string().contains("not found") || err.to_string().contains("no USB device"),
            "unexpected error: {err}"
        );
    }

    /// DRM discovery finds the card under the hardware's subtree and maps it to
    /// `/dev/dri/cardN`.
    #[test]
    fn drm_discovery_finds_the_card_for_the_hardware() {
        let scratch = Scratch::new();
        let dev = usb_device(&scratch.0, "7-6", "05ac", "8302");
        drm_card(&dev.join("7-6:2.1"), "card0");

        let mut discovery = SysfsHardwareDiscovery::new(&scratch.0);
        let hardware = discovery.find(TOUCH_BAR).unwrap();

        let mut cards = SysfsDrmCardDiscovery::new();
        let card = cards.discover(&hardware).expect("card found");
        assert_eq!(card.device_path, std::path::Path::new("/dev/dri/card0"));
    }

    /// DRM discovery never returns a card belonging to another USB device: it
    /// only walks the given hardware's own subtree.
    #[test]
    fn drm_discovery_never_returns_another_devices_card() {
        let scratch = Scratch::new();
        // Decoy device with its own card.
        let decoy = usb_device(&scratch.0, "1-2", "1d6b", "0002");
        drm_card(&decoy.join("1-2:1.0"), "card0");
        // Target hardware with no DRM card at all.
        usb_device(&scratch.0, "7-6", "05ac", "8302");

        let mut discovery = SysfsHardwareDiscovery::new(&scratch.0);
        let hardware = discovery.find(TOUCH_BAR).unwrap();

        let mut cards = SysfsDrmCardDiscovery::new();
        let err = cards
            .discover(&hardware)
            .expect_err("no card for this hardware");
        assert!(
            err.to_string().contains("no DRM card"),
            "must report missing card, not another device's card: {err}"
        );
    }

    /// DRM discovery is immune to the card number: the same hardware with a
    /// different card index still resolves to the right device node.
    #[test]
    fn drm_discovery_works_regardless_of_card_number() {
        let scratch = Scratch::new();
        let dev = usb_device(&scratch.0, "7-6", "05ac", "8302");
        drm_card(&dev.join("7-6:2.1"), "card2");

        let mut discovery = SysfsHardwareDiscovery::new(&scratch.0);
        let hardware = discovery.find(TOUCH_BAR).unwrap();
        let mut cards = SysfsDrmCardDiscovery::new();

        assert_eq!(
            cards.discover(&hardware).unwrap().device_path,
            std::path::Path::new("/dev/dri/card2")
        );

        // Same discovery runs again against a renumbered card (card3 now).
        fs::rename(
            dev.join("7-6:2.1").join("drm").join("card2"),
            dev.join("7-6:2.1").join("drm").join("card3"),
        )
        .unwrap();
        assert_eq!(
            cards.discover(&hardware).unwrap().device_path,
            std::path::Path::new("/dev/dri/card3")
        );
    }

    /// Hardware exists but has no DRM card: DRM discovery fails cleanly.
    #[test]
    fn drm_discovery_fails_cleanly_without_a_card() {
        let scratch = Scratch::new();
        // Hardware present, but nothing under it exposes a `drm` card.
        let dev = usb_device(&scratch.0, "7-6", "05ac", "8302");
        fs::create_dir_all(dev.join("7-6:2.1")).unwrap();
        // The interface has a `drm` dir but only the connector symlink, which is
        // a symlink and not a real card directory.
        let drm = dev.join("7-6:2.1").join("drm");
        fs::create_dir_all(&drm).unwrap();
        symlink("nonexistent", drm.join("card4-DP-1")).unwrap();

        let mut discovery = SysfsHardwareDiscovery::new(&scratch.0);
        let hardware = discovery.find(TOUCH_BAR).unwrap();

        let mut cards = SysfsDrmCardDiscovery::new();
        let err = cards.discover(&hardware).expect_err("no card exposed");
        assert!(
            err.to_string().contains("no DRM card"),
            "unexpected error: {err}"
        );
    }

    /// The production discovery code - everything before the test module - must
    /// not hardcode the Touch Bar's ids, a card number, or a driver name. Only
    /// Touch Bar-specific code in `touchbar` may name them.
    #[test]
    fn production_discovery_code_has_no_hardcoded_ids_or_driver_names() {
        let forbidden = ["05ac", "8302", "card2", "appletbdrm", "t2drm"];
        for (file, source) in [
            ("sysfs.rs", include_str!("sysfs.rs")),
            ("discovery.rs", include_str!("discovery.rs")),
        ] {
            let production = source.split("#[cfg(test)]").next().unwrap_or(source);
            for token in forbidden {
                assert!(
                    !production.contains(token),
                    "{file} production discovery code must not contain `{token}`"
                );
            }
        }
    }

    /// End-to-end: hardware + card present in the fixture tree resolve through
    /// the full discovery orchestration without any workaround.
    #[test]
    fn discover_or_prepare_end_to_end_with_real_sysfs_impls() {
        let scratch = Scratch::new();
        let dev = usb_device(&scratch.0, "7-6", "05ac", "8302");
        usb_device(&scratch.0, "1-2", "1d6b", "0002"); // unrelated decoy
        drm_card(&dev.join("7-6:2.1"), "card1");

        let mut hardware = SysfsHardwareDiscovery::new(&scratch.0);
        let mut cards = SysfsDrmCardDiscovery::new();
        let card = crate::discovery::discover_or_prepare_with(
            &mut hardware,
            &mut cards,
            TOUCH_BAR,
            |_| None,
            std::time::Duration::from_millis(50),
            std::time::Duration::from_millis(1),
        )
        .expect("end-to-end discovery succeeds");
        assert_eq!(card.device_path, std::path::Path::new("/dev/dri/card1"));
    }
}
