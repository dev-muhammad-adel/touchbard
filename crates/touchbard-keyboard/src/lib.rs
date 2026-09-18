//! Linux keyboard input with per-device readers, gesture recognition, udev
//! hot-plug tracking, reconnect handling, and a logind suspend/resume
//! lifecycle, delivered to subscribers as a unified [`KeyEvent`] stream.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock, Weak};
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

const RECONNECT_DELAY: Duration = Duration::from_secs(3);
const DOUBLE_PRESS_GAP: Duration = Duration::from_millis(350);
const LONG_PRESS_DURATION: Duration = Duration::from_millis(500);
const WORKER_TICK: Duration = Duration::from_millis(50);
// Confirmation window after an ENOBUFS overflow. It is a short reconciliation
// checkpoint only, never a device-readiness or resume-completion deadline.
const UEVENT_CONFIRM_DELAY: Duration = Duration::from_millis(250);
const UEVENT_BUFFER_SIZE: usize = 8192;
const UEVENT_RECV_BUFFER: usize = 1 << 20;
const NETLINK_KOBJECT_UEVENT: libc::c_int = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Fn,
    Escape,
    Tab,
    Backspace,
    Enter,
    Space,
    Ctrl,
    RCtrl,
    Shift,
    RShift,
    Alt,
    RAlt,
    Meta,
    RMeta,
    CapsLock,
    F(u8),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Mute,
    VolumeDown,
    VolumeUp,
    NextSong,
    PlayPause,
    PreviousSong,
    BrightnessDown,
    BrightnessUp,
    KeyboardIlluminationDown,
    KeyboardIlluminationUp,
    MicMute,
    Letter(char),
    Digit(u8),
    Character(char),
    Unknown(u16),
}

