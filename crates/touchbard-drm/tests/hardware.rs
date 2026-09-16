//! Hardware-discovery verification against the physical Touch Bar display
//! (05ac:8302).
//!
//! [`SysfsHardwareDiscovery`] + [`SysfsDrmCardDiscovery`] run against the real
//! sysfs tree via their `Default`/`new` entry points. No workaround or
//! preparation is involved - the display must already be in its DRM
//! configuration for discovery to see it.
//!
//! On machines without the Touch Bar hardware the test skips itself at the
//! FIRST step (hardware not found); it never fabricates a result. Expected
//! topology of the real machine this was written against:
//!
//! ```text
//! /sys/bus/usb/devices/7-6          (05ac:8302)
//!   └── 7-6:2.1
//!        └── drm
//!             └── card2  →  /dev/dri/card2
//! ```

use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;

use touchbard_drm::{
    DrmCardDiscovery, HardwareDiscovery, SysfsDrmCardDiscovery, SysfsHardwareDiscovery, TOUCHBAR_ID,
};

/// Expected device node on the verification machine. Not a production constant:
/// discovery itself never hardcodes a card.
const EXPECTED_DEVICE_NODE: &str = "/dev/dri/card2";
/// Expected sysfs device directory (bus address) of the Touch Bar display.
const EXPECTED_SYSFS_DEVICE: &str = "/sys/bus/usb/devices/7-6";
/// Expected direct child of the device: the display USB interface.
const EXPECTED_INTERFACE: &str = "7-6:2.1";

#[test]
fn touch_bar_hardware_resolves_to_the_expected_drm_node() {
    let mut hardware_discovery = SysfsHardwareDiscovery::default();
    let mut card_discovery = SysfsDrmCardDiscovery::new();

    // Real `HardwareDiscovery` against /sys/bus/usb/devices.
    let hardware = match hardware_discovery.find(TOUCHBAR_ID) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("SKIP: Touch Bar hardware 05ac:8302 not found ({e})");
            return;
        }
    };

    let sysfs_device = hardware.sysfs_path.clone();
    eprintln!(
        "HardwareDiscovery found sysfs device: {}",
        sysfs_device.display()
    );
    assert_eq!(
        sysfs_device,
        Path::new(EXPECTED_SYSFS_DEVICE),
        "unexpected sysfs device path"
    );

    // Verify the documented topology directly: <device>/<interface>/drm/cardN.
    let interface_dir = sysfs_device.join(EXPECTED_INTERFACE);
    let card_dir = interface_dir.join("drm").join("card2");
    assert!(
        card_dir.is_dir(),
        "expected interface/drm/card2 subtree missing at {}",
        card_dir.display()
    );
    eprintln!(
        "Topology verified: {} → {} → drm/card2",
        sysfs_device.display(),
        interface_dir.display()
    );

    // Real `DrmCardDiscovery` on the just-discovered hardware.
    let card = card_discovery
        .discover(&hardware)
        .expect("DrmCardDiscovery must find the card for 05ac:8302");
    eprintln!(
        "DrmCardDiscovery returned device node: {}",
        card.device_path.display()
    );
    assert_eq!(
        card.device_path,
        Path::new(EXPECTED_DEVICE_NODE),
        "discovered DRM node differs from expected"
    );

    // The node must exist and be a DRM character device.
    let meta = fs::metadata(&card.device_path)
        .unwrap_or_else(|e| panic!("DRM node {} missing: {e}", card.device_path.display()));
    assert!(
        meta.file_type().is_char_device(),
        "{} is not a character device",
        card.device_path.display()
    );
    eprintln!(
        "Verified {} exists and is a character device",
        card.device_path.display()
    );
}
