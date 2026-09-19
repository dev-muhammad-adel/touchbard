//! Pointer integration for the Control Center app.
//!
//! The *use* side of `touchbard-pointer`: a bridge between the background
//! pointer reader and Dioxus state. The reader runs off the app's main loop
//! entirely — it opens the `/dev/input/event*` pointer devices (mouse,
//! trackpad, pointing stick) in its own worker thread
//! ([`Pointer::global()`](touchbard_pointer::Pointer)) and publishes a
//! `PointerEvent` whenever the pointer moves. This hook subscribes to that
//! stream and turns "the pointer is moving" into a reactive boolean.
//!
//! The idle countdown is deliberately *reactor-free*, like the `Interval` in
//! `app/page.rs`: Dioxus futures are polled by Dioxus's own scheduler, so a
//! `tokio::time::sleep` would panic with "no reactor running" the moment the
//! hook first runs. Even spawning a countdown thread that writes the signal
//! directly is off the table — `Signal` is `!Send` (`UnsyncStorage`) — so
//! [`IdleTimer`]'s worker thread wakes a stored [`Waker`] (the same
//! subscription wake pattern the keyboard reader uses) and lets the hook's
//! own poll loop flip the signal back to `false`.

use std::future::{poll_fn, Future};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

use dioxus::prelude::*;
use touchbard_pointer::Pointer;

/// How long the pointer may stay quiet before it counts as "idle".
const IDLE_MS: Duration = Duration::from_millis(400);

/// `true` while the mouse / trackpad is actively moving, `false` once it has
/// been still for [`IDLE_MS`] (400 ms). Read the returned signal elsewhere
/// (e.g. the `ptr:` readout in `app/layout.rs`) to react to pointer activity.
///
/// How it works:
/// * `use_hook(|| Pointer::global().subscribe())` subscribes exactly once —
///   [`use_hook`] only runs its initializer on the first render, so re-renders
///   do not create more readers or leak subscriptions.
/// * `use_future` drives a single poll loop: it drains motion events off the
///   subscription (setting the signal and re-arming the idle timer), waits
///   idle against the deadline, and flips the signal back to `false`.
pub fn use_pointer_moving() -> Signal<bool> {
    let mut moving = use_signal(|| false);
    let subscription = use_hook(|| Pointer::global().subscribe());
    // One idle timer for the component's lifetime; re-armed on every motion.
    let idle = use_hook(|| IdleTimer::start(IDLE_MS));
    use_future(move || {
        let subscription = subscription.clone();
        let idle = idle.clone();
        async move {
            let mut recv = Box::pin(subscription.recv());
            // Did we start an idle countdown we are still expecting to fire?
            let mut armed = false;
            poll_fn(|cx| {
                // 1) Drain queued motions. Each one re-arms the idle countdown.
                while subscription.try_recv().is_some() {
                    moving.set(true);
                    idle.arm();
                    armed = true;
                }
                // 2) Poll the subscription so it wakes us on the next motion.
                match recv.as_mut().poll(cx) {
                    Poll::Ready(Some(_)) => {
                        moving.set(true);
                        idle.arm();
                        armed = true;
                        recv = Box::pin(subscription.recv());
                    }
                    // Reader shut down; the stream is over.
                    Poll::Ready(None) => return Poll::Ready(()),
                    Poll::Pending => {}
                }
                // 3) Watch the idle countdown. `idle_poll` reports when the
                // deadline has been reached (or already fired) so we can flip
                // the signal back to "not moving".
                if idle_poll(&idle, Some(cx.waker()), &mut armed) {
                    moving.set(false);
                }
                Poll::Pending
            })
            .await;
        }
    });
    moving
}

/// Run one idle-timer check for the hook's poll loop; returns `true` when the
/// pointer has just gone idle and the caller should clear the moving flag.
///
/// The busy logic lives here (rather than inline) so it can be unit-tested
/// against a real [`IdleTimer`] without a Dioxus runtime. Given an `armed`
/// countdown it:
/// * registers the poller's waker while the deadline is still in the future;
/// * reports idle itself if the deadline is already past; and
/// * reports idle if the timer's deadline has gone — the worker wipes it right
///   before waking us, so the disappearance *is* the fire we were waiting for.
fn idle_poll(timer: &IdleTimer, waker: Option<&Waker>, armed: &mut bool) -> bool {
    if !*armed {
        return false;
    }
    match timer.deadline() {
        Some(deadline) if Instant::now() >= deadline => {
            timer.clear();
            *armed = false;
            true
        }
        Some(_) => {
            if let Some(waker) = waker {
                timer.register(waker.clone());
            }
            false
        }
        None => {
            *armed = false;
            true
        }
    }
}

/// A reactor-free resettable idle countdown.
///
/// [`IdleTimer::arm`] (re)starts the countdown and [`IdleTimer::register`]
/// stores the poller's waker; when the deadline elapses without another
/// `arm()`, the worker wakes it. A single helper thread waits on a condition
/// variable, so re-arming mid-countdown never leaks threads (the
/// naïvely per-motion "spawn a sleeping thread" alternative would, since a
/// fast swipe publishes hundreds of motions).
#[derive(Clone)]
struct IdleTimer {
    inner: Arc<IdleInner>,
}

struct IdleInner {
    duration: Duration,
    state: Mutex<IdleState>,
    sleep: Condvar,
}

#[derive(Default)]
struct IdleState {
    deadline: Option<Instant>,
    waker: Option<Waker>,
    quit: bool,
}

