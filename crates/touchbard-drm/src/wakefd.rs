//! Event-driven host wake for the DRM presentation loop.
//!
//! The DRM loop blocks on a single wake source: an `eventfd` that the runtime's
//! waker fires
//! whenever a new frame is wanted, with an optional bounded timeout while the
//! document is animating. There is no fixed frame-rate sleep anywhere in the
//! backend — an idle UI blocks indefinitely and takes no CPU.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::task::{RawWaker, RawWakerVTable, Waker};

/// Fallback animation cadence (≈60 Hz) used when the display mode reports no
/// vertical refresh rate. Otherwise the loop paces to the mode's own refresh
/// interval. Once animation stops, [`WakeFd::wait`] blocks indefinitely again.
pub(crate) const ANIM_TICK: std::time::Duration = std::time::Duration::from_millis(16);

/// An `eventfd` host wake and runtime waker.
///
/// The waker points to the owning `WakeFd`, which the run loop keeps alive for
/// the whole presentation run. Its `clone` and `drop` operations are no-ops.
pub struct WakeFd {
    fd: OwnedFd,
}

/// The owner outlives every waker clone stored by the runtime.
static WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);

unsafe fn clone(data: *const ()) -> RawWaker {
    RawWaker::new(data, &WAKER_VTABLE)
}

unsafe fn wake(data: *const ()) {
    wake_by_ref(data)
}

unsafe fn wake_by_ref(data: *const ()) {
    let wake = unsafe { &*(data.cast::<WakeFd>()) };
    let _ = wake.wake();
}

unsafe fn drop(_data: *const ()) {}

impl WakeFd {
    pub fn new() -> io::Result<Self> {
        let raw = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a valid, owned eventfd descriptor.
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(raw) },
        })
    }

    pub fn waker(&self) -> &'static Waker {
        // SAFETY: `self` outlives any waker handed out (the loop owns it), and
        // the vtable only reads it through the raw pointer stored here.
        Box::leak(Box::new(unsafe {
            Waker::from_raw(RawWaker::new(
                self as *const WakeFd as *const (),
                &WAKER_VTABLE,
            ))
        }))
    }

    /// Block until the wake fires or `timeout` elapses.
    pub fn wait(&self, timeout: Option<std::time::Duration>) -> io::Result<u64> {
        let mut pfd = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms: libc::c_int = timeout.map_or(-1, |t| t.as_millis().min(i32::MAX as u128) as i32);
        let res = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, ms) };
        if res < 0 {
            return Err(io::Error::last_os_error());
        }
        if res == 0 {
            return Ok(0);
        }
        self.reset()
    }

    /// Drain pending wakes and return their count.
    pub fn reset(&self) -> io::Result<u64> {
        let mut count: u64 = 0;
        // SAFETY: `count` is a valid `u64` out-pointer for one read.
        let ret = unsafe { libc::eventfd_read(self.fd.as_raw_fd(), &mut count) };
        if ret < 0 {
            // EAGAIN: nothing pending — not an error.
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock {
                return Ok(0);
            }
            return Err(err);
        }
        Ok(count)
    }

    pub fn wake(&self) -> io::Result<()> {
        // SAFETY: `fd` is a valid eventfd and 1 fits its counter.
        if unsafe { libc::eventfd_write(self.fd.as_raw_fd(), 1) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(crate) fn raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waker_wakes_the_eventfd() {
        let fd = WakeFd::new().unwrap();
        let waker = fd.waker();

        let before = std::time::Instant::now();
        assert_eq!(
            fd.wait(Some(std::time::Duration::from_millis(20))).unwrap(),
            0
        );
        assert!(before.elapsed() >= std::time::Duration::from_millis(15));

        waker.wake_by_ref();
        assert_eq!(fd.wait(None).unwrap(), 1);
    }

    #[test]
    fn wake_and_reset_round_trip() {
        let fd = WakeFd::new().unwrap();
        fd.wake().unwrap();
        fd.wake().unwrap();
        assert!(fd.reset().unwrap() >= 2);
        assert_eq!(fd.reset().unwrap(), 0);
    }
}
