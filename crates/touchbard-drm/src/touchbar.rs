//! Touch Bar-specific hardware preparation.
//!
//! The Touch Bar display is an Apple USB device (vendor `0x05ac`, product
//! `0x8302`) with two USB configurations: configuration 1 is the firmware
//! function-row strip, and configuration 2 exposes the display interface the
//! DRM driver binds. Most DRM hardware needs nothing like this; this module is
//! the opt-in workaround for the one known exception, selected by the generic
//! [`crate::workaround_for`] lookup.
//!
//! This module owns all Touch Bar knowledge (the VID/PID, the configuration
//! switch) and no DRM knowledge: it never opens a DRM device, picks a card, or
//! touches framebuffer/modeset/rendering.

use std::error::Error;
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::preparer::{DevicePreparer, HardwareId};

/// Apple's USB vendor id.
pub const TOUCHBAR_USB_VENDOR_ID: u16 = 0x05ac;
/// The Touch Bar Display product id.
pub const TOUCHBAR_USB_PRODUCT_ID: u16 = 0x8302;
/// The hardware identity of the Touch Bar Display.
pub const TOUCHBAR_ID: HardwareId = HardwareId {
    vendor_id: TOUCHBAR_USB_VENDOR_ID,
    product_id: TOUCHBAR_USB_PRODUCT_ID,
};
/// The USB configuration whose interface the DRM driver binds. Configuration 1
/// is the firmware function-row strip; only configuration 2 makes the display
/// interface appear so the kernel can bind a driver to it.
pub const TOUCHBAR_DISPLAY_USB_CONFIG: u8 = 2;

/// Default total time to keep waiting for the Touch Bar to be prepared.
const USB_WAIT: Duration = Duration::from_secs(30);
/// Poll interval while waiting for the USB device / its permissions to settle.
const USB_POLL: Duration = Duration::from_millis(250);

/// The USB sysfs operations the Touch Bar preparation needs, behind a small
/// seam so the preparation logic is testable without the physical device.
pub trait UsbAccess {
    /// Find the sysfs directory of the USB device with `id`, if present.
    fn find(&self, id: HardwareId) -> Option<PathBuf>;
    /// The device's current configuration number, `None` when absent or
    /// unconfigured.
    fn config_value(&self, dir: &Path) -> Option<u8>;
    /// Whether the configuration node is writable yet. udev applies the
    /// group/ownership a beat after enumeration, so this can briefly be false
    /// right after a device appears.
    fn config_writable(&self, dir: &Path) -> bool;
    /// Set the active USB configuration to `value`.
    fn set_config(&self, dir: &Path, value: u8) -> Result<(), Box<dyn Error + Send + Sync>>;
}

/// The sysfs-backed [`UsbAccess`], scanning `/sys/bus/usb/devices` for
/// the 05ac:8302 device.
#[derive(Clone)]
pub struct SysfsUsbAccess {
    sysfs_root: PathBuf,
}

impl Default for SysfsUsbAccess {
    fn default() -> Self {
        Self::new("/sys/bus/usb/devices")
    }
}

impl SysfsUsbAccess {
    /// An accessor scanning `sysfs_root` instead of the real sysfs (used by
    /// tests against a scratch tree; production uses [`Default`]).
    pub fn new(sysfs_root: impl Into<PathBuf>) -> Self {
        Self {
            sysfs_root: sysfs_root.into(),
        }
    }

    /// The sysfs device-tree root this accessor scans.
    pub fn sysfs_root(&self) -> &Path {
        &self.sysfs_root
    }
}

impl UsbAccess for SysfsUsbAccess {
    fn find(&self, id: HardwareId) -> Option<PathBuf> {
        let entries = fs::read_dir(&self.sysfs_root).ok()?;
        for entry in entries {
            let entry = entry.ok()?;
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Some(vendor) = read_u16_hex(&dir.join("idVendor")) else {
                continue;
            };
            if vendor != id.vendor_id {
                continue;
            }
            let Some(product) = read_u16_hex(&dir.join("idProduct")) else {
                continue;
            };
            if product == id.product_id {
                return Some(dir);
            }
        }
        None
    }

    fn config_value(&self, dir: &Path) -> Option<u8> {
        let raw = fs::read_to_string(dir.join("bConfigurationValue")).ok()?;
        raw.trim().parse().ok()
    }

