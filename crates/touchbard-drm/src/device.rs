//! Minimal generic [`DrmDevice`]: opening and owning a DRM card device node.
//!
//! [`DrmDevice::open`] takes ownership of a `/dev/dri/cardN` node produced by
//! discovery. It issues no DRM ioctls and knows nothing about a specific
//! vendor, driver, or card path - the node is always supplied by the caller.
//!
//! Ownership is RAII through a standard [`File`], so the descriptor is closed
//! automatically when the [`DrmDevice`] is dropped and never leaked. Later DRM
//! operations work off [`DrmDevice::raw_fd`] and the [`AsFd`] implementation.

use std::error::Error;
use std::fs::{File, OpenOptions};
use std::os::raw::c_int;
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd};
use std::path::Path;

/// An owned, opened DRM card device (e.g. `/dev/dri/cardN`).
///
/// Generic by design: it knows nothing about which vendor the card belongs to,
/// how the card was discovered, or any driver name. It exists purely to hold
/// an open descriptor to a real DRM node and hand out the raw fd the next DRM
/// step needs.
///
/// The underlying [`File`] is closed automatically on `drop` (RAII); a
/// [`DrmDevice`] never leaks its descriptor.
#[derive(Debug)]
pub struct DrmDevice {
    file: File,
}

impl DrmDevice {
    /// Open a DRM card device node read/write.
    ///
    /// `path` is the device node (e.g. `/dev/dri/cardN`, taken from discovery -
    /// never guessed). On failure a message naming the node and the underlying
    /// error is returned.
    pub fn open(path: impl AsRef<Path>) -> Result<DrmDevice, Box<dyn Error + Send + Sync>> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| -> Box<dyn Error + Send + Sync> {
                format!("cannot open DRM device {}: {e}", path.display()).into()
            })?;
        Ok(DrmDevice { file })
    }

    /// The raw file descriptor of the opened device, for direct DRM ioctls.
    /// The descriptor stays owned by this [`DrmDevice`].
    pub fn raw_fd(&self) -> c_int {
        self.file.as_raw_fd()
    }
}

/// The `drm` crate drives the kernel through an owned descriptor: implementing
/// [`AsFd`] (delegating to the inner [`File`]) is all it needs to treat this
/// device as a DRM card.
impl AsFd for DrmDevice {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

/// A [`DrmDevice`] is a DRM card: the [`drm::Device`] trait makes the
/// whole safe KMS surface (resource discovery, modesetting) available on it.
/// No extra state lives at the `drm` layer; the [`File`] remains the single
/// owner of the descriptor.
impl drm::Device for DrmDevice {}

/// The kernel-side KMS control surface (resources, connectors, modes). All its
/// methods have default implementations that talk to the card through the
/// descriptor [`AsFd`] borrows, so a [`DrmDevice`] needs no per-call state.
impl drm::control::Device for DrmDevice {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::SystemTime;

    /// Unique scratch dir for one test run; removed (best-effort) afterwards.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "touchbard_drm_device_test_{}_{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Scratch(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Whether a specific fd number is currently open, from /proc/self/fd.
    ///
    /// Linux is the only target this DRM crate supports, so /proc/self/fd is a
    /// reliable view of the process's own descriptor table.
    fn fd_is_open(fd: c_int) -> bool {
        let dir = std::fs::read_dir("/proc/self/fd").expect("read /proc/self/fd");
        dir.flatten()
            .any(|e| e.file_name().to_string_lossy() == fd.to_string())
    }

    /// Opening a non-existent node fails with an error naming the node.
    #[test]
    fn open_missing_path_returns_a_meaningful_error() {
        let scratch = Scratch::new();
        let missing = scratch.0.join("no-such-card-device");

        let err = DrmDevice::open(&missing).expect_err("missing node must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("DRM device"),
            "error must say what failed: {msg}"
        );
        assert!(
            msg.contains("no-such-card-device"),
            "error must name the node: {msg}"
        );
    }

    /// Opening a real character device read/write succeeds and yields a valid
    /// fd. `/dev/null` is a deterministic stand-in for a DRM node: opening
    /// only needs read/write access, it does not probe the node. The real
    /// Touch Bar node is opened in `tests/device.rs` when that hardware is
    /// present.
    #[test]
    fn opens_a_device_node_and_owns_a_valid_fd() {
        let dev = DrmDevice::open("/dev/null").expect("open /dev/null");
        assert!(dev.raw_fd() >= 0, "raw fd must be valid");
    }

    /// RAII: dropping the device closes the descriptor.
    ///
    /// The check reads `/proc/self/fd` (Linux-only, matching the crate's sole
    /// target). Scanning the directory itself claims a new fd - the lowest free
    /// one - which could reuse the just-freed device fd and be mistaken for a
    /// leak. To keep the assertion unambiguous, a set of guard fds is opened
    /// *before* the device so the device's fd sits strictly above them; freeing
    /// the lowest guard afterwards guarantees the scanner can never grab the
    /// device's own number.
    #[test]
    fn drop_closes_the_fd() {
        const GUARDS: usize = 24;
        let mut guards: Vec<File> = (0..GUARDS)
            .map(|_| File::open("/dev/null").expect("open guard fd"))
            .collect();

        let dev = DrmDevice::open("/dev/null").expect("open /dev/null");
        let fd = dev.raw_fd();
        assert!(fd_is_open(fd), "fd {fd} should be open while owned");

        drop(dev);
        // Release the *lowest* guard slot so the scanner always allocates a
        // number below the (now freed) device fd and cannot alias it.
        guards.remove(0);
        assert!(!fd_is_open(fd), "fd {fd} leaked after drop");
    }
}
