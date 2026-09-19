//! Pointer (mouse / trackpad) input reading, hotplug discovery, and reconnect
//! handling.
//!
//! This is the *pointer* sibling of `touchbard-keyboard`: a background reader
//! that opens the `/dev/input/event*` pointer devices (mice, trackpads,
//! pointing sticks) in its own worker thread and publishes a [`PointerEvent`]
//! whenever the pointer moves. Subscribers get an async
//! [`PointerSubscription`], so a Dioxus hook can turn "the pointer is moving"
//! into reactive UI state.
//!
//! The reader only reports *that* motion happened (relative `EV_REL` or
//! absolute `EV_ABS` travel). It carries no delta today -- add fields to
//! [`PointerEvent`] when gestures need distance or direction.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

const DISCOVERY_INTERVAL: Duration = Duration::from_millis(250);
const OPEN_RETRY_DELAY: Duration = Duration::from_secs(3);
const WORKER_TICK: Duration = Duration::from_millis(50);
/// How many evdev events to drain per poll cycle per device, so a fast swipe
/// doesn't fall behind before the next poll.
const MAX_READS_PER_CYCLE: usize = 16;

/// A pointer motion (mouse movement or trackpad finger travel).
///
/// Today the reader only reports *that* motion happened, not how much; add
/// delta fields here when gestures start needing them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerEvent;

/// A subscription to the shared pointer reader: drains queued motions into a
/// local queue with a blocking-compatible async `recv` (waker-driven), so
/// Dioxus `use_future` can await it on the app's runtime thread.
#[derive(Debug, Clone)]
pub struct PointerSubscription {
    state: Arc<Mutex<SubscriptionState>>,
}

impl PointerSubscription {
    /// Take the oldest pending motion, if any.
    pub fn try_recv(&self) -> Option<PointerEvent> {
        self.state.lock().ok()?.events.pop_front()
    }

    /// Await the next motion; `None` when the reader has shut down.
    pub async fn recv(&self) -> Option<PointerEvent> {
        std::future::poll_fn(|context| self.poll_recv(context)).await
    }

    fn poll_recv(&self, context: &mut Context<'_>) -> Poll<Option<PointerEvent>> {
        let Ok(mut state) = self.state.lock() else {
            return Poll::Ready(None);
        };
        if let Some(event) = state.events.pop_front() {
            Poll::Ready(Some(event))
        } else if state.closed {
            Poll::Ready(None)
        } else {
            state.waker = Some(context.waker().clone());
            Poll::Pending
        }
    }
}

#[derive(Debug)]
struct SubscriptionState {
    events: VecDeque<PointerEvent>,
    waker: Option<Waker>,
    closed: bool,
}

/// Shared handle to the pointer reader. [`Pointer::global`] is a process-wide
/// singleton, mirroring `touchbard-keyboard::Keyboard::global`.
#[derive(Clone)]
pub struct Pointer {
    inner: Arc<PointerInner>,
}

struct PointerInner {
    subscribers: Arc<Mutex<Vec<Arc<Mutex<SubscriptionState>>>>>,
    stopped: AtomicBool,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Pointer {
    /// The process-wide pointer reader.
    pub fn global() -> &'static Self {
        static POINTER: OnceLock<Pointer> = OnceLock::new();
        POINTER.get_or_init(Self::new)
    }

    /// Spawn a fresh pointer reader (mostly for tests; prefer [`Self::global`]).
    pub fn new() -> Self {
        let subscribers = Arc::new(Mutex::new(Vec::new()));
        let worker_subscribers = Arc::clone(&subscribers);
        let worker = thread::Builder::new()
            .name("touchbard-pointer".into())
            .spawn(move || run_worker(worker_subscribers))
            .expect("failed to spawn pointer reader");
        Self {
            inner: Arc::new(PointerInner {
                subscribers,
                stopped: AtomicBool::new(false),
                worker: Mutex::new(Some(worker)),
            }),
        }
    }