    fn config_writable(&self, dir: &Path) -> bool {
        OpenOptions::new()
            .write(true)
            .open(dir.join("bConfigurationValue"))
            .is_ok()
    }

    fn set_config(&self, dir: &Path, value: u8) -> Result<(), Box<dyn Error + Send + Sync>> {
        fs::write(dir.join("bConfigurationValue"), value.to_string())?;
        Ok(())
    }
}

/// Reads a sysfs attribute encoded as a hexadecimal number (e.g. idVendor =
/// `05ac`).
fn read_u16_hex(path: &Path) -> Option<u16> {
    let raw = fs::read_to_string(path).ok()?;
    u16::from_str_radix(raw.trim(), 16).ok()
}

/// Prepares Touch Bar hardware for DRM discovery: finds the 05ac:8302 USB
/// device and ensures it is in its display USB configuration (2), so the
/// display interface appears and a DRM driver can bind.
///
/// This is the known preparation workaround for the one hardware identity that
/// needs it. It only ever does USB plumbing; waiting for the *DRM device* to
/// become visible afterwards is the job of the discovery flow
/// ([`crate::discover_or_prepare`]).
pub struct TouchBarDevicePreparer {
    access: Box<dyn UsbAccess>,
    wait: Duration,
    poll: Duration,
}

impl Default for TouchBarDevicePreparer {
    fn default() -> Self {
        Self::new()
    }
}

impl TouchBarDevicePreparer {
    /// A preparer driving the real sysfs USB device tree.
    pub fn new() -> Self {
        Self {
            access: Box::new(SysfsUsbAccess::default()),
            wait: USB_WAIT,
            poll: USB_POLL,
        }
    }

    /// A preparer driving a custom [`UsbAccess`] with bounded waits/polling
    /// (used by tests; production goes through [`Self::new`]).
    pub fn with_access(access: Box<dyn UsbAccess>, wait: Duration, poll: Duration) -> Self {
        Self { access, wait, poll }
    }
}

impl DevicePreparer for TouchBarDevicePreparer {
    fn prepare(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let deadline = Instant::now() + self.wait;

        // The device may enumerate late (e.g. the USB bus re-enumerates from
        // scratch); wait for it to appear before touching its configuration.
        let mut dir = self.find_usb_device(deadline)?;

        // Ensure the display configuration is active.
        while self.access.config_value(&dir) != Some(TOUCHBAR_DISPLAY_USB_CONFIG) {
            if Instant::now() >= deadline {
                return Err(touch_bar_error(&format!(
                    "did not reach configuration {}",
                    TOUCHBAR_DISPLAY_USB_CONFIG
                )));
            }
            // udev applies the config node's permissions a beat after
            // enumeration; wait for write access before attempting the switch.
            if !self.access.config_writable(&dir) {
                thread::sleep(self.poll);
                continue;
            }
            // Switch through an unconfigured state (a direct 1 -> 2 write does
            // not take effect on the T2 firmware).
            let switched = self.access.set_config(&dir, 0).is_ok()
                && self
                    .access
                    .set_config(&dir, TOUCHBAR_DISPLAY_USB_CONFIG)
                    .is_ok();
            if !switched {
                // A failed configuration write makes the kernel reset the
                // device and the devpath can change; re-resolve it.
                dir = self.find_usb_device(deadline)?;
                thread::sleep(self.poll);
            } else if self.access.config_value(&dir) == Some(TOUCHBAR_DISPLAY_USB_CONFIG) {
                return Ok(());
            }
        }
        Ok(())
    }
}

impl TouchBarDevicePreparer {
    /// Polls `find` until the Touch Bar USB device shows up or `deadline`
    /// passes.
    fn find_usb_device(&self, deadline: Instant) -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
        loop {
            if let Some(dir) = self.access.find(TOUCHBAR_ID) {
                return Ok(dir);
            }
            if Instant::now() >= deadline {
                return Err(touch_bar_error("not found in the USB device tree"));
            }
            thread::sleep(self.poll);
        }
    }
}