impl Key {
    fn from_code(code: u16) -> Self {
        match code {
            464 => Self::Fn,
            1 => Self::Escape,
            15 => Self::Tab,
            14 => Self::Backspace,
            28 => Self::Enter,
            57 => Self::Space,
            29 => Self::Ctrl,
            97 => Self::RCtrl,
            42 => Self::Shift,
            54 => Self::RShift,
            56 => Self::Alt,
            100 => Self::RAlt,
            125 => Self::Meta,
            126 => Self::RMeta,
            58 => Self::CapsLock,
            59..=68 => Self::F((code - 58) as u8),
            87 => Self::F(11),
            88 => Self::F(12),
            183..=194 => Self::F((code - 170) as u8),
            103 => Self::Up,
            108 => Self::Down,
            105 => Self::Left,
            106 => Self::Right,
            102 => Self::Home,
            107 => Self::End,
            104 => Self::PageUp,
            109 => Self::PageDown,
            110 => Self::Insert,
            111 => Self::Delete,
            113 => Self::Mute,
            114 => Self::VolumeDown,
            115 => Self::VolumeUp,
            163 => Self::NextSong,
            164 => Self::PlayPause,
            165 => Self::PreviousSong,
            224 => Self::BrightnessDown,
            225 => Self::BrightnessUp,
            229 => Self::KeyboardIlluminationDown,
            230 => Self::KeyboardIlluminationUp,
            248 => Self::MicMute,
            2..=10 => Self::Digit((code - 1) as u8),
            11 => Self::Digit(0),
            71 => Self::Digit(7),
            72 => Self::Digit(8),
            73 => Self::Digit(9),
            75 => Self::Digit(4),
            76 => Self::Digit(5),
            77 => Self::Digit(6),
            79 => Self::Digit(1),
            80 => Self::Digit(2),
            81 => Self::Digit(3),
            82 => Self::Digit(0),
            16 => Self::Letter('q'),
            17 => Self::Letter('w'),
            18 => Self::Letter('e'),
            19 => Self::Letter('r'),
            20 => Self::Letter('t'),
            21 => Self::Letter('y'),
            22 => Self::Letter('u'),
            23 => Self::Letter('i'),
            24 => Self::Letter('o'),
            25 => Self::Letter('p'),
            30 => Self::Letter('a'),
            31 => Self::Letter('s'),
            32 => Self::Letter('d'),
            33 => Self::Letter('f'),
            34 => Self::Letter('g'),
            35 => Self::Letter('h'),
            36 => Self::Letter('j'),
            37 => Self::Letter('k'),
            38 => Self::Letter('l'),
            44 => Self::Letter('z'),
            45 => Self::Letter('x'),
            46 => Self::Letter('c'),
            47 => Self::Letter('v'),
            48 => Self::Letter('b'),
            49 => Self::Letter('n'),
            50 => Self::Letter('m'),
            41 => Self::Character('`'),
            12 => Self::Character('-'),
            13 => Self::Character('='),
            26 => Self::Character('['),
            27 => Self::Character(']'),
            43 => Self::Character('\\'),
            39 => Self::Character(';'),
            40 => Self::Character('\''),
            51 => Self::Character(','),
            52 => Self::Character('.'),
            53 => Self::Character('/'),
            83 => Self::Character('.'),
            89 => Self::Character('-'),
            90 => Self::Character('+'),
            91 => Self::Character('*'),
            93 => Self::Character('/'),
            86 => Self::Character('<'),
            96 => Self::Enter,
            code => Self::Unknown(code),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyGesture {
    Press,
    LongPress,
    Release,
    DoublePress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub gesture: KeyGesture,
}

#[derive(Debug, Clone, Copy)]
struct HeldKey {
    key: Key,
    pressed_at: Instant,
    long_fired: bool,
}

/// Per-key gesture recognition for a single keyboard device.
///
/// Gesture semantics, applied independently to every key:
///
/// - [`Press`](KeyGesture::Press) is emitted the moment a key goes down. A
///   second down event for an already-held key is ignored.
/// - [`LongPress`](KeyGesture::LongPress) is emitted exactly once, at the
///   first tick on or after `long_duration` for which the key is still held.
///   It fires while the key is still physically down; it never waits for the
///   key-up.
/// - [`Release`](KeyGesture::Release) is emitted on the physical key-up of a
///   held key. It is never synthesized by a timer tick.
/// - [`DoublePress`](KeyGesture::DoublePress) is emitted on the second release
///   of the same key when the two releases are at most `double_gap` apart, and
///   is always accompanied by the physical
///   [`Release`](KeyGesture::Release) for that key-up. A release after a
///   `LongPress` never produces a `DoublePress`. After a `DoublePress` the
///   key's release history resets, so the next sequence starts fresh.
///
/// Timing uses the monotonic clock. State is fully independent per key and per
/// recognizer, so multiple devices and overlapping holds never interact.
#[derive(Debug)]
pub struct GestureRecognizer {
    double_gap: Duration,
    long_duration: Duration,
    held: HashMap<Key, HeldKey>,
    last_release: HashMap<Key, Instant>,
}

impl Default for GestureRecognizer {
    fn default() -> Self {
        Self::new(DOUBLE_PRESS_GAP, LONG_PRESS_DURATION)
    }
}

impl GestureRecognizer {
    pub fn new(double_gap: Duration, long_duration: Duration) -> Self {
        Self {
            double_gap,
            long_duration,
            held: HashMap::new(),
            last_release: HashMap::new(),
        }
    }

    pub fn input(&mut self, key: Key, value: i32, now: Instant) -> Vec<KeyEvent> {
        match value {
            1 => self.press(key, now),
            0 => self.release(key, now),
            _ => self.advance(now),
        }
    }

    pub fn advance(&mut self, now: Instant) -> Vec<KeyEvent> {
        let mut due: Vec<(Instant, KeyEvent)> = Vec::new();
        for held in self.held.values_mut() {
            if !held.long_fired
                && now.saturating_duration_since(held.pressed_at) >= self.long_duration
            {
                held.long_fired = true;
                due.push((
                    held.pressed_at,
                    KeyEvent {
                        key: held.key,
                        gesture: KeyGesture::LongPress,
                    },
                ));
            }
        }
        due.sort_by_key(|(pressed_at, event)| (*pressed_at, event.key));
        due.into_iter().map(|(_, event)| event).collect()
    }

    pub fn clear(&mut self) {
        self.held.clear();
        self.last_release.clear();
    }

    fn press(&mut self, key: Key, now: Instant) -> Vec<KeyEvent> {
        let mut events = self.advance(now);
        if !self.held.contains_key(&key) {
            self.held.insert(
                key,
                HeldKey {
                    key,
                    pressed_at: now,
                    long_fired: false,
                },
            );
            events.push(KeyEvent {
                key,
                gesture: KeyGesture::Press,
            });
        }
        events
    }

    fn release(&mut self, key: Key, now: Instant) -> Vec<KeyEvent> {
        // A key-up only reports the trackable physical key; an untracked
        // release is ignored.
        let Some(held) = self.held.remove(&key) else {
            return Vec::new();
        };
        let mut events = vec![KeyEvent {
            key,
            gesture: KeyGesture::Release,
        }];
        if held.long_fired {
            // A hold that crossed the long-press threshold never factors into
            // double-press detection: its release is reported but not recorded
            // as a pairing candidate.
            self.last_release.remove(&key);
            return events;
        }
        let double = self
            .last_release
            .get(&key)
            .map(|release_at| now.saturating_duration_since(*release_at) <= self.double_gap)
            .unwrap_or(false);
        if double {
            self.last_release.remove(&key);
            events.push(KeyEvent {
                key,
                gesture: KeyGesture::DoublePress,
            });
        } else {
            self.last_release.insert(key, now);
        }
        events
    }
}

#[derive(Debug, Clone)]
pub struct KeyboardSubscription {
    state: Arc<Mutex<SubscriptionState>>,
}

impl KeyboardSubscription {
    pub fn try_recv(&self) -> Option<KeyEvent> {
        self.state.lock().ok()?.events.pop_front()
    }

    pub async fn recv(&self) -> Option<KeyEvent> {
        std::future::poll_fn(|context| self.poll_recv(context)).await
    }

    fn poll_recv(&self, context: &mut Context<'_>) -> Poll<Option<KeyEvent>> {
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
    events: VecDeque<KeyEvent>,
    waker: Option<Waker>,
    closed: bool,
}

// The registry holds weak references so that a dropped subscription is pruned
// on the next publish or subscribe instead of being retained forever.
type Subscribers = Arc<Mutex<Vec<Weak<Mutex<SubscriptionState>>>>>;

#[derive(Clone)]
pub struct Keyboard {
    inner: Arc<KeyboardInner>,
}

struct KeyboardInner {
    commands: mpsc::Sender<Command>,
    subscribers: Subscribers,
    stopped: AtomicBool,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    lifecycle: Mutex<Option<LifecycleWatcher>>,
}

enum Command {
    Suspend,
    Resume,
    Stop,
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
        Self::Waiting(now + RECONNECT_DELAY)
    }

    fn should_reconnect(self, now: Instant) -> bool {
        matches!(self, Self::Waiting(deadline) if now >= deadline)
    }
}

struct DeviceState {
    fd: Option<OwnedFd>,
    recognizer: GestureRecognizer,
    state: ReaderState,
}

impl Keyboard {
    pub fn global() -> &'static Self {
        static KEYBOARD: OnceLock<Keyboard> = OnceLock::new();
        KEYBOARD.get_or_init(Self::new)
    }

    pub fn new() -> Self {
        let (commands, command_rx) = mpsc::channel();
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let worker_subscribers = Arc::clone(&subscribers);
        let worker = thread::Builder::new()
            .name("touchbard-keyboard".into())
            .spawn(move || run_worker(command_rx, worker_subscribers))
            .expect("failed to spawn keyboard reader");
        let lifecycle = LifecycleWatcher::start(commands.clone());
        Self {
            inner: Arc::new(KeyboardInner {
                commands,
                subscribers,
                stopped: AtomicBool::new(false),
                worker: Mutex::new(Some(worker)),
                lifecycle: Mutex::new(lifecycle),
            }),
        }
    }

    pub fn subscribe(&self) -> KeyboardSubscription {
        let state = Arc::new(Mutex::new(SubscriptionState {
            events: VecDeque::new(),
            waker: None,
            closed: false,
        }));
        if let Ok(mut subscribers) = self.inner.subscribers.lock() {
            subscribers.retain(|slot| slot.strong_count() > 0);
            subscribers.push(Arc::downgrade(&state));
        }
        KeyboardSubscription { state }
    }

    pub fn suspend(&self) {
        if !self.inner.stopped.load(Ordering::Acquire) {
            let _ = self.inner.commands.send(Command::Suspend);
        }
    }

    pub fn resume(&self) {
        if !self.inner.stopped.load(Ordering::Acquire) {
            let _ = self.inner.commands.send(Command::Resume);
        }
    }

    pub fn stop(&self) {
        if !self.inner.stopped.swap(true, Ordering::AcqRel) {
            let _ = self.inner.commands.send(Command::Stop);
        }
    }
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for KeyboardInner {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let _ = self.commands.send(Command::Stop);
        if let Ok(worker) = self.worker.get_mut() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
        if let Ok(subscribers) = self.subscribers.lock() {
            let states: Vec<_> = subscribers
                .iter()
                .filter_map(|slot| slot.upgrade())
                .collect();
            for state in states {
                if let Ok(mut state) = state.lock() {
                    state.closed = true;
                    if let Some(waker) = state.waker.take() {
                        waker.wake();
                    }
                }
            }
        }
        if let Ok(lifecycle) = self.lifecycle.get_mut() {
            if let Some(watcher) = lifecycle.take() {
                watcher.stop();
            }
        }
    }
}

struct UeventSocket {
    fd: OwnedFd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecvError {
    Interrupted,
    Overflow,
    WouldBlock,
    Other,
}

#[derive(Debug)]
struct Uevent {
    action: String,
    subsystem: Option<String>,
    /// Authoritative kernel device path from the `DEVPATH=` field, when the
    /// kernel includes it in the uevent payload. Prefer this over any path
    /// derived from the `@` prefix or from a `DEVNAME=` reconstruction.
    devpath: Option<String>,
}

struct UeventDrain {
    events: Vec<Uevent>,
    overflow: bool,
}

impl UeventSocket {
    fn open() -> io::Result<Self> {
        let sock = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                NETLINK_KOBJECT_UEVENT,
            )
        };
        if sock < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe {
            // `nl_pad` is a private field in libc 0.2.185+, so the whole
            // address is zeroed before filling in the family and groups.
            let mut address: libc::sockaddr_nl = std::mem::zeroed();
            address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
            address.nl_groups = u32::MAX;
            if libc::bind(
                sock,
                &address as *const libc::sockaddr_nl as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            ) < 0
            {
                let error = io::Error::last_os_error();
                libc::close(sock);
                return Err(error);
            }
            // The kernel doubles the requested value, and a second call would
            // only reset the buffer the first call sized. SO_RCVBUFFORCE
            // bypasses net.core.rmem_max but requires CAP_NET_ADMIN; without
            // it (EPERM) fall back to SO_RCVBUF, which the kernel clamps.
            let recv_buffer = UEVENT_RECV_BUFFER as libc::c_int;
            let recv_len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            if libc::setsockopt(
                sock,
                libc::SOL_SOCKET,
                libc::SO_RCVBUFFORCE,
                (&recv_buffer as *const libc::c_int).cast(),
                recv_len,
            ) < 0
            {
                libc::setsockopt(
                    sock,
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    (&recv_buffer as *const libc::c_int).cast(),
                    recv_len,
                );
            }
            let mut actual: libc::c_int = 0;
            let mut actual_len = recv_len;
            libc::getsockopt(
                sock,
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                (&mut actual as *mut libc::c_int).cast(),
                &mut actual_len,
            );
            tracing::debug!(
                requested = recv_buffer,
                actual,
                "keyboard discovery uevent socket receive buffer"
            );
            OwnedFd::from_raw_fd(sock)
        };
        Ok(Self { fd })
    }

    fn drain_input_events(&self) -> UeventDrain {
        let mut buffer = [0u8; UEVENT_BUFFER_SIZE];
        let mut drain = UeventDrain {
            events: Vec::new(),
            overflow: false,
        };
        loop {
            let received = unsafe {
                libc::recv(
                    self.fd.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if received < 0 {
                let error = io::Error::last_os_error();
                match classify_uevent_recv_error(&error) {
                    RecvError::Interrupted => continue,
                    RecvError::Overflow => {
                        // The kernel dropped uevents because the receive buffer
                        // overflowed. The caller must reconcile against
                        // authoritative udev state instead of silently missing
                        // hot-plug transitions.
                        drain.overflow = true;
                        return drain;
                    }
                    RecvError::WouldBlock => return drain,
                    RecvError::Other => {
                        tracing::warn!(error = %error, "keyboard discovery uevent socket read failed");
                        return drain;
                    }
                }
            }
            let received = received as usize;
            if received == 0 {
                return drain;
            }
            if let Some(event) = parse_uevent(&buffer[..received]) {
                if event.subsystem.as_deref() == Some("input") {
                    drain.events.push(event);
                }
            }
        }
    }
}

fn classify_uevent_recv_error(error: &io::Error) -> RecvError {
    match error.raw_os_error() {
        Some(code) if code == libc::EINTR => RecvError::Interrupted,
        Some(code) if code == libc::ENOBUFS => RecvError::Overflow,
        _ if error.kind() == io::ErrorKind::WouldBlock => RecvError::WouldBlock,
        _ => RecvError::Other,
    }
}

fn parse_uevent(buffer: &[u8]) -> Option<Uevent> {
    // NETLINK_KOBJECT_UEVENT datagrams carry no netlink (nlmsghdr) header:
    // the payload begins directly with the kernel uevent prefix word, e.g.
    // `remove@/devices/pci0000:00/...`, NUL-terminated, followed by
    // NUL-separated `KEY=VALUE` fields. Nothing is stripped before parsing.
    let prefix_end = buffer.iter().position(|&byte| byte == 0)?;
    let prefix = std::str::from_utf8(&buffer[..prefix_end]).ok()?;
    // The prefix word is `action@/devices/...`; the action fragment is used
    // only as a fallback since the `ACTION=` field is authoritative.
    let mut action = prefix.split('@').next().unwrap_or(prefix).to_string();
    let mut subsystem = None;
    let mut devpath = None;
    let mut rest = &buffer[prefix_end + 1..];
    while let Some(field_end) = rest.iter().position(|&byte| byte == 0) {
        let field = &rest[..field_end];
        if let Some(equal) = field.iter().position(|&byte| byte == b'=') {
            let value = std::str::from_utf8(&field[equal + 1..]).ok();
            match (&field[..equal], value) {
                (b"ACTION", Some(value)) => action = value.to_string(),
                (b"SUBSYSTEM", Some(value)) => subsystem = Some(value.to_string()),
                (b"DEVPATH", Some(value)) => devpath = Some(value.to_string()),
                _ => {}
            }
        }
        rest = &rest[field_end + 1..];
        if rest.is_empty() {
            break;
        }
    }
    Some(Uevent {
        action,
        subsystem,
        devpath,
    })
}

fn devpath_to_dev_path(devpath: &str) -> Option<PathBuf> {
    // Consumes the authoritative `DEVPATH=` value (e.g.
    // `/devices/.../input/input11/event11`) directly. The device is
    // identified by its terminal `eventN` fragment, which is the same
    // fragment the discovery pass used to key the `devices` map; the map key
    // is therefore derived from DEVPATH directly, never rebuilt from the
    // `action@/devices/...` prefix word.
    devpath
        .split('/')
        .find(|part| part.starts_with("event"))
        .map(|name| PathBuf::from(format!("/dev/input/{name}")))
}

fn remove_uevent_device(devices: &mut HashMap<PathBuf, DeviceState>, devpath: &str) -> bool {
    let Some(path) = devpath_to_dev_path(devpath) else {
        return false;
    };
    if devices.remove(&path).is_some() {
        tracing::info!(path = %path.display(), "keyboard input reader removed");
        true
    } else {
        false
    }
}

struct WorkerState {
    devices: HashMap<PathBuf, DeviceState>,
    suspended: bool,
    confirm_discovery: Option<Instant>,
    discover: Box<dyn Fn() -> Vec<PathBuf>>,
}

impl WorkerState {
    fn new(discover: impl Fn() -> Vec<PathBuf> + 'static) -> Self {
        Self {
            devices: HashMap::new(),
            suspended: false,
            confirm_discovery: None,
            discover: Box::new(discover),
        }
    }

    fn rescan(&mut self, now: Instant) {
        add_discovered(&mut self.devices, (self.discover)(), now);
    }

    fn reconcile(&mut self, now: Instant) {
        let found = (self.discover)();
        self.devices.retain(|path, _| found.contains(path));
        add_discovered(&mut self.devices, found, now);
    }

    fn suspend(&mut self) {
        self.suspended = true;
        self.devices.clear();
    }

    fn resume(&mut self, now: Instant) {
        self.suspended = false;
        self.devices.clear();
        self.confirm_discovery = Some(now);
    }

    fn maybe_confirm(&mut self, now: Instant) {
        if let Some(confirm_at) = self.confirm_discovery {
            if now >= confirm_at {
                self.rescan(now);
                self.confirm_discovery = None;
            }
        }
    }

    fn handle_uevent(&mut self, event: &Uevent, now: Instant) {
        match event.action.as_str() {
            "add" | "move" | "bind" => self.rescan(now),
            "remove" => {
                // DEVPATH= is authoritative and is REQUIRED for removal. It
                // is consumed directly: the value is routed to the
                // `/dev/input/eventN` map key through `devpath_to_dev_path`.
                // DEVPATH is never rebuilt from the `remove@...` prefix word,
                // and DEVNAME= is never used for removal. A remove uevent
                // that omits DEVPATH= is incomplete/invalid and is safely
                // ignored/rejected: no device path is reconstructed from the
                // `remove@...` prefix word or from DEVNAME=.
                if let Some(devpath) = event.devpath.as_deref() {
                    remove_uevent_device(&mut self.devices, devpath);
                }
                self.rescan(now);
            }
            _ => {}
        }
        self.confirm_discovery = Some(now + UEVENT_CONFIRM_DELAY);
    }
}

fn process_uevent_drain(state: &mut WorkerState, drain: UeventDrain, now: Instant) {
    if drain.overflow {
        tracing::warn!(
            "keyboard discovery uevent socket overflowed; reconciling keyboard device state"
        );
        state.reconcile(now);
        state.confirm_discovery = Some(now + UEVENT_CONFIRM_DELAY);
    }
    for event in drain.events {
        state.handle_uevent(&event, now);
    }
}

fn run_worker(commands: mpsc::Receiver<Command>, subscribers: Subscribers) {
    let mut uevent = match UeventSocket::open() {
        Ok(socket) => Some(socket),
        Err(error) => {
            tracing::error!(error = %error, "keyboard discovery uevent socket unavailable; hot-plug detection disabled");
            None
        }
    };
    let mut state = WorkerState::new(find_keyboard_devices);
    state.rescan(Instant::now());
    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Suspend => {
                    tracing::info!("keyboard suspend: releasing input devices and clearing state");
                    state.suspend();
                }
                Command::Resume => {
                    tracing::info!("keyboard resume: rediscovering input devices");
                    state.resume(Instant::now());
                }
                Command::Stop => return,
            }
        }
        if state.suspended {
            thread::sleep(WORKER_TICK);
            continue;
        }

        state.maybe_confirm(Instant::now());

        let now = Instant::now();
        for (path, device) in state.devices.iter_mut() {
            if device.fd.is_none() && device.state.should_reconnect(now) {
                match open_keyboard(path) {
                    Ok(opened) => {
                        tracing::info!(path = %path.display(), "keyboard input reader opened");
                        device.fd = Some(opened);
                        device.state = ReaderState::Ready;
                        device.recognizer.clear();
                    }
                    Err(error) => {
                        tracing::warn!(path = %path.display(), error = %error, "keyboard input unavailable; retrying");
                        device.state = device.state.disconnected(now);
                    }
                }
            }
        }

        let pollable: Vec<_> = state
            .devices
            .iter()
            .filter_map(|(path, device)| {
                device.fd.as_ref().map(|fd| (path.clone(), fd.as_raw_fd()))
            })
            .collect();
        let mut pollfds: Vec<_> = pollable
            .iter()
            .map(|(_, fd)| libc::pollfd {
                fd: *fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let uevent_offset = usize::from(uevent.is_some());
        if uevent.is_some() {
            pollfds.insert(
                0,
                libc::pollfd {
                    fd: uevent.as_ref().unwrap().fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            );
        }
        if pollfds.is_empty() {
            thread::sleep(WORKER_TICK);
            continue;
        }
        let result = unsafe {
            libc::poll(
                pollfds.as_mut_ptr(),
                pollfds.len() as libc::nfds_t,
                WORKER_TICK.as_millis() as libc::c_int,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                tracing::warn!(error = %error, "keyboard poll failed; marking readers failed");
                for (path, _) in &pollable {
                    mark_failed(&mut state.devices, path);
                }
                state.reconcile(Instant::now());
                state.confirm_discovery = Some(Instant::now() + UEVENT_CONFIRM_DELAY);
            }
        } else if result > 0 {
            let now = Instant::now();
            let uevent_revents = if uevent.is_some() {
                pollfds[0].revents
            } else {
                0
            };
            if uevent_revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
            {
                if let Some(socket) = uevent.as_ref() {
                    let drain = socket.drain_input_events();
                    process_uevent_drain(&mut state, drain, now);
                }
            }
            if uevent_revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                tracing::warn!("keyboard discovery uevent socket failed; reopening");
                uevent = None;
                match UeventSocket::open() {
                    Ok(socket) => uevent = Some(socket),
                    Err(error) => {
                        tracing::error!(error = %error, "keyboard discovery uevent socket unavailable; hot-plug detection disabled")
                    }
                }
                state.reconcile(now);
                state.confirm_discovery = Some(now + UEVENT_CONFIRM_DELAY);
            }
            for ((path, _), pollfd) in pollable.iter().zip(pollfds.iter().skip(uevent_offset)) {
                if pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    tracing::warn!(path = %path.display(), "keyboard input reader lost the device; retrying");
                    mark_failed(&mut state.devices, path);
                } else if pollfd.revents & libc::POLLIN != 0 {
                    read_device(&mut state.devices, path, &subscribers);
                }
            }
        }
        let now = Instant::now();
        for device in state.devices.values_mut() {
            publish(&subscribers, device.recognizer.advance(now));
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
            recognizer: GestureRecognizer::default(),
            state: ReaderState::new(now),
        });
    }
}

