//! Full display path verification against the physical Touch Bar (05ac:8302),
//! driven purely through the existing discovery flow:
//!
//! ```text
//! DrmDevice → DrmResources → selected connector/mode/encoder/CRTC
//!           → DrmDumbBuffer → mmap → test pattern → ADD_FB → SETCRTC
//! ```
//!
//! This test visibly drives the physical Touch Bar. It displays a
//! deterministic pattern (white border, red/green/blue thirds, center stripe)
//! for a short, bounded observation window, then tears the whole pipe down in
//! the correct order, finishing with the CRTC released and every kernel
//! resource destroyed. It never hangs: the observation window is a fixed
//! sleep.
//!
//! No card number, connector, encoder, CRTC, or mode ID is hardcoded; the
//! test acquires DRM master on the discovered node, which `SETCRTC` requires.
//! Machines without the Touch Bar skip at the first discovery step.

use std::thread;
use std::time::Duration;

use drm::Device as DrmDeviceTrait;
use touchbard_drm::{
    disable_crtc, initial_modeset, select_display_config, write_test_pattern, DrmCardDiscovery,
    DrmDevice, DrmDumbBuffer, DrmFramebuffer, DrmResources, HardwareDiscovery,
    SysfsDrmCardDiscovery, SysfsHardwareDiscovery, TOUCHBAR_ID,
};

/// How long the test pattern stays on the panel before teardown.
const OBSERVATION_WINDOW: Duration = Duration::from_secs(3);

#[test]
fn display_the_test_pattern_on_the_touch_bar() {
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
        "selected connector {} mode {} ({}x{} @{} Hz)",
        connector.id(),
        mode.name(),
        mode.width(),
        mode.height(),
        mode.vrefresh(),
    );

    let config = select_display_config(&device, &resources)
        .unwrap_or_else(|e| panic!("selecting display config: {e}"));
    eprintln!(
        "selected connector/encoder/CRTC {}/{}/{} and mode {}x{}",
        u32::from(config.connector()),
        u32::from(config.encoder()),
        u32::from(config.crtc()),
        config.mode().size().0,
        config.mode().size().1,
    );

    // SETCRTC requires DRM master; acquire it on the freshly opened node.
    device
        .acquire_master_lock()
        .unwrap_or_else(|e| panic!("acquiring DRM master on {}: {e}", node.display()));

    // Allocate the CPU buffer, map it, paint the test pattern, then let the
    // mapping unmap itself when the block ends.
    let mut buffer = DrmDumbBuffer::create(&device, mode)
        .unwrap_or_else(|e| panic!("allocating dumb buffer: {e}"));

    {
        let mut mapping = buffer
            .map()
            .unwrap_or_else(|e| panic!("mapping dumb buffer: {e}"));
        eprintln!(
            "mapped {}x{} buffer: pitch={} len={}",
            mapping.width(),
            mapping.height(),
            mapping.pitch(),
            mapping.len(),
        );
        let (mapping_width, mapping_height, mapping_pitch) =
            (mapping.width(), mapping.height(), mapping.pitch());
        write_test_pattern(&mut mapping, mapping_width, mapping_height, mapping_pitch);
    }

    // Bind the buffer to a framebuffer, then modeset.
    let framebuffer = DrmFramebuffer::create(&device, &buffer)
        .unwrap_or_else(|e| panic!("creating framebuffer: {e}"));
    let fb_info = framebuffer
        .info()
        .unwrap_or_else(|e| panic!("querying framebuffer: {e}"));
    eprintln!(
        "created framebuffer {} ({}x{}, pitch {}, bpp {})",
        u32::from(framebuffer.handle()),
        fb_info.size().0,
        fb_info.size().1,
        fb_info.pitch(),
        fb_info.bpp(),
    );

    initial_modeset(&device, framebuffer.handle(), &config)
        .unwrap_or_else(|e| panic!("initial modeset: {e}"));
    eprintln!("SETCRTC complete - the Touch Bar should now show the test pattern");

    // Bounded observation window: keep the pipe alive, then tear it down.
    eprintln!("sleeping {OBSERVATION_WINDOW:?} for visual confirmation...");
    thread::sleep(OBSERVATION_WINDOW);

    // Teardown order: CRTC no longer scanning the FB → FB gone → mapping gone
    // (already unmapped) → dumb buffer gone. Each RAII drop does exactly one
    // kernel call, and the CRTC release happens first.
    disable_crtc(&device, &config)
        .unwrap_or_else(|e| eprintln!("WARN: disabling CRTC failed (ignored): {e}"));
    drop(framebuffer);
    drop(buffer);

    device
        .release_master_lock()
        .unwrap_or_else(|e| eprintln!("WARN: releasing DRM master failed (ignored): {e}"));

    eprintln!("teardown complete; all kernel resources released");
}
