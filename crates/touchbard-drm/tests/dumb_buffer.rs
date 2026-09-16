//! Allocate a dumb buffer sized for the connected Touch Bar mode.
//!
//! Discovers the physical Touch Bar display (05ac:8302), opens its DRM node,
//! queries its resources, selects the connected connector's first mode, and
//! allocates a dumb buffer for that exact mode.
//!
//! Nothing is modeset and nothing is displayed: the test only proves the card
//! accepts a 32bpp CPU buffer sized to the mode. The buffer's RAII drop
//! destroys it as the test stack unwinds.
//!
//! The card is located entirely through discovery, never by hardcoding a node
//! path or card number. Machines without the Touch Bar skip at the first
//! discovery step.
//!
//! ```text
//! 05ac:8302 → HardwareDiscovery → DrmCardDiscovery → DrmDevice::open
//!          → DrmResources::query → connected connector → first mode
//!          → DrmDumbBuffer::create  (validated, then dropped)
//! ```

use touchbard_drm::{
    DrmCardDiscovery, DrmDevice, DrmDumbBuffer, DrmResources, HardwareDiscovery,
    SysfsDrmCardDiscovery, SysfsHardwareDiscovery, TOUCHBAR_ID,
};

#[test]
fn allocate_a_dumb_buffer_for_the_touch_bar_mode() {
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

    let resources = DrmResources::query(&device)
        .unwrap_or_else(|e| panic!("querying resources of {}: {e}", node.display()));

    let connector = resources
        .connected_connector()
        .unwrap_or_else(|| panic!("no connected connector on {node:?}"));
    let mode = connector
        .modes()
        .first()
        .unwrap_or_else(|| panic!("connector {} has no modes", connector.id()));
    eprintln!(
        "connector {} selects mode {} ({}x{} @{} Hz)",
        connector.id(),
        mode.name(),
        mode.width(),
        mode.height(),
        mode.vrefresh(),
    );

    let buffer = DrmDumbBuffer::create(&device, mode).unwrap_or_else(|e| {
        panic!(
            "allocating dumb buffer for {}x{}: {e}",
            mode.width(),
            mode.height()
        )
    });

    assert_eq!(buffer.width(), mode.width(), "width must match the mode");
    assert_eq!(buffer.height(), mode.height(), "height must match the mode");
    assert_eq!(buffer.format(), drm::buffer::DrmFourcc::Abgr8888);
    assert!(buffer.pitch() > 0, "pitch must be positive");
    assert_eq!(
        buffer.size(),
        buffer.pitch() as usize * usize::from(buffer.height())
    );
    assert!(buffer.bpp() > 0, "bpp must be positive");
    eprintln!(
        "allocated dumb buffer: {}x{} pitch={} size={} bpp={} handle={:?}",
        buffer.width(),
        buffer.height(),
        buffer.pitch(),
        buffer.size(),
        buffer.bpp(),
        buffer.handle(),
    );
}