fn mark_failed(devices: &mut HashMap<PathBuf, DeviceState>, path: &Path) {
    if let Some(device) = devices.get_mut(path) {
        device.fd = None;
        device.state = device.state.disconnected(Instant::now());
        device.recognizer.clear();
    }
}

fn read_device(
    devices: &mut HashMap<PathBuf, DeviceState>,
    path: &Path,
    subscribers: &Subscribers,
) {
    let Some(device) = devices.get_mut(path) else {
        return;
    };
    let Some(fd) = device.fd.as_ref().map(AsRawFd::as_raw_fd) else {
        return;
    };
    let mut event = InputEvent::default();
    let read = unsafe {
        libc::read(
            fd,
            &mut event as *mut InputEvent as *mut libc::c_void,
            std::mem::size_of::<InputEvent>(),
        )
    };
    if read < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            return;
        }
        tracing::warn!(path = %path.display(), error = %error, "failed to read a keyboard input event; retrying");
        mark_failed(devices, path);
    } else if read != std::mem::size_of::<InputEvent>() as isize {
        tracing::warn!(path = %path.display(), "failed to read a complete keyboard input event; retrying");
        mark_failed(devices, path);
    } else if event.event_type == EV_KEY {
        publish(
            subscribers,
            device
                .recognizer
                .input(Key::from_code(event.code), event.value, Instant::now()),
        );
    }
}