impl IdleTimer {
    fn start(duration: Duration) -> Self {
        let timer = Self {
            inner: Arc::new(IdleInner {
                duration,
                state: Mutex::new(IdleState::default()),
                sleep: Condvar::new(),
            }),
        };
        let worker = Arc::clone(&timer.inner);
        thread::Builder::new()
            .name("control-center-pointer-idle".into())
            .spawn(move || worker.run())
            .expect("failed to spawn the pointer idle timer");
        timer
    }

    /// (Re)start the idle countdown.
    fn arm(&self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.deadline = Some(Instant::now() + self.inner.duration);
        self.inner.sleep.notify_one();
    }

    /// Store the waker to be woken when the countdown elapses.
    fn register(&self, waker: Waker) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .waker = Some(waker);
    }

    /// The current countdown deadline, if armed.
    fn deadline(&self) -> Option<Instant> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .deadline
    }

    /// Cancel the countdown without firing.
    fn clear(&self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.deadline = None;
        self.inner.sleep.notify_one();
    }
}

impl IdleInner {
    fn run(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if state.quit {
                return;
            }
            match state.deadline {
                // Disarmed: sleep until a motion arms us.
                None => {
                    state = self
                        .sleep
                        .wait_while(state, |s| !s.quit && s.deadline.is_none())
                        .unwrap_or_else(|e| e.into_inner());
                }
                // Countdown: wait for the deadline, a re-arm, or shutdown.
                Some(deadline) => {
                    let now = Instant::now();
                    let (guard, _) = self
                        .sleep
                        .wait_timeout_while(
                            state,
                            deadline.saturating_duration_since(now),
                            |s| !s.quit && s.deadline == Some(deadline) && Instant::now() < deadline,
                        )
                        .unwrap_or_else(|e| e.into_inner());
                    state = guard;
                    if state.deadline == Some(deadline) && Instant::now() >= deadline {
                        let waker = state.waker.take();
                        state.deadline = None;
                        drop(state);
                        if let Some(waker) = waker {
                            waker.wake();
                        }
                        state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    }
                }
            }
        }
    }
}

impl Drop for IdleInner {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.quit = true;
            state.deadline = None;
            self.sleep.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A waker that counts its wake-ups, so the tests observe when the
    /// countdown fires without needing a reactor or a Dioxus signal.
    struct Counting(Arc<AtomicUsize>);

    impl std::task::Wake for Counting {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn idle_fires_once_after_a_quiet_period() {
        let fired = Arc::new(AtomicUsize::new(0));
        let timer = IdleTimer::start(Duration::from_millis(60));
        timer.register(Waker::from(Arc::new(Counting(Arc::clone(&fired)))));
        timer.arm();
        // Re-arm mid-countdown: the first deadline must be forgotten.
        std::thread::sleep(Duration::from_millis(30));
        timer.arm();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn idle_arms_again_after_firing() {
        let fired = Arc::new(AtomicUsize::new(0));
        let timer = IdleTimer::start(Duration::from_millis(60));
        timer.register(Waker::from(Arc::new(Counting(Arc::clone(&fired)))));
        timer.arm();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        timer.register(Waker::from(Arc::new(Counting(Arc::clone(&fired)))));
        timer.arm();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(fired.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn never_armed_never_fires() {
        let fired = Arc::new(AtomicUsize::new(0));
        let timer = IdleTimer::start(Duration::from_millis(20));
        timer.register(Waker::from(Arc::new(Counting(Arc::clone(&fired)))));
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(fired.load(Ordering::SeqCst), 0);
    }

    /// Wake payload for the drive-loop test: records that the timer woke us,
    /// the way waking the hook's task waker would schedule a re-poll.
    struct WakeFlag(Arc<AtomicBool>);
    impl std::task::Wake for WakeFlag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn drive_loop_detects_the_fire_after_a_quiet_period() {
        // Plays the hook's poll loop against a real IdleTimer: a motion arms
        // the countdown; when it fires the worker clears the deadline and
        // wakes the re-poll, and THAT poll must flip the moving flag.
        let pending = Arc::new(AtomicBool::new(false));
        let waker = Waker::from(Arc::new(WakeFlag(Arc::clone(&pending))));
        let timer = IdleTimer::start(Duration::from_millis(50));

        let mut armed = true;
        let mut moving = true; // a motion just happened
        timer.arm();

        // First poll: deadline is in the future, so the waker gets registered
        // and nothing is reported idle.
        assert!(!idle_poll(&timer, Some(&waker), &mut armed));
        assert!(moving);
        assert!(armed);

        // A re-arm mid-countdown, the way another motion would.
        timer.arm();
        assert!(!idle_poll(&timer, Some(&waker), &mut armed));
        assert!(armed);

        // Let the countdown elapse: the worker must wake our waker.
        std::thread::sleep(Duration::from_millis(250));
        assert!(
            pending.load(Ordering::SeqCst),
            "timer should have woken the poller"
        );
        pending.store(false, Ordering::SeqCst);

        // The re-poll at the fire sees the worker already cleared the
        // deadline, but armed is still set — that is the fire, so idle.
        assert!(idle_poll(&timer, Some(&waker), &mut armed));
        assert!(!armed);
        moving = false;
        assert!(!moving); // the caller flips the signal

        // And a fresh motion afterwards arms and works again.
        timer.arm();
        armed = true;
        moving = true;
        assert!(!idle_poll(&timer, Some(&waker), &mut armed));
        std::thread::sleep(Duration::from_millis(200));
        assert!(pending.load(Ordering::SeqCst));
        assert!(idle_poll(&timer, Some(&waker), &mut armed));
        assert!(!armed);
    }
}