fn touch_bar_error(detail: &str) -> Box<dyn Error + Send + Sync> {
    format!(
        "Touch Bar USB device {:04x}:{:04x} {}",
        TOUCHBAR_USB_VENDOR_ID, TOUCHBAR_USB_PRODUCT_ID, detail
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::SystemTime;

    /// Unique scratch dir for one test run; removed (best-effort) afterwards.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "touchbard_drm_test_{}_{}",
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

    /// The Touch Bar identity and display configuration are the *Touch Bar's*
    /// responsibility and live here, not in generic DRM code.
    #[test]
    fn touch_bar_knowledge_is_05ac_8302_config_2() {
        assert_eq!(TOUCHBAR_USB_VENDOR_ID, 0x05ac);
        assert_eq!(TOUCHBAR_USB_PRODUCT_ID, 0x8302);
        assert_eq!(TOUCHBAR_ID.vendor_id, 0x05ac);
        assert_eq!(TOUCHBAR_ID.product_id, 0x8302);
        assert_eq!(TOUCHBAR_DISPLAY_USB_CONFIG, 2);
    }

    fn scratch_touch_bar(scratch: &Path, config: u8) -> PathBuf {
        let dev = scratch.join("1-1");
        fs::create_dir_all(&dev).unwrap();
        write_text(&dev, "idVendor", "05ac\n");
        write_text(&dev, "idProduct", "8302\n");
        write_text(&dev, "bConfigurationValue", &config.to_string());
        dev
    }

    /// Sysfs discovery locates 05ac:8302 and ignores unrelated devices.
    #[test]
    fn sysfs_find_locates_the_touch_bar() {
        let scratch = Scratch::new();
        let dev = scratch_touch_bar(&scratch.0, 1);

        let decoy = scratch.0.join("1-2");
        fs::create_dir_all(&decoy).unwrap();
        write_text(&decoy, "idVendor", "1d6b\n");
        write_text(&decoy, "idProduct", "0002\n");

        let access = SysfsUsbAccess::new(&scratch.0);
        assert_eq!(access.find(TOUCHBAR_ID), Some(dev));
        assert!(access
            .find(HardwareId {
                vendor_id: 0x1d6b,
                product_id: 0x0002,
            })
            .is_some());
        assert!(access
            .find(HardwareId {
                vendor_id: 0x1234,
                product_id: 0x5678,
            })
            .is_none());
    }

    /// A device in the firmware configuration (1) is switched to the display
    /// configuration (2) end to end through the real sysfs accessor.
    #[test]
    fn sysfs_prepare_switches_config_1_to_2() {
        let scratch = Scratch::new();
        let dev = scratch_touch_bar(&scratch.0, 1);

        let access = SysfsUsbAccess::new(&scratch.0);
        let mut preparer = TouchBarDevicePreparer::with_access(
            Box::new(access.clone()),
            Duration::from_secs(2),
            Duration::from_millis(5),
        );
        preparer.prepare().expect("preparation succeeds");

        assert_eq!(access.config_value(&dev), Some(TOUCHBAR_DISPLAY_USB_CONFIG));
    }

    /// A device already in configuration 2 needs no work.
    #[test]
    fn sysfs_prepare_no_op_when_already_in_display_config() {
        let scratch = Scratch::new();
        let dev = scratch_touch_bar(&scratch.0, 2);

        let access = SysfsUsbAccess::new(&scratch.0);
        let mut preparer = TouchBarDevicePreparer::with_access(
            Box::new(access.clone()),
            Duration::from_secs(2),
            Duration::from_millis(5),
        );
        preparer.prepare().expect("already prepared");
        assert_eq!(access.config_value(&dev), Some(2));
    }

    /// A scripted [`UsbAccess`] whose behavior is driven by shared atomics, so
    /// the preparation logic under test stays single-threaded but a helper
    /// thread can "unstick" a wait (late enumeration / udev permission race).
    #[derive(Clone)]
    struct ScriptedUsb {
        present: Arc<AtomicBool>,
        config: Arc<AtomicU8>,
        writable: Arc<AtomicBool>,
        write_failures: Arc<AtomicUsize>,
        reject_display_config: Arc<AtomicBool>,
    }

    impl ScriptedUsb {
        fn new(config: u8, writable: bool) -> Self {
            Self {
                present: Arc::new(AtomicBool::new(true)),
                config: Arc::new(AtomicU8::new(config)),
                writable: Arc::new(AtomicBool::new(writable)),
                write_failures: Arc::new(AtomicUsize::new(0)),
                reject_display_config: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl UsbAccess for ScriptedUsb {
        fn find(&self, _id: HardwareId) -> Option<PathBuf> {
            self.present
                .load(Ordering::SeqCst)
                .then(|| PathBuf::from("/sys/fake/1-1"))
        }

        fn config_value(&self, _dir: &Path) -> Option<u8> {
            Some(self.config.load(Ordering::SeqCst))
        }

        fn config_writable(&self, _dir: &Path) -> bool {
            self.writable.load(Ordering::SeqCst)
        }

        fn set_config(&self, _dir: &Path, value: u8) -> Result<(), Box<dyn Error + Send + Sync>> {
            if value == TOUCHBAR_DISPLAY_USB_CONFIG
                && self.reject_display_config.load(Ordering::SeqCst)
            {
                return Err("display configuration rejected".into());
            }
            if self.write_failures.load(Ordering::SeqCst) > 0 {
                self.write_failures.fetch_sub(1, Ordering::SeqCst);
                return Err("injected write failure".into());
            }
            self.config.store(value, Ordering::SeqCst);
            Ok(())
        }
    }

    fn fast_preparer(access: Box<dyn UsbAccess>) -> TouchBarDevicePreparer {
        TouchBarDevicePreparer::with_access(
            access,
            Duration::from_millis(400),
            Duration::from_millis(10),
        )
    }

    /// The preparation waits for udev's permission race to clear before it
    /// attempts the configuration switch.
    #[test]
    fn waits_for_config_writability_before_switching() {
        let configured = Arc::new(AtomicBool::new(false));
        let usb = ScriptedUsb::new(1, false);
        let writable = Arc::clone(&usb.writable);
        let configured_close = Arc::clone(&configured);
        let switch_thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            writable.store(true, Ordering::SeqCst);
            while !configured_close.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        });

        let mut preparer = fast_preparer(Box::new(usb.clone()));
        preparer.prepare().expect("prep waits for writable config");
        assert_eq!(usb.config.load(Ordering::SeqCst), 2);
        configured.store(true, Ordering::SeqCst);
        switch_thread.join().unwrap();
    }

    /// A failed configuration write is retried (through a re-resolve) until it
    /// lands; the injected failure disturbs only the first `set_config` call.
    #[test]
    fn retries_after_a_failed_config_switch() {
        let usb = ScriptedUsb::new(1, true);
        usb.write_failures.store(1, Ordering::SeqCst);

        let mut preparer = fast_preparer(Box::new(usb.clone()));
        preparer.prepare().expect("retry succeeds");
        assert_eq!(usb.config.load(Ordering::SeqCst), 2);
    }

    /// If the device only shows up late, preparation waits for it instead of
    /// failing immediately.
    #[test]
    fn waits_for_a_late_device() {
        let usb = ScriptedUsb::new(1, true);
        usb.present.store(false, Ordering::SeqCst);
        let present = Arc::clone(&usb.present);
        let appeared = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            present.store(true, Ordering::SeqCst);
        });

        let mut preparer = fast_preparer(Box::new(usb));
        preparer
            .prepare()
            .expect("waits for the device to enumerate");
        appeared.join().unwrap();
    }

    /// A missing device fails within the bounded wait, not forever.
    #[test]
    fn missing_device_fails_within_the_bounded_wait() {
        let usb = ScriptedUsb::new(1, true);
        usb.present.store(false, Ordering::SeqCst);
        let mut preparer = TouchBarDevicePreparer::with_access(
            Box::new(usb),
            Duration::from_millis(30),
            Duration::from_millis(5),
        );
        let err = preparer.prepare().expect_err("missing device must fail");
        assert!(
            err.to_string().contains("not found"),
            "unexpected error: {err}"
        );
    }

    /// A configuration that never sticks fails within the bounded wait.
    #[test]
    fn stuck_configuration_fails_within_the_bounded_wait() {
        let usb = ScriptedUsb::new(1, true);
        usb.reject_display_config.store(true, Ordering::SeqCst);
        let mut preparer = TouchBarDevicePreparer::with_access(
            Box::new(usb),
            Duration::from_millis(30),
            Duration::from_millis(5),
        );
        let err = preparer.prepare().expect_err("stuck config must fail");
        assert!(
            err.to_string().contains("configuration 2"),
            "unexpected error: {err}"
        );
    }
}