fn publish(subscribers: &Subscribers, events: Vec<KeyEvent>) {
    if events.is_empty() {
        return;
    }
    for event in &events {
        tracing::info!(key = ?event.key, gesture = ?event.gesture, "keyboard event");
    }
    let states: Vec<_> = subscribers
        .lock()
        .map(|subscribers| subscribers.iter().filter_map(Weak::upgrade).collect())
        .unwrap_or_default();
    for state in &states {
        if let Ok(mut state) = state.lock() {
            // Latest-wins: keep only the newest event. A consumer on the UI
            // thread polls until `recv()` returns Pending; an unbounded backlog
            // would keep the Dioxus scheduler's `render_immediate` loop hot
            // (each `recv` is Ready, each signal write re-queues work), so a
            // producer flood could starve presentation. Coalescing to one event
            // guarantees the consumer drains to Pending after every wake. The
            // stream is a key-state feed, so older events are stale by design.
            state.events.clear();
            state.events.push_back(*events.last().unwrap());
            if let Some(waker) = state.waker.take() {
                waker.wake();
            }
        }
    }
    if let Ok(mut subscribers) = subscribers.lock() {
        subscribers.retain(|slot| slot.strong_count() > 0);
    }
}

// Mirrors the kernel's `struct input_event` layout so one read on an evdev
// node yields exactly one event.
#[repr(C)]
#[derive(Default)]
struct InputEvent {
    _time: libc::timeval,
    event_type: u16,
    code: u16,
    value: i32,
}

