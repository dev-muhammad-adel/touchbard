//! Enumerate the KMS resources of the Touch Bar card.
//!
//! Discovers the physical Touch Bar display (05ac:8302), opens its DRM node,
//! and enumerates the card's KMS resources (connectors, modes, encoders,
//! CRTCs) through [`DrmResources`].
//!
//! The card is located entirely through discovery, never by hardcoding a node
//! path or card number. The test prints what the physical card actually
//! exposes, so the report for this step reflects real hardware. Machines
//! without the Touch Bar skip at the first discovery step.
//!
//! ```text
//! 05ac:8302  →  HardwareDiscovery  →  DrmCardDiscovery  →  DrmDevice::open
//!            →  DrmResources::query  (print connector/mode/encoder/CRTC)
//! ```

use touchbard_drm::{
    DrmCardDiscovery, DrmDevice, DrmResources, HardwareDiscovery, SysfsDrmCardDiscovery,
    SysfsHardwareDiscovery, TOUCHBAR_ID,
};

#[test]
fn discover_and_enumerate_the_touch_bar_card_resources() {
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

    let resources = DrmResources::query(&device)
        .unwrap_or_else(|e| panic!("querying resources of {}: {e}", node.display()));

    eprintln!(
        "card exposes {} connector(s), {} encoder(s), {} CRTC(s)",
        resources.connectors().len(),
        resources.encoders().len(),
        resources.crtcs().len(),
    );

    for connector in resources.connectors() {
        eprintln!(
            "  connector id={} interface={}{} status={:?} current_encoder={:?}",
            connector.id(),
            connector.interface(),
            connector.interface_id(),
            connector.connection_status(),
            connector.current_encoder(),
        );
        eprintln!("    possible encoders: {:?}", connector.encoders());
        for mode in connector.modes() {
            eprintln!(
                "    mode {} {}x{} @{} Hz (clock {} kHz)",
                mode.name(),
                mode.width(),
                mode.height(),
                mode.vrefresh(),
                mode.clock(),
            );
        }
    }

    for encoder in resources.encoders() {
        eprintln!("  encoder id={} crtc={:?}", encoder.id(), encoder.crtc());
    }

    for crtc in resources.crtcs() {
        eprintln!("  crtc id={}", crtc.id());
    }

    if let Some(connector) = resources.connected_connector() {
        eprintln!(
            "first connected connector: {} (id {})",
            connector.interface(),
            connector.id(),
        );
    }
}