    /// Subscribe to pointer motion events.
    pub fn subscribe(&self) -> PointerSubscription {
        let state = Arc::new(Mutex::new(SubscriptionState {
            events: VecDeque::new(),
            waker: None,
            closed: false,
        }));
        if let Ok(mut subscribers) = self.inner.subscribers.lock() {
            subscribers.push(Arc::clone(&state));
        }
        PointerSubscription { state }
    }

    /// Ask the worker to stop. Called by tests; the worker also exits on drop.
    pub fn stop(&self) {
        if !self.inner.stopped.swap(true, Ordering::AcqRel) {
            if let Ok(mut worker) = self.inner.worker.lock() {
                if let Some(worker) = worker.take() {
                    let _ = worker.join();
                }
            }
        }
    }
}

impl Default for Pointer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PointerInner {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Ok(worker) = self.worker.get_mut() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
        if let Ok(subscribers) = self.subscribers.lock() {
            for subscriber in subscribers.iter() {
                if let Ok(mut state) = subscriber.lock() {
                    state.closed = true;
                    if let Some(waker) = state.waker.take() {
                        waker.wake();
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReaderState {
    Waiting(Instant),
    Ready,
}

impl ReaderState {
    fn new(now: Instant) -> Self {
        Self::Waiting(now)
    }

    fn disconnected(self, now: Instant) -> Self {
        Self::Waiting(now + OPEN_RETRY_DELAY)
    }

    fn should_reconnect(self, now: Instant) -> bool {
        matches!(self, Self::Waiting(at) if now >= at)
    }
}

struct DeviceState {
    fd: Option<OwnedFd>,
    state: ReaderState,
    /// Last reported value per absolute X/Y axis, so a resting finger on an
    /// absolute multitouch pad (which keeps emitting position updates) does
    /// not count as continuous motion. Indexed by [`AbsAxis`].
    last_abs: [Option<i32>; 4],
    /// Signed net displacement of the tracked X and Y axes since the last
    /// motion was reported. A resting finger dithers around a fixed centroid
    /// (delivering ±few-unit updates at ~250 Hz), which cancels out here,
    /// while deliberate movement accumulates until it crosses the threshold.
    acc_x: i32,
    acc_y: i32,
}

/// Absolute-position displacement (in axis units) required before a finger is
/// considered to be actually moving. Resting dither nets only a few units over
/// a 400 ms idle window; 128 keeps a healthy margin while still tripping
/// quickly for real movement.
const ABS_MOTION_EPS: i32 = 128;

fn run_worker(subscribers: Arc<Mutex<Vec<Arc<Mutex<SubscriptionState>>>>>) {
    let mut devices = HashMap::<PathBuf, DeviceState>::new();
    let mut next_discovery = Instant::now();
    loop {
        let now = Instant::now();
        if now >= next_discovery {
            add_discovered(&mut devices, find_pointer_devices(), now);
            next_discovery = now + DISCOVERY_INTERVAL;
        }

        for (path, device) in devices.iter_mut() {
            if device.fd.is_none() && device.state.should_reconnect(now) {
                match open_pointer(path) {
                    Ok(opened) => {
                        tracing::info!(path = %path.display(), "pointer input reader opened");
                        device.fd = Some(opened);
                        device.state = ReaderState::Ready;
                    }
                    Err(error) => {
                        tracing::warn!(path = %path.display(), error = %error, "pointer input unavailable; retrying");
                        device.state = device.state.disconnected(now);
                    }
                }
            }
        }

        let pollable: Vec<_> = devices
            .iter()
            .filter_map(|(path, device)| {
                device.fd.as_ref().map(|fd| (path.clone(), fd.as_raw_fd()))
            })
            .collect();
        if pollable.is_empty() {
            thread::sleep(WORKER_TICK);
            continue;
        }
        let mut pollfds: Vec<_> = pollable
            .iter()
            .map(|(_, fd)| libc::pollfd {
                fd: *fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let result = unsafe {
            libc::poll(
                pollfds.as_mut_ptr(),
                pollfds.len() as libc::nfds_t,
                WORKER_TICK.as_millis() as libc::c_int,
            )
        };
        if result < 0 {
            for (path, _) in &pollable {
                mark_failed(&mut devices, path);
            }
        } else {
            for ((path, _), pollfd) in pollable.iter().zip(pollfds) {
                if pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    tracing::warn!(path = %path.display(), "pointer reader lost the device; retrying");
                    mark_failed(&mut devices, path);
                } else if pollfd.revents & libc::POLLIN != 0 {
                    read_device(&mut devices, path, &subscribers);
                }
            }
        }
    }
}

fn add_discovered(
    devices: &mut HashMap<PathBuf, DeviceState>,
    paths: impl IntoIterator<Item = PathBuf>,
    now: Instant,
) {
    for path in paths {
        devices.entry(path).or_insert_with(|| DeviceState {
            fd: None,
            state: ReaderState::new(now),
            last_abs: [None; 4],
            acc_x: 0,
            acc_y: 0,
        });
    }
}

fn mark_failed(devices: &mut HashMap<PathBuf, DeviceState>, path: &Path) {
    if let Some(device) = devices.get_mut(path) {
        device.fd = None;
        device.state = device.state.disconnected(Instant::now());
    }
}

fn read_device(
    devices: &mut HashMap<PathBuf, DeviceState>,
    path: &Path,
    subscribers: &Arc<Mutex<Vec<Arc<Mutex<SubscriptionState>>>>>,
) {
    let Some(device) = devices.get_mut(path) else {
        return;
    };
    let Some(fd) = device.fd.as_ref().map(AsRawFd::as_raw_fd) else {
        return;
    };
    for _ in 0..MAX_READS_PER_CYCLE {
        let mut event = InputEvent::default();
        let read = unsafe {
            libc::read(
                fd,
                &mut event as *mut InputEvent as *mut libc::c_void,
                std::mem::size_of::<InputEvent>(),
            )
        };
        if read != std::mem::size_of::<InputEvent>() as isize {
            if read < 0
                && (io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock
                    || io::Error::last_os_error().kind() == io::ErrorKind::Interrupted)
            {
                return;
            }
            tracing::warn!(path = %path.display(), "pointer reader failed to read a complete event; retrying");
            mark_failed(devices, path);
            return;
        }
        // Relative motion counts only when the delta is non-zero. Absolute
        // position updates accumulate signed displacement and only count once
        // the finger has moved far enough to rule out resting dither on an
        // absolute multitouch pad.
        if is_motion(device, event.event_type, event.code, event.value) {
            publish(subscribers);
        }
    }
}

/// The absolute X/Y axes we track, indexed into [`DeviceState::last_abs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbsAxis {
    X,
    Y,
    MtX,
    MtY,
}

impl AbsAxis {
    fn from_code(code: u16) -> Option<Self> {
        match code {
            ABS_X => Some(Self::X),
            ABS_Y => Some(Self::Y),
            ABS_MT_POSITION_X => Some(Self::MtX),
            ABS_MT_POSITION_Y => Some(Self::MtY),
            _ => None,
        }
    }
}

impl From<AbsAxis> for usize {
    fn from(axis: AbsAxis) -> Self {
        match axis {
            AbsAxis::X => 0,
            AbsAxis::Y => 1,
            AbsAxis::MtX => 2,
            AbsAxis::MtY => 3,
        }
    }
}

fn is_motion(device: &mut DeviceState, event_type: u16, code: u16, value: i32) -> bool {
    match event_type {
        EV_REL => (code == REL_X || code == REL_Y) && value != 0,
        EV_ABS => match AbsAxis::from_code(code) {
            Some(axis) => {
                let slot = usize::from(axis);
                let Some(last) = device.last_abs[slot] else {
                    // First report from a contact establishes its origin; the
                    // previous position belongs to an earlier, unrelated touch.
                    device.last_abs[slot] = Some(value);
                    return false;
                };
                if value == last {
                    return false;
                }
                let delta = value - last;
                device.last_abs[slot] = Some(value);
                match axis {
                    AbsAxis::X | AbsAxis::MtX => device.acc_x += delta,
                    AbsAxis::Y | AbsAxis::MtY => device.acc_y += delta,
                }
                if device.acc_x.abs() >= ABS_MOTION_EPS || device.acc_y.abs() >= ABS_MOTION_EPS {
                    device.acc_x = 0;
                    device.acc_y = 0;
                    true
                } else {
                    false
                }
            }
            None => false,
        },
        _ => false,
    }
}

fn publish(subscribers: &Arc<Mutex<Vec<Arc<Mutex<SubscriptionState>>>>>) {
    tracing::trace!("pointer event");
    if let Ok(subscribers) = subscribers.lock() {
        for subscriber in subscribers.iter() {
            if let Ok(mut state) = subscriber.lock() {
                state.events.push_back(PointerEvent);
                if let Some(waker) = state.waker.take() {
                    waker.wake();
                }
            }
        }
    }
}

#[repr(C)]
#[derive(Default)]
struct InputEvent {
    _time: libc::timeval,
    event_type: u16,
    code: u16,
    value: i32,
}

const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;
#[cfg(test)]
const EV_KEY: u16 = 0x01;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;

fn open_pointer(path: &Path) -> io::Result<OwnedFd> {
    let file = fs::OpenOptions::new().read(true).open(path)?;
    Ok(file.into())
}

/// Discover pointer (mouse / trackpad / pointing-stick) devices via sysfs and
/// udev properties, mirroring `touchbard-keyboard`'s discovery. Virtual
/// devices and non-`seat0` seats are skipped.
fn find_pointer_devices() -> Vec<PathBuf> {
    let input = Path::new("/sys/class/input");
    let Ok(entries) = fs::read_dir(input) else {
        return Vec::new();
    };
    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("event") {
            continue;
        }
        let Ok(sysfs) = fs::canonicalize(entry.path().join("device")) else {
            continue;
        };
        if sysfs.to_string_lossy().contains("/devices/virtual/") {
            continue;
        }
        let Some(input_name) = sysfs.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(properties) =
            fs::read_to_string(Path::new("/run/udev/data").join(format!("+input:{input_name}")))
        else {
            continue;
        };
        let is_pointer = [
            "E:ID_INPUT_MOUSE=1",
            "E:ID_INPUT_TOUCHPAD=1",
            "E:ID_INPUT_POINTINGSTICK=1",
        ]
        .iter()
        .any(|marker| properties.lines().any(|line| line == *marker));
        if !is_pointer {
            continue;
        }
        if properties
            .lines()
            .any(|line| line.starts_with("E:ID_SEAT=") && line != "E:ID_SEAT=seat0")
        {
            continue;
        }
        devices.push(PathBuf::from(format!("/dev/input/{name}")));
    }
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_drains_motions_in_order() {
        let subscribers = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(Mutex::new(SubscriptionState {
            events: VecDeque::new(),
            waker: None,
            closed: false,
        }));
        subscribers.lock().unwrap().push(Arc::clone(&state));
        let subscription = PointerSubscription { state };

        publish(&subscribers);
        publish(&subscribers);
        assert_eq!(subscription.try_recv(), Some(PointerEvent));
        assert_eq!(subscription.try_recv(), Some(PointerEvent));
        assert_eq!(subscription.try_recv(), None);
    }

    #[test]
    fn closed_subscription_returns_none() {
        let state = Arc::new(Mutex::new(SubscriptionState {
            events: VecDeque::new(),
            waker: None,
            closed: true,
        }));
        let subscription = PointerSubscription { state };
        assert_eq!(subscription.try_recv(), None);
    }

    #[test]
    fn closed_wakes_pending_recv() {
        let state = Arc::new(Mutex::new(SubscriptionState {
            events: VecDeque::new(),
            waker: None,
            closed: false,
        }));
        let subscription = PointerSubscription { state };
        let waker = noop_waker();
        let mut context = Context::from_waker(&waker);
        assert_eq!(subscription.poll_recv(&mut context), Poll::Pending);
        subscription.state.lock().unwrap().closed = true;
        assert_eq!(subscription.poll_recv(&mut context), Poll::Ready(None));
    }

    fn noop_waker() -> std::task::Waker {
        struct NoopWake;
        impl std::task::Wake for NoopWake {
            fn wake(self: std::sync::Arc<Self>) {}
            fn wake_by_ref(self: &std::sync::Arc<Self>) {}
        }
        std::task::Waker::from(std::sync::Arc::new(NoopWake))
    }

    fn device() -> DeviceState {
        DeviceState {
            fd: None,
            state: ReaderState::Ready,
            last_abs: [None; 4],
            acc_x: 0,
            acc_y: 0,
        }
    }

    #[test]
    fn relative_motion_requires_a_nonzero_delta() {
        let mut device = device();
        assert!(is_motion(&mut device, EV_REL, REL_X, 2));
        assert!(is_motion(&mut device, EV_REL, REL_Y, -3));
        assert!(!is_motion(&mut device, EV_REL, REL_X, 0));
    }

    #[test]
    fn absolute_first_report_establishes_its_origin() {
        let mut device = device();
        assert!(!is_motion(&mut device, EV_ABS, ABS_X, 1000));
        assert!(!is_motion(&mut device, EV_ABS, ABS_X, 1000));
    }

    #[test]
    fn absolute_dithering_is_not_motion() {
        // A resting finger dithers around a fixed centroid; the signed deltas
        // cancel out and never reach the 128-unit threshold.
        let mut device = device();
        let origin = 1000;
        is_motion(&mut device, EV_ABS, ABS_X, origin); // establish origin
        for offset in [1, -1, 2, -1, 1, -2, 1, -1] {
            assert!(
                !is_motion(&mut device, EV_ABS, ABS_X, origin + offset),
                "{offset:+.1} should stay below threshold"
            );
        }
    }

    #[test]
    fn absolute_sustained_movement_accumulates() {
        let mut device = device();
        assert!(!is_motion(&mut device, EV_ABS, ABS_X, 1000)); // origin
        let mut tripped = 0;
        for step in 1..=10i32 {
            let motion = is_motion(&mut device, EV_ABS, ABS_X, 1000 + step * 20);
            if motion {
                tripped += 1;
                assert!(step * 20 >= ABS_MOTION_EPS);
            }
        }
        assert_eq!(tripped, 1);
    }

    #[test]
    fn absolute_axes_cancel_in_signed_drift() {
        // Mirrored ABS_X and ABS_MT_POSITION_X carry the same physical motion;
        // dithering on both still nets ~0, so resting is never motion.
        let mut device = device();
        is_motion(&mut device, EV_ABS, ABS_X, 1000);
        is_motion(&mut device, EV_ABS, ABS_MT_POSITION_X, 1000);
        for (x, mx) in [(1001, 1002), (999, 998), (1001, 1000), (1000, 1001)] {
            assert!(!is_motion(&mut device, EV_ABS, ABS_X, x));
            assert!(!is_motion(&mut device, EV_ABS, ABS_MT_POSITION_X, mx));
        }
        assert!(!is_motion(&mut device, EV_ABS, ABS_MT_POSITION_X, 1002)); // still < EPS
    }

    #[test]
    fn non_position_axes_are_never_motion() {
        let mut device = device();
        assert!(!is_motion(&mut device, EV_ABS, 0x18, 5)); // ABS_PRESSURE
        assert!(!is_motion(&mut device, EV_ABS, 0x47, 2)); // ABS_MT_SLOT
        assert!(!is_motion(&mut device, EV_KEY, 330, 1)); // BTN_TOUCH
        assert!(!is_motion(&mut device, 0x00, 0x00, 1)); // EV_SYN
    }
}