const EV_KEY: u16 = 0x01;

fn open_keyboard(path: &Path) -> io::Result<OwnedFd> {
    let file = fs::OpenOptions::new().read(true).open(path)?;
    // Best-effort non-blocking so a wedged device can never stall the worker;
    // evdev reads deliver whole events, so an early EAGAIN is simply skipped.
    unsafe {
        libc::fcntl(file.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
    }
    Ok(file.into())
}

fn find_keyboard_devices() -> Vec<PathBuf> {
    let input = Path::new("/sys/class/input");
    let mut devices = Vec::new();
    let Ok(entries) = fs::read_dir(input) else {
        return Vec::new();
    };
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
        if !properties
            .lines()
            .any(|line| line == "E:ID_INPUT_KEYBOARD=1")
            || properties
                .lines()
                .any(|line| line.starts_with("E:ID_SEAT=") && line != "E:ID_SEAT=seat0")
        {
            continue;
        }
        devices.push(PathBuf::from(format!("/dev/input/{name}")));
    }
    devices
}

struct LifecycleWatcher {
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<std::process::Child>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl LifecycleWatcher {
    fn start(commands: mpsc::Sender<Command>) -> Option<Self> {
        let mut child = std::process::Command::new("dbus-monitor")
            .args([
                "--system",
                "type='signal',interface='org.freedesktop.login1.Manager',member='PrepareForSleep'",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let stop = Arc::new(AtomicBool::new(false));
        let child_slot = Arc::new(Mutex::new(Some(child)));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("touchbard-keyboard-logind".into())
            .spawn(move || {
                let mut signal = false;
                for line in std::io::BufRead::lines(std::io::BufReader::new(stdout)).flatten() {
                    if thread_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if line.contains("member=PrepareForSleep") {
                        signal = true;
                    } else if signal && line.trim() == "boolean true" {
                        tracing::info!("logind requested suspend for keyboard");
                        let _ = commands.send(Command::Suspend);
                        signal = false;
                    } else if signal && line.trim() == "boolean false" {
                        tracing::info!("logind reported resume for keyboard");
                        let _ = commands.send(Command::Resume);
                        signal = false;
                    }
                }
            })
            .ok()?;
        Some(Self {
            stop,
            child: child_slot,
            thread: Some(thread),
        })
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(mut child) = self.child.lock() {
            if let Some(child) = child.as_mut() {
                let _ = child.kill();
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Ok(mut child) = self.child.lock() {
            if let Some(mut child) = child.take() {
                let _ = child.wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf as Pb;

    /// Deterministic monotonic clock: offsets from a fixed instant.
    fn at_ms(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    fn base() -> Instant {
        Instant::now()
    }

    fn press(rec: &mut GestureRecognizer, key: Key, start: Instant, at: u64) -> Vec<KeyEvent> {
        rec.input(key, 1, at_ms(start, at))
    }

    fn release(rec: &mut GestureRecognizer, key: Key, start: Instant, at: u64) -> Vec<KeyEvent> {
        rec.input(key, 0, at_ms(start, at))
    }

    fn assert_press(events: &[KeyEvent], key: Key) {
        assert_eq!(
            events,
            &[KeyEvent {
                key,
                gesture: KeyGesture::Press,
            }]
        );
    }

    fn assert_long(events: &[KeyEvent], key: Key) {
        assert_eq!(
            events,
            &[KeyEvent {
                key,
                gesture: KeyGesture::LongPress,
            }]
        );
    }

    fn assert_release(events: &[KeyEvent], key: Key) {
        assert_eq!(
            events,
            &[KeyEvent {
                key,
                gesture: KeyGesture::Release,
            }]
        );
    }

    // ---- Key mapping ---------------------------------------------------------

    #[test]
    fn letters_digits_and_familiar_punctuation_map_from_codes() {
        assert_eq!(Key::from_code(30), Key::Letter('a'));
        assert_eq!(Key::from_code(16), Key::Letter('q'));
        assert_eq!(Key::from_code(44), Key::Letter('z'));
        assert_eq!(Key::from_code(2), Key::Digit(1));
        assert_eq!(Key::from_code(11), Key::Digit(0));
        assert_eq!(Key::from_code(71), Key::Digit(7));
        assert_eq!(Key::from_code(12), Key::Character('-'));
        assert_eq!(Key::from_code(13), Key::Character('='));
        assert_eq!(Key::from_code(41), Key::Character('`'));
        assert_eq!(Key::from_code(43), Key::Character('\\'));
        assert_eq!(Key::from_code(26), Key::Character('['));
        assert_eq!(Key::from_code(27), Key::Character(']'));
    }

    #[test]
    fn modifiers_functions_navigation_media_and_brightness_map_from_codes() {
        assert_eq!(Key::from_code(29), Key::Ctrl);
        assert_eq!(Key::from_code(97), Key::RCtrl);
        assert_eq!(Key::from_code(42), Key::Shift);
        assert_eq!(Key::from_code(54), Key::RShift);
        assert_eq!(Key::from_code(56), Key::Alt);
        assert_eq!(Key::from_code(100), Key::RAlt);
        assert_eq!(Key::from_code(125), Key::Meta);
        assert_eq!(Key::from_code(126), Key::RMeta);
        assert_eq!(Key::from_code(58), Key::CapsLock);
        assert_eq!(Key::from_code(59), Key::F(1));
        assert_eq!(Key::from_code(68), Key::F(10));
        assert_eq!(Key::from_code(103), Key::Up);
        assert_eq!(Key::from_code(108), Key::Down);
        assert_eq!(Key::from_code(105), Key::Left);
        assert_eq!(Key::from_code(106), Key::Right);
        assert_eq!(Key::from_code(113), Key::Mute);
        assert_eq!(Key::from_code(114), Key::VolumeDown);
        assert_eq!(Key::from_code(115), Key::VolumeUp);
        assert_eq!(Key::from_code(224), Key::BrightnessDown);
        assert_eq!(Key::from_code(225), Key::BrightnessUp);
        assert_eq!(Key::from_code(163), Key::NextSong);
        assert_eq!(Key::from_code(164), Key::PlayPause);
        assert_eq!(Key::from_code(165), Key::PreviousSong);
        assert_eq!(Key::from_code(58), Key::CapsLock);
    }

    #[test]
    fn unknown_codes_map_to_unknown_with_the_original_value() {
        assert_eq!(Key::from_code(0xFFFF), Key::Unknown(0xFFFF));
        assert_eq!(Key::from_code(0), Key::Unknown(0));
    }

    // ---- Recognizer ----------------------------------------------------------

    #[test]
    fn press_emits_press_immediately() {
        let mut rec = GestureRecognizer::default();
        let events = press(&mut rec, Key::Escape, base(), 0);
        assert_press(&events, Key::Escape);
    }

    #[test]
    fn duplicate_down_is_ignored_without_resetting_the_clock() {
        let start = base();
        let mut rec = GestureRecognizer::default();
        press(&mut rec, Key::Escape, start, 0);
        assert!(press(&mut rec, Key::Escape, start, 400).is_empty());
        assert!(rec.advance(at_ms(start, 499)).is_empty());
        assert_long(&rec.advance(at_ms(start, 500)), Key::Escape);
    }

    #[test]
    fn double_press_within_the_gap_emits_double_press_and_releases() {
        let start = base();
        let mut rec = GestureRecognizer::default();
        press(&mut rec, Key::Escape, start, 0);
        assert_release(&release(&mut rec, Key::Escape, start, 50), Key::Escape);
        press(&mut rec, Key::Escape, start, 200);
        let events = release(&mut rec, Key::Escape, start, 300);
        // The physical key-up reports Release first, then the gesture: the
        // second release inside the gap yields `Release → DoublePress`.
        assert_eq!(
            events,
            vec![
                KeyEvent {
                    key: Key::Escape,
                    gesture: KeyGesture::Release,
                },
                KeyEvent {
                    key: Key::Escape,
                    gesture: KeyGesture::DoublePress,
                },
            ]
        );
    }

    #[test]
    fn second_release_outside_the_gap_starts_a_fresh_sequence() {
        let start = base();
        let mut rec = GestureRecognizer::default();
        press(&mut rec, Key::Escape, start, 0);
        assert_release(&release(&mut rec, Key::Escape, start, 40), Key::Escape);
        press(&mut rec, Key::Escape, start, 2000);
        assert_release(&release(&mut rec, Key::Escape, start, 2100), Key::Escape);
    }

    #[test]
    fn long_press_fires_once_at_the_deadline_and_release_is_reported() {
        let start = base();
        let mut rec = GestureRecognizer::default();
        assert_press(&press(&mut rec, Key::Escape, start, 0), Key::Escape);
        assert_long(&rec.advance(at_ms(start, 500)), Key::Escape);
        assert_release(&release(&mut rec, Key::Escape, start, 600), Key::Escape);
    }

    #[test]
    fn release_after_long_press_never_doubles() {
        let start = base();
        let mut rec = GestureRecognizer::default();
        press(&mut rec, Key::Escape, start, 0);
        rec.advance(at_ms(start, 500));
        assert_release(&release(&mut rec, Key::Escape, start, 600), Key::Escape);
        press(&mut rec, Key::Escape, start, 700);
        assert_release(&release(&mut rec, Key::Escape, start, 800), Key::Escape);
    }

    #[test]
    fn double_press_in_the_window_precedes_long_press() {
        let start = base();
        let mut rec = GestureRecognizer::default();
        press(&mut rec, Key::Escape, start, 0);
        assert_release(&release(&mut rec, Key::Escape, start, 20), Key::Escape);
        press(&mut rec, Key::Escape, start, 300);
        let events = release(&mut rec, Key::Escape, start, 350);
        assert_eq!(
            events,
            vec![
                KeyEvent {
                    key: Key::Escape,
                    gesture: KeyGesture::Release,
                },
                KeyEvent {
                    key: Key::Escape,
                    gesture: KeyGesture::DoublePress,
                },
            ]
        );
        // Wait past long-press deadline: no LongPress should double-emit here.
        assert!(rec.advance(at_ms(start, 900)).is_empty());
    }

    #[test]
    fn overlapping_holds_are_recognized_independently() {
        let start = base();
        let mut rec = GestureRecognizer::default();
        press(&mut rec, Key::Escape, start, 0);
        press(&mut rec, Key::Tab, start, 20);
        let events = rec.advance(at_ms(start, 700));
        assert_eq!(
            events,
            vec![
                KeyEvent {
                    key: Key::Escape,
                    gesture: KeyGesture::LongPress,
                },
                KeyEvent {
                    key: Key::Tab,
                    gesture: KeyGesture::LongPress,
                },
            ]
        );
        // Each key keeps its own LongPress/Release state: releasing one never
        // affects the other, and each hold gets exactly one Release.
        assert_release(&release(&mut rec, Key::Escape, start, 800), Key::Escape);
        assert_release(&release(&mut rec, Key::Tab, start, 900), Key::Tab);
        assert!(rec.advance(at_ms(start, 1000)).is_empty());
    }

    #[test]
    fn short_press_emits_press_then_release_without_long_press() {
        let start = base();
        let mut rec = GestureRecognizer::default();
        assert_press(&press(&mut rec, Key::Escape, start, 0), Key::Escape);
        assert_release(&release(&mut rec, Key::Escape, start, 50), Key::Escape);
        // Nothing was held past the threshold, so no LongPress can appear.
        assert!(rec.advance(at_ms(start, 900)).is_empty());
    }

    #[test]
    fn long_press_emits_press_longpress_then_release() {
        let start = base();
        let mut rec = GestureRecognizer::default();
        assert_press(&press(&mut rec, Key::Escape, start, 0), Key::Escape);
        assert_long(&rec.advance(at_ms(start, 500)), Key::Escape);
        assert_release(&release(&mut rec, Key::Escape, start, 600), Key::Escape);
    }

    #[test]
    fn long_press_fires_only_once_during_a_single_hold() {
        let start = base();
        let mut rec = GestureRecognizer::default();
        press(&mut rec, Key::Escape, start, 0);
        assert_long(&rec.advance(at_ms(start, 500)), Key::Escape);
        // The key is still physically held; further ticks must not re-fire
        // LongPress.
        assert!(rec.advance(at_ms(start, 700)).is_empty());
        assert!(rec.advance(at_ms(start, 900)).is_empty());
        assert_release(&release(&mut rec, Key::Escape, start, 1000), Key::Escape);
    }

    #[test]
    fn release_before_threshold_never_produces_long_press() {
        let start = base();
        let mut rec = GestureRecognizer::default();
        assert_press(&press(&mut rec, Key::Escape, start, 0), Key::Escape);
        assert_release(&release(&mut rec, Key::Escape, start, 499), Key::Escape);
        assert!(rec.advance(at_ms(start, 900)).is_empty());
    }

    #[test]
    fn repeated_key_down_does_not_duplicate_long_press_or_held_state() {
        let start = base();
        let mut rec = GestureRecognizer::default();
        assert_press(&press(&mut rec, Key::Escape, start, 0), Key::Escape);
        // Kernel auto-repeat / spurious re-downs for the still-held key are
        // ignored and cannot reset the long-press clock.
        assert!(press(&mut rec, Key::Escape, start, 100).is_empty());
        assert!(press(&mut rec, Key::Escape, start, 480).is_empty());
        assert_long(&rec.advance(at_ms(start, 500)), Key::Escape);
        assert!(press(&mut rec, Key::Escape, start, 550).is_empty());
        // Still one hold, exactly one LongPress, one Release on key-up.
        assert!(rec.advance(at_ms(start, 900)).is_empty());
        assert_release(&release(&mut rec, Key::Escape, start, 1000), Key::Escape);
    }

    #[test]
    fn clear_resets_held_state() {
        let start = base();
        let mut rec = GestureRecognizer::default();
        press(&mut rec, Key::Escape, start, 0);
        rec.clear();
        assert!(rec.advance(at_ms(start, 900)).is_empty());
    }

    // ---- ReaderState ---------------------------------------------------------

    #[test]
    fn reader_state_gates_reconnection_on_its_disconnect_deadline() {
        let start = base();
        let ready = ReaderState::new(start);
        assert!(ready.should_reconnect(start));
        let disconnected = ready.disconnected(start);
        assert!(!disconnected.should_reconnect(at_ms(start, 2500)));
        assert!(disconnected.should_reconnect(at_ms(start, 3500)));
    }

    // ---- Uevent parsing ------------------------------------------------------

    // Real NETLINK_KOBJECT_UEVENT datagrams carry no netlink header: the
    // payload begins directly with the kernel uevent prefix word
    // (`add@/devices/...`, NUL-terminated), followed by NUL-separated
    // `KEY=VALUE` env fields. Tests emit exactly that wire layout.
    fn uevent_buffer(payload: &[&[u8]]) -> Vec<u8> {
        let mut buffer: Vec<u8> = Vec::new();
        for field in payload {
            buffer.extend_from_slice(field);
            buffer.push(0);
        }
        buffer
    }

    #[test]
    fn parses_add_uevent_with_action_and_authoritative_devpath() {
        let buffer = uevent_buffer(&[
            b"add@/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11",
            b"ACTION=add",
            b"DEVPATH=/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11",
            b"SUBSYSTEM=input",
        ]);
        let event = parse_uevent(&buffer).expect("parseable");
        assert_eq!(event.action, "add");
        assert_eq!(event.subsystem.as_deref(), Some("input"));
        assert_eq!(
            event.devpath.as_deref(),
            Some("/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11")
        );
    }

    #[test]
    fn parses_remove_uevent_with_action_and_authoritative_devpath() {
        let buffer = uevent_buffer(&[
            b"remove@/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11",
            b"ACTION=remove",
            b"DEVPATH=/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11",
            b"SUBSYSTEM=input",
        ]);
        let event = parse_uevent(&buffer).expect("parseable");
        assert_eq!(event.action, "remove");
        assert_eq!(event.subsystem.as_deref(), Some("input"));
        assert_eq!(
            event.devpath.as_deref(),
            Some("/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11")
        );
    }

    #[test]
    fn parses_change_uevent_with_action_and_authoritative_devpath() {
        let buffer = uevent_buffer(&[
            b"change@/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11",
            b"ACTION=change",
            b"DEVPATH=/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11",
            b"SUBSYSTEM=input",
        ]);
        let event = parse_uevent(&buffer).expect("parseable");
        assert_eq!(event.action, "change");
        assert_eq!(
            event.devpath.as_deref(),
            Some("/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11")
        );
    }

    #[test]
    fn tolerates_extra_arbitrary_key_value_fields() {
        let buffer = uevent_buffer(&[
            b"add@/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11",
            b"ACTION=add",
            b"DEVPATH=/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11",
            b"SUBSYSTEM=input",
            b"SEQNUM=4811",
            b"MAJOR=13",
            b"MINOR=71",
            b"DEVNAME=input/event11",
            b"SOME_VENDOR_EXTENSION=foo=bar",
        ]);
        let event = parse_uevent(&buffer).expect("parseable");
        assert_eq!(event.action, "add");
        assert_eq!(event.subsystem.as_deref(), Some("input"));
        assert_eq!(
            event.devpath.as_deref(),
            Some("/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11")
        );
    }

    #[test]
    fn devpath_field_is_authoritative_over_prefix_word() {
        let buffer = uevent_buffer(&[
            b"remove@/devices/decoy/prefix/only",
            b"ACTION=remove",
            b"DEVPATH=/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11",
            b"SUBSYSTEM=input",
        ]);
        let event = parse_uevent(&buffer).expect("parseable");
        assert_eq!(event.action, "remove");
        // The authoritative device path comes from DEVPATH=, never by
        // reconstructing the `remove@...` prefix word.
        assert_eq!(
            event.devpath.as_deref(),
            Some("/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11")
        );
        assert_ne!(event.devpath.as_deref(), Some("/devices/decoy/prefix/only"));
    }

    #[test]
    fn action_field_is_authoritative_over_prefix_word() {
        let buffer = uevent_buffer(&[
            b"add@/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11",
            b"ACTION=remove",
            b"DEVPATH=/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11",
            b"SUBSYSTEM=input",
        ]);
        let event = parse_uevent(&buffer).expect("parseable");
        assert_eq!(event.action, "remove");
    }

    #[test]
    fn empty_buffer_has_no_uevent() {
        assert!(parse_uevent(&[]).is_none());
        assert!(parse_uevent(b"not-a-uevent").is_none());
    }

    // A remove uevent is honored only when it carries the authoritative
    // `DEVPATH=` field. The device is identified by DEVPATH exactly: a device
    // matching DEVPATH is removed even when the `remove@...` prefix word and
    // `DEVNAME=` point at a different event device.
    #[test]
    fn remove_uevent_uses_devpath_and_never_prefix_or_devname() {
        let present = Arc::new(AtomicBool::new(true));
        let probe = Arc::clone(&present);
        let mut state = WorkerState::new(move || {
            if probe.load(Ordering::Relaxed) {
                vec![
                    Pb::from("/dev/input/event11"),
                    Pb::from("/dev/input/event99"),
                ]
            } else {
                Vec::new()
            }
        });
        let now = base();
        state.rescan(now);
        assert!(state.devices.contains_key(&Pb::from("/dev/input/event11")));
        assert!(state.devices.contains_key(&Pb::from("/dev/input/event99")));

        // The kernel has dropped the device from udev, so the reconciliation
        // rescan no longer reports it; only the remove handling matters now.
        present.store(false, Ordering::Relaxed);

        let event = parse_uevent(&uevent_buffer(&[
            b"remove@/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input99/event99",
            b"ACTION=remove",
            b"DEVPATH=/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11",
            b"SUBSYSTEM=input",
            b"DEVNAME=input/event99",
        ]))
        .expect("parseable");
        assert_eq!(event.action, "remove");

        state.handle_uevent(&event, now);

        // The device identified by DEVPATH= is removed; the decoy
        // `remove@.../event99` prefix word and `DEVNAME=input/event99` are
        // never used to reconstruct a removal path.
        assert!(!state.devices.contains_key(&Pb::from("/dev/input/event11")));
        assert!(state.devices.contains_key(&Pb::from("/dev/input/event99")));
    }

    // A remove uevent without DEVPATH= is incomplete/invalid: it is safely
    // ignored, and no device is removed even when the `remove@...` prefix word
    // and `DEVNAME=` look like valid event devices.
    #[test]
    fn remove_uevent_without_devpath_is_safely_ignored() {
        let present = Arc::new(AtomicBool::new(true));
        let probe = Arc::clone(&present);
        let mut state = WorkerState::new(move || {
            if probe.load(Ordering::Relaxed) {
                vec![Pb::from("/dev/input/event11")]
            } else {
                Vec::new()
            }
        });
        let now = base();
        state.rescan(now);
        assert!(state.devices.contains_key(&Pb::from("/dev/input/event11")));

        present.store(false, Ordering::Relaxed);

        let event = parse_uevent(&uevent_buffer(&[
            b"remove@/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/input/input11/event11",
            b"ACTION=remove",
            b"SUBSYSTEM=input",
            b"DEVNAME=input/event11",
        ]))
        .expect("parseable");
        assert_eq!(event.action, "remove");
        assert!(event.devpath.is_none());

        state.handle_uevent(&event, now);

        // No device path is reconstructed from the `remove@...` prefix word or
        // from DEVNAME=: the device stays.
        assert!(state.devices.contains_key(&Pb::from("/dev/input/event11")));
    }
}
