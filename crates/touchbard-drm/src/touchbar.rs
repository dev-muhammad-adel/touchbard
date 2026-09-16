//! Touch Bar-specific hardware preparation.
//!
use std::error::Error;
use std::fs;
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::preparer::{DevicePreparer, HardwareId};

pub const TOUCHBAR_USB_VENDOR_ID: u16 = 0x05ac;
pub const TOUCHBAR_USB_PRODUCT_ID: u16 = 0x8302;
pub const TOUCHBAR_ID: HardwareId = HardwareId {
    vendor_id: TOUCHBAR_USB_VENDOR_ID,
    product_id: TOUCHBAR_USB_PRODUCT_ID,
};
/// The USB configuration whose interface the DRM driver binds. Configuration 1
/// is the firmware function-row strip; only configuration 2 makes the display
/// interface appear so the kernel can bind a driver to it.
pub const TOUCHBAR_DISPLAY_USB_CONFIG: u8 = 2;
const USBDEVFS_RESET: libc::c_ulong = 0x5514;

const USB_WAIT: Duration = Duration::from_secs(30);
const USB_POLL: Duration = Duration::from_millis(250);

/// The USB sysfs operations the Touch Bar preparation needs, behind a small
/// seam so the preparation logic is testable without the physical device.
pub trait UsbAccess {
    fn find(&self, id: HardwareId) -> Option<PathBuf>;
    /// The device's current configuration number. An empty sysfs value means
    /// the device is unconfigured; an I/O or parse failure is an error.
    fn config_value(&self, dir: &Path) -> Result<UsbConfiguration, Box<dyn Error + Send + Sync>>;
    /// Whether the configuration node is writable yet. udev applies the
    /// group/ownership a beat after enumeration, so this can briefly be false
    /// right after a device appears.
    fn config_writable(&self, dir: &Path) -> bool;
    fn set_config(&self, dir: &Path, value: u8) -> Result<(), Box<dyn Error + Send + Sync>>;
    fn reset(&self, dir: &Path) -> Result<(), Box<dyn Error + Send + Sync>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbConfiguration {
    Unconfigured,
    Configured(u8),
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
    pub fn new(sysfs_root: impl Into<PathBuf>) -> Self {
        Self {
            sysfs_root: sysfs_root.into(),
        }
    }

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

    fn config_value(&self, dir: &Path) -> Result<UsbConfiguration, Box<dyn Error + Send + Sync>> {
        let raw = fs::read_to_string(dir.join("bConfigurationValue"))?;
        let value = raw.trim();
        if value.is_empty() || value == "0" {
            Ok(UsbConfiguration::Unconfigured)
        } else {
            Ok(UsbConfiguration::Configured(value.parse()?))
        }
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

    fn reset(&self, dir: &Path) -> Result<(), Box<dyn Error + Send + Sync>> {
        let bus = fs::read_to_string(dir.join("busnum"))?
            .trim()
            .parse::<u8>()?;
        let device = fs::read_to_string(dir.join("devnum"))?
            .trim()
            .parse::<u8>()?;
        let path = format!("/dev/bus/usb/{bus:03}/{device:03}");
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let result = unsafe { libc::ioctl(file.as_raw_fd(), USBDEVFS_RESET) };
        if result < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

/// Reads a sysfs attribute encoded as a hexadecimal number (e.g. idVendor =
/// `05ac`).
fn read_u16_hex(path: &Path) -> Option<u16> {
    let raw = fs::read_to_string(path).ok()?;
    u16::from_str_radix(raw.trim(), 16).ok()
}

/// Prepares the Touch Bar USB device for DRM discovery.
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
    pub fn new() -> Self {
        Self {
            access: Box::new(SysfsUsbAccess::default()),
            wait: USB_WAIT,
            poll: USB_POLL,
        }
    }

    pub fn with_access(access: Box<dyn UsbAccess>, wait: Duration, poll: Duration) -> Self {
        Self { access, wait, poll }
    }
}

impl DevicePreparer for TouchBarDevicePreparer {
    fn prepare(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let deadline = Instant::now() + self.wait;

        let mut dir = self.find_usb_device(deadline)?;

        let mut last_write_error = None;
        let mut awaiting_reenumeration = false;
        let mut reset_completed = false;
        loop {
            if Instant::now() >= deadline {
                let detail = last_write_error
                    .map(|e: Box<dyn Error + Send + Sync>| format!("; last write failed: {e}"))
                    .unwrap_or_default();
                return Err(touch_bar_error(&format!(
                    "did not reach configuration {}{}",
                    TOUCHBAR_DISPLAY_USB_CONFIG, detail
                )));
            }
            match self.access.config_value(&dir)? {
                UsbConfiguration::Configured(TOUCHBAR_DISPLAY_USB_CONFIG) => return Ok(()),
                UsbConfiguration::Configured(value) => {
                    reset_completed = false;
                    if awaiting_reenumeration {
                        thread::sleep(self.poll);
                        continue;
                    }
                    eprintln!("Touch Bar USB is in configuration {value}; switching to 2");
                }
                UsbConfiguration::Unconfigured => {
                    if !reset_completed {
                        eprintln!("Touch Bar USB is unconfigured; resetting before re-enumeration");
                        self.access.reset(&dir)?;
                        dir = self.find_usb_device(deadline)?;
                        reset_completed = true;
                        awaiting_reenumeration = false;
                        continue;
                    }
                }
            }
            // udev may make the sysfs control writable after enumeration.
            if !self.access.config_writable(&dir) {
                thread::sleep(self.poll);
                continue;
            }
            if let Err(error) = self.access.set_config(&dir, TOUCHBAR_DISPLAY_USB_CONFIG) {
                last_write_error = Some(error);
                reset_completed = false;
                dir = self.find_usb_device(deadline)?;
                thread::sleep(self.poll);
                continue;
            }
            // A configuration switch can re-enumerate the USB device.
            reset_completed = false;
            awaiting_reenumeration = true;
            dir = self.find_usb_device(deadline)?;
        }
    }

    fn recover_no_card(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let deadline = Instant::now() + self.wait;
        let mut dir = self.find_usb_device(deadline)?;

        if self.access.config_value(&dir)?
            != UsbConfiguration::Configured(TOUCHBAR_DISPLAY_USB_CONFIG)
        {
            return self.prepare();
        }
        if !self.access.config_writable(&dir) {
            while !self.access.config_writable(&dir) {
                if Instant::now() >= deadline {
                    return Err(touch_bar_error(
                        "configuration node did not become writable during no-card recovery",
                    ));
                }
                thread::sleep(self.poll);
            }
        }

        eprintln!("Touch Bar config 2 + no DRM card detected; performing no-card recovery/reprobe");
        self.access.set_config(&dir, 0)?;
        loop {
            dir = self.find_usb_device(deadline)?;
            match self.access.config_value(&dir)? {
                UsbConfiguration::Unconfigured => {
                    self.access.reset(&dir)?;
                    dir = self.find_usb_device(deadline)?;
                    if !self.access.config_writable(&dir) {
                        while !self.access.config_writable(&dir) {
                            if Instant::now() >= deadline {
                                return Err(touch_bar_error(
                                    "configuration node did not become writable after no-card reset",
                                ));
                            }
                            thread::sleep(self.poll);
                        }
                    }
                    self.access.set_config(&dir, TOUCHBAR_DISPLAY_USB_CONFIG)?;
                    dir = self.find_usb_device(deadline)?;
                    return match self.access.config_value(&dir)? {
                        UsbConfiguration::Configured(TOUCHBAR_DISPLAY_USB_CONFIG) => Ok(()),
                        _ => self.prepare(),
                    };
                }
                UsbConfiguration::Configured(TOUCHBAR_DISPLAY_USB_CONFIG) => {
                    if Instant::now() >= deadline {
                        return Err(touch_bar_error(
                            "config 2 did not leave the device during no-card recovery",
                        ));
                    }
                    thread::sleep(self.poll);
                }
                UsbConfiguration::Configured(value) => {
                    return Err(touch_bar_error(&format!(
                        "unexpected configuration {value} during no-card recovery"
                    )));
                }
            }
        }
    }
}

impl TouchBarDevicePreparer {
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

        assert_eq!(
            access.config_value(&dev).unwrap(),
            UsbConfiguration::Configured(TOUCHBAR_DISPLAY_USB_CONFIG)
        );
    }

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
        assert_eq!(
            access.config_value(&dev).unwrap(),
            UsbConfiguration::Configured(2)
        );
    }

    #[derive(Clone)]
    struct ScriptedUsb {
        present: Arc<AtomicBool>,
        config: Arc<AtomicU8>,
        writable: Arc<AtomicBool>,
        write_failures: Arc<AtomicUsize>,
        reject_display_config: Arc<AtomicBool>,
        reset_count: Arc<AtomicUsize>,
        empty_after_set: Arc<AtomicBool>,
        read_error: Arc<AtomicBool>,
    }

    impl ScriptedUsb {
        fn new(config: u8, writable: bool) -> Self {
            Self {
                present: Arc::new(AtomicBool::new(true)),
                config: Arc::new(AtomicU8::new(config)),
                writable: Arc::new(AtomicBool::new(writable)),
                write_failures: Arc::new(AtomicUsize::new(0)),
                reject_display_config: Arc::new(AtomicBool::new(false)),
                reset_count: Arc::new(AtomicUsize::new(0)),
                empty_after_set: Arc::new(AtomicBool::new(false)),
                read_error: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl UsbAccess for ScriptedUsb {
        fn find(&self, _id: HardwareId) -> Option<PathBuf> {
            self.present
                .load(Ordering::SeqCst)
                .then(|| PathBuf::from("/sys/fake/1-1"))
        }

        fn config_value(
            &self,
            _dir: &Path,
        ) -> Result<UsbConfiguration, Box<dyn Error + Send + Sync>> {
            if self.read_error.load(Ordering::SeqCst) {
                return Err("injected configuration read failure".into());
            }
            let value = self.config.load(Ordering::SeqCst);
            Ok(if value == 0 {
                UsbConfiguration::Unconfigured
            } else {
                UsbConfiguration::Configured(value)
            })
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
            if value == TOUCHBAR_DISPLAY_USB_CONFIG
                && self.empty_after_set.swap(false, Ordering::SeqCst)
            {
                self.config.store(0, Ordering::SeqCst);
            } else {
                self.config.store(value, Ordering::SeqCst);
            }
            Ok(())
        }

        fn reset(&self, _dir: &Path) -> Result<(), Box<dyn Error + Send + Sync>> {
            self.reset_count.fetch_add(1, Ordering::SeqCst);
            self.config
                .store(TOUCHBAR_DISPLAY_USB_CONFIG, Ordering::SeqCst);
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

    #[test]
    fn retries_after_a_failed_config_switch() {
        let usb = ScriptedUsb::new(1, true);
        usb.write_failures.store(1, Ordering::SeqCst);

        let mut preparer = fast_preparer(Box::new(usb.clone()));
        preparer.prepare().expect("retry succeeds");
        assert_eq!(usb.config.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn unconfigured_device_is_reset_and_reaches_display_config() {
        let usb = ScriptedUsb::new(0, true);
        let mut preparer = fast_preparer(Box::new(usb.clone()));

        preparer.prepare().expect("reset makes the device ready");
        assert_eq!(usb.reset_count.load(Ordering::SeqCst), 1);
        assert_eq!(usb.config.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_post_switch_verification_resets_and_restarts() {
        let usb = ScriptedUsb::new(1, true);
        usb.empty_after_set.store(true, Ordering::SeqCst);
        let mut preparer = fast_preparer(Box::new(usb.clone()));

        preparer
            .prepare()
            .expect("reset recovers the failed switch");
        assert_eq!(usb.reset_count.load(Ordering::SeqCst), 1);
        assert_eq!(usb.config.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn no_card_recovery_resets_config_2_and_restores_it() {
        let usb = ScriptedUsb::new(2, true);
        let mut preparer = fast_preparer(Box::new(usb.clone()));

        preparer
            .recover_no_card()
            .expect("no-card recovery succeeds");
        assert_eq!(usb.reset_count.load(Ordering::SeqCst), 1);
        assert_eq!(usb.config.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn configuration_read_errors_are_not_treated_as_unconfigured() {
        let usb = ScriptedUsb::new(1, true);
        usb.read_error.store(true, Ordering::SeqCst);
        let mut preparer = fast_preparer(Box::new(usb.clone()));

        let error = preparer.prepare().expect_err("read failure must propagate");
        assert!(error.to_string().contains("configuration read failure"));
        assert_eq!(usb.reset_count.load(Ordering::SeqCst), 0);
    }

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
