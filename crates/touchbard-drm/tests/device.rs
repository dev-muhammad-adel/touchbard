//! Open the discovered Touch Bar DRM node through [`DrmDevice`].
//!
//! This test only uses the existing discovery implementation to obtain the
//! node, then hands it to [`DrmDevice::open`] unchanged:
//!
//! ```text
//! 05ac:8302  →  HardwareDiscovery  →  DrmCardDiscovery  →  DrmDevice::open
//! ```
//!
//! No card number is hardcoded; the node is exactly what discovery returned.
//! On machines without the Touch Bar the test skips itself at the discovery
//! step and never fabricates a result.

use touchbard_drm::{
    DrmCardDiscovery, DrmDevice, HardwareDiscovery, SysfsDrmCardDiscovery, SysfsHardwareDiscovery,
    TOUCHBAR_ID,
};

#[test]
fn discover_and_open_the_touch_bar_drm_node() {
    let mut hardware_discovery = SysfsHardwareDiscovery::default();
    let hardware = match hardware_discovery.find(TOUCHBAR_ID) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("SKIP: Touch Bar hardware 05ac:8302 not found ({e})");
            return;
        }
    };

    let mut card_discovery = SysfsDrmCardDiscovery::new();
    let card = match card_discovery.discover(&hardware) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: hardware present but no DRM card exposed ({e})");
            return;
        }
    };

    let node = card.device_path;
    eprintln!("discovered DRM node: {}", node.display());

    let device = DrmDevice::open(&node).unwrap_or_else(|e| panic!("open {}: {e}", node.display()));
    assert!(device.raw_fd() >= 0, "raw fd must be valid");
    eprintln!("opened {} on fd {}", node.display(), device.raw_fd());
}
