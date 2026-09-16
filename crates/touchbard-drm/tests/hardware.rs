//! Hardware-discovery verification against the physical Touch Bar display
//! (05ac:8302).
//!
//! [`SysfsHardwareDiscovery`] + [`SysfsDrmCardDiscovery`] run against the real
//! sysfs tree via their `Default`/`new` entry points. No workaround or
//! preparation is involved - the display must already be in its DRM
//! configuration for discovery to see it.
//!
//! On machines without the Touch Bar hardware or DRM card the test skips.

use std::fs;
use std::os::unix::fs::FileTypeExt;

use touchbard_drm::{
    DrmCardDiscovery, HardwareDiscovery, SysfsDrmCardDiscovery, SysfsHardwareDiscovery, TOUCHBAR_ID,
};

#[test]
fn touch_bar_hardware_resolves_to_the_expected_drm_node() {
    let mut hardware_discovery = SysfsHardwareDiscovery::default();
    let mut card_discovery = SysfsDrmCardDiscovery::new();

    let hardware = match hardware_discovery.find(TOUCHBAR_ID) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("SKIP: Touch Bar hardware 05ac:8302 not found ({e})");
            return;
        }
    };

    let sysfs_device = hardware.sysfs_path.clone();
    assert!(sysfs_device.is_dir(), "discovered hardware path must exist");

    let card = card_discovery
        .discover(&hardware)
        .expect("DrmCardDiscovery must find the card for 05ac:8302");
    let meta = fs::metadata(&card.device_path)
        .unwrap_or_else(|e| panic!("DRM node {} missing: {e}", card.device_path.display()));
    assert!(
        meta.file_type().is_char_device(),
        "{} is not a character device",
        card.device_path.display()
    );
}
