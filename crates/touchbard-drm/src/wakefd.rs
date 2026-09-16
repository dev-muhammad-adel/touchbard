//! Event-driven host wake for the DRM presentation loop.
//!
//! The DRM loop blocks on a single wake source: an `eventfd` that the runtime's
//! waker (armed on the Dioxus scheduler and the shell redraw bridge) fires
//! whenever a new frame is wanted, with an optional bounded timeout while the
//! document is animating. There is no fixed frame-rate sleep anywhere in the
//! backend — an idle UI blocks indefinitely and takes no CPU.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::task::{RawWaker, RawWakerVTable, Waker};

/// How long the DRM loop waits while the document is animating (≈60 Hz). Once
/// animation stops, [`WakeFd::wait`] blocks indefinitely again.
pub(crate) const ANIM_TICK: std::time::Duration = std::time::Duration::from_millis(16);

/// True 60 Hz frame period (1/60 s) used by the deadline-based pacing
/// experiment (`TOUCHBARD_DRM_PACING_TEST=1`): the loop waits only until the
/// next absolute deadline instead of always sleeping a full [`ANIM_TICK`], so
/// the produce + DirtyFB transfer cost stays inside the frame budget.
pub(crate) const FRAME_PERIOD: std::time::Duration =
    std::time::Duration::from_nanos(1_000_000_000 / 60);

/// An `eventfd` host wake, plus a `'static` [`Waker`] built from raw pointers
/// so the runtime (which needs a `&'static Waker` for the Dioxus scheduler)
/// can be handed one without any owned state.
///
/// The waker's `data` pointer refers to the owning `WakeFd`, which the run loop
/// keeps alive for the whole presentation run; the vtable's `clone`/`drop` are
/// therefore no-ops (no refcounting is needed) and calling `waker()` more than
/// once per run still yields the same registration.
pub struct WakeFd {
    fd: OwnedFd,
}

/// Vtable functions for the run-loop waker: `data` is always the owning
/// [`WakeFd`], alive for the whole run. `clone`/`drop` are no-ops because the
/// owning value outlives every clone of the waker (including the ones the
/// runtime stores).
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
    /// Create an eventfd in non-blocking (`EFD_NONBLOCK`) mode.
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

    /// The runtime waker for this wake source.
    ///
    /// Returns a fixed `&'static` waker that wakes [`Self`] every time it is
    /// fired. Call it exactly once per run and reuse the result: allocating and
    /// leaking it is deliberate and bounded (the DRM run loop lives for the
    /// process lifetime, and Dioxus' scheduler requires a `&'static` waker to
    /// register against). Every call leaks one tiny box, so calling it in a
    /// loop would grow memory without bound. The vtable is no-op for
    /// `clone`/`drop`, so dropping clones never touches freed state — the
    /// owning `WakeFd` lives on the run-loop frame.
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

    /// Block until the wake fires, or until `timeout` elapses.
    ///
    /// Returns the number of pending wake writes absorbed, or 0 on a timeout.
    /// A `None` timeout blocks indefinitely.
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

    /// Drain the event counter (non-blocking), returning how many pending
    /// wakes were absorbed.
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

    /// Fire the wake: increments the event counter, which [`Wait`](Self::wait)
    /// observes. Called by the runtime integration (shell redraw bridge, Dioxus
    /// scheduler waker).
    pub fn wake(&self) -> io::Result<()> {
        // SAFETY: `fd` is a valid eventfd and 1 fits its counter.
        if unsafe { libc::eventfd_write(self.fd.as_raw_fd(), 1) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waker_wakes_the_eventfd() {
        let fd = WakeFd::new().unwrap();
        let waker = fd.waker();

        // Nothing pending: the wait times out.
        let before = std::time::Instant::now();
        assert_eq!(
            fd.wait(Some(std::time::Duration::from_millis(20))).unwrap(),
            0
        );
        assert!(before.elapsed() >= std::time::Duration::from_millis(15));

        // Firing the runtime waker wakes the loop and is observable.
        waker.wake_by_ref();
        assert_eq!(fd.wait(None).unwrap(), 1);
    }

    #[test]
    fn wake_and_reset_round_trip() {
        let fd = WakeFd::new().unwrap();
        fd.wake().unwrap();
        // Two pending writes coalesce into one counter value, which reset drains.
        fd.wake().unwrap();
        assert!(fd.reset().unwrap() >= 2);
        // Drained: nothing left to read.
        assert_eq!(fd.reset().unwrap(), 0);
    }
}
