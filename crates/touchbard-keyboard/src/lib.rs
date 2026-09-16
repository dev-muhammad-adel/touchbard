//! Keyboard input, gesture recognition, and reconnect lifecycle handling.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

const RECONNECT_DELAY: Duration = Duration::from_secs(3);
const DOUBLE_PRESS_GAP: Duration = Duration::from_millis(350);
const LONG_PRESS_DURATION: Duration = Duration::from_millis(500);
const WORKER_TICK: Duration = Duration::from_millis(50);
const DISCOVERY_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
            59..=68 => Self::F((code - 58) as u8),
            87 => Self::F(11),
            88 => Self::F(12),
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
            _ => Self::Unknown(code),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyGesture {
    Press,
    LongPress,
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

#[derive(Debug)]
pub struct GestureRecognizer {
    double_gap: Duration,
    long_duration: Duration,
    held: Option<HeldKey>,
    last_release: Option<(Key, Instant)>,
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
            held: None,
            last_release: None,
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
        let Some(held) = self.held.as_mut() else {
            return Vec::new();
        };
        if !held.long_fired && now.duration_since(held.pressed_at) >= self.long_duration {
            held.long_fired = true;
            return vec![KeyEvent {
                key: held.key,
                gesture: KeyGesture::LongPress,
            }];
        }
        Vec::new()
    }

    pub fn clear(&mut self) {
        self.held = None;
        self.last_release = None;
    }

    fn press(&mut self, key: Key, now: Instant) -> Vec<KeyEvent> {
        let mut events = self.advance(now);
        self.held = Some(HeldKey {
            key,
            pressed_at: now,
            long_fired: false,
        });
        events.push(KeyEvent {
            key,
            gesture: KeyGesture::Press,
        });
        events
    }

    fn release(&mut self, key: Key, now: Instant) -> Vec<KeyEvent> {
        let Some(held) = self.held.take() else {
            return Vec::new();
        };
        if held.key != key {
            self.clear();
            return Vec::new();
        }
        if held.long_fired {
            self.last_release = None;
            return Vec::new();
        }
        let double = self
            .last_release
            .filter(|(last_key, at)| *last_key == key && now.duration_since(*at) <= self.double_gap)
            .is_some();
        self.last_release = if double { None } else { Some((key, now)) };
        if double {
            vec![KeyEvent {
                key,
                gesture: KeyGesture::DoublePress,
            }]
        } else {
            Vec::new()
        }
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

#[derive(Clone)]
pub struct Keyboard {
    inner: Arc<KeyboardInner>,
}

struct KeyboardInner {
    commands: mpsc::Sender<Command>,
    subscribers: Arc<Mutex<Vec<Arc<Mutex<SubscriptionState>>>>>,
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
    Suspended,
}

struct DeviceState {
    fd: Option<OwnedFd>,
    recognizer: GestureRecognizer,
    state: ReaderState,
}

impl ReaderState {
    fn new(now: Instant) -> Self {
        Self::Waiting(now)
    }

    fn disconnected(self, now: Instant) -> Self {
        match self {
            Self::Suspended => Self::Suspended,
            Self::Waiting(_) | Self::Ready => Self::Waiting(now + RECONNECT_DELAY),
        }
    }

    fn should_reconnect(self, now: Instant) -> bool {
        matches!(self, Self::Waiting(at) if now >= at)
    }
}

impl Keyboard {
    pub fn global() -> &'static Self {
        static KEYBOARD: OnceLock<Keyboard> = OnceLock::new();
        KEYBOARD.get_or_init(Self::new)
    }

    pub fn new() -> Self {
        let (commands, command_rx) = mpsc::channel();
        let subscribers = Arc::new(Mutex::new(Vec::new()));
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
            subscribers.push(Arc::clone(&state));
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
            for subscriber in subscribers.iter() {
                if let Ok(mut state) = subscriber.lock() {
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

fn run_worker(
    commands: mpsc::Receiver<Command>,
    subscribers: Arc<Mutex<Vec<Arc<Mutex<SubscriptionState>>>>>,
) {
    let mut devices = HashMap::<PathBuf, DeviceState>::new();
    let mut suspended = false;
    let mut next_discovery = Instant::now();
    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Suspend => {
                    tracing::info!("keyboard suspend: releasing input device and clearing state");
                    suspended = true;
                    devices.clear();
                }
                Command::Resume => {
                    tracing::info!("keyboard resume: rediscovering input device");
                    suspended = false;
                    devices.clear();
                    next_discovery = Instant::now();
                }
                Command::Stop => return,
            }
        }
        if suspended {
            thread::sleep(WORKER_TICK);
            continue;
        }

        let now = Instant::now();
        if now >= next_discovery {
            add_discovered(&mut devices, find_keyboard_devices(), now);
            next_discovery = now + DISCOVERY_INTERVAL;
        }

        for (path, device) in devices.iter_mut() {
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
                    tracing::warn!(path = %path.display(), "keyboard input reader lost the device; retrying");
                    mark_failed(&mut devices, path);
                } else if pollfd.revents & libc::POLLIN != 0 {
                    read_device(&mut devices, path, &subscribers);
                }
            }
        }
        let now = Instant::now();
        for device in devices.values_mut() {
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
    subscribers: &Arc<Mutex<Vec<Arc<Mutex<SubscriptionState>>>>>,
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
    if read != std::mem::size_of::<InputEvent>() as isize {
        tracing::warn!(path = %path.display(), "keyboard input reader failed to read a complete event; retrying");
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

fn publish(subscribers: &Arc<Mutex<Vec<Arc<Mutex<SubscriptionState>>>>>, events: Vec<KeyEvent>) {
    if events.is_empty() {
        return;
    }
    for event in &events {
        tracing::info!(key = ?event.key, gesture = ?event.gesture, "keyboard event");
    }
    if let Ok(subscribers) = subscribers.lock() {
        for subscriber in subscribers.iter() {
            if let Ok(mut state) = subscriber.lock() {
                state.events.extend(events.iter().copied());
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

const EV_KEY: u16 = 0x01;

fn open_keyboard(path: &Path) -> io::Result<OwnedFd> {
    let file = fs::OpenOptions::new().read(true).open(&path)?;
    Ok(file.into())
}

fn find_keyboard_devices() -> Vec<PathBuf> {
    let input = Path::new("/sys/class/input");
    let mut devices = Vec::<(i32, PathBuf)>::new();
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
        let score = if bridge_matches(&sysfs) { 60 } else { 10 };
        devices.push((score, PathBuf::from(format!("/dev/input/{name}"))));
    }
    devices.sort_by_key(|(score, path)| (-*score, path.clone()));
    devices.into_iter().map(|(_, path)| path).collect()
}

fn bridge_matches(path: &Path) -> bool {
    let path = path.to_string_lossy();
    if let Ok(bridges) = std::env::var("REACT_DRM_USB_BRIDGE") {
        return bridges
            .split(',')
            .map(str::trim)
            .filter(|bridge| !bridge.is_empty())
            .any(|bridge| path.contains(bridge));
    }
    path.contains("apple-bce") || path.contains("bce-vhci")
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

    const KEY: Key = Key::Space;
    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn press_emits_once_and_repeat_is_ignored() {
        let start = Instant::now();
        let mut recognizer = GestureRecognizer::default();
        assert_eq!(
            recognizer.input(KEY, 1, start)[0].gesture,
            KeyGesture::Press
        );
        assert!(recognizer.input(KEY, 2, at(start, 100)).is_empty());
    }

    #[test]
    fn long_press_fires_at_deadline_and_release_does_not_double() {
        let start = Instant::now();
        let mut recognizer = GestureRecognizer::default();
        recognizer.input(KEY, 1, start);
        assert_eq!(
            recognizer.advance(at(start, 500))[0].gesture,
            KeyGesture::LongPress
        );
        assert!(recognizer.input(KEY, 0, at(start, 600)).is_empty());
    }

    #[test]
    fn second_release_within_gap_emits_double_press() {
        let start = Instant::now();
        let mut recognizer = GestureRecognizer::default();
        recognizer.input(KEY, 1, start);
        recognizer.input(KEY, 0, at(start, 10));
        recognizer.input(KEY, 1, at(start, 100));
        assert_eq!(
            recognizer.input(KEY, 0, at(start, 200))[0].gesture,
            KeyGesture::DoublePress
        );
    }

    #[test]
    fn interactions_and_suspend_clear_state() {
        let start = Instant::now();
        let mut recognizer = GestureRecognizer::default();
        recognizer.input(KEY, 1, start);
        recognizer.clear();
        assert!(recognizer.input(KEY, 0, at(start, 10)).is_empty());
        recognizer.input(KEY, 1, at(start, 20));
        assert!(recognizer.input(KEY, 0, at(start, 500)).is_empty());
    }

    #[test]
    fn release_without_press_and_key_mismatch_do_not_leak_state() {
        let start = Instant::now();
        let mut recognizer = GestureRecognizer::default();
        assert!(recognizer.input(KEY, 0, start).is_empty());
        recognizer.input(KEY, 1, start);
        assert!(recognizer.input(Key::Enter, 0, at(start, 10)).is_empty());
        assert!(recognizer.advance(at(start, 1_000)).is_empty());
    }

    #[test]
    fn reader_failure_waits_before_reconnect_and_suspend_blocks_it() {
        let start = Instant::now();
        let state = ReaderState::new(start).disconnected(start);
        assert!(!state.should_reconnect(at(start, 2_999)));
        assert!(state.should_reconnect(at(start, 3_000)));
        let suspended = ReaderState::Suspended;
        assert!(!suspended.should_reconnect(at(start, 30_000)));
        assert!(ReaderState::Waiting(at(start, 30_000)).should_reconnect(at(start, 30_000)));
    }

    #[test]
    fn discovered_devices_get_one_reader_each_and_new_devices_are_added() {
        let start = Instant::now();
        let first = PathBuf::from("/dev/input/event-a");
        let second = PathBuf::from("/dev/input/event-b");
        let third = PathBuf::from("/dev/input/event-c");
        let mut devices = HashMap::new();
        add_discovered(
            &mut devices,
            [first.clone(), second.clone(), first.clone()],
            start,
        );
        assert_eq!(devices.len(), 2);
        add_discovered(&mut devices, [second, third], at(start, 250));
        assert_eq!(devices.len(), 3);
    }

    #[test]
    fn readers_merge_events_without_sharing_gesture_state() {
        let subscribers = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(Mutex::new(SubscriptionState {
            events: VecDeque::new(),
            waker: None,
            closed: false,
        }));
        subscribers.lock().unwrap().push(Arc::clone(&state));
        let subscription = KeyboardSubscription { state };
        publish(
            &subscribers,
            vec![
                KeyEvent {
                    key: Key::Space,
                    gesture: KeyGesture::Press,
                },
                KeyEvent {
                    key: Key::Enter,
                    gesture: KeyGesture::Press,
                },
            ],
        );
        assert_eq!(subscription.try_recv().unwrap().key, Key::Space);
        assert_eq!(subscription.try_recv().unwrap().key, Key::Enter);

        let start = Instant::now();
        let mut keyboard_one = GestureRecognizer::default();
        let mut keyboard_two = GestureRecognizer::default();
        keyboard_one.input(Key::Space, 1, start);
        keyboard_two.input(Key::Space, 1, at(start, 100));
        assert_eq!(
            keyboard_one.advance(at(start, 500))[0].gesture,
            KeyGesture::LongPress
        );
        assert!(keyboard_two.advance(at(start, 500)).is_empty());
    }
}
