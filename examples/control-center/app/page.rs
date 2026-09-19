//! Home page (`/`): a CSS animation smoothness test with a live counter.
//!
//! The whole screen — 2008×60 logical pixels, rendered 1:1 on the bar (the
//! default scale factor of both backends is 1.0) — is a near-black background
//! with a single small circle sliding continuously from the left edge of the
//! bar to the right edge and back. The motion is one infinite CSS
//! `transform: translateX(...)` animation with a linear timing function, driven
//! entirely by the runtime's CSS animation pipeline:
//!
//! ```text
//! Dioxus → Blitz → Stylo CSS animation → document.resolve(now)
//!         → Vello → Frame → Preview/DRM
//! ```
//!
//! The circle is 22px in diameter and keeps a 16px margin from both edges, so
//! it travels `2008 − 2·16 − 22 = 1954px` per half-cycle: `translateX(0)` →
//! `translateX(1954px)` → `translateX(0)`, one complete left-to-right-to-left
//! cycle every 20s, `linear` and `infinite`.
//!
//! In the top-right corner a counter advances once every 10ms. It is Dioxus
//! state mutated by a timer — a `setInterval(..., 500)` equivalent
//! (see [`Interval::new`]). Each tick writes the signal, the scheduler wakes,
//! and the runtime re-renders the chip, which validates timer-driven state
//! updates alongside the pure animation.
//!
//! The timer deliberately does *not* use `tokio::time`: the reactor is not
//! running when the app mounts (the preview enters its Tokio runtime only after
//! [`touchbard::run`]'s initial render, and DRM has no reactor at all), so a
//! Tokio interval would panic. Instead the ticker is a helper thread that sleeps
//! `period` and wakes the polling future through its stored [`Waker`] — the same
//! reactor-free pattern `touchbard-keyboard` uses for its subscriptions.

use std::future::poll_fn;
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

use dioxus::prelude::*;

/// A reactor-free periodic timer: the native equivalent of
/// `setInterval(callback, period)`.
///
/// A helper thread sleeps `period`, then wakes whatever future is polling for
/// the next tick. `tick()` yields, and advances/returns the number of periods
/// that elapsed since the previous read, so a delayed wake still lands on the
/// correct value. The thread runs until the app exits (the home page never
/// unmounts).
#[derive(Clone)]
struct Interval {
    inner: Arc<Mutex<IntervalInner>>,
}

struct IntervalInner {
    period: Duration,
    next: Instant,
    waker: Option<Waker>,
}

impl Interval {
    fn new(period: Duration) -> Self {
        let interval = Self {
            inner: Arc::new(Mutex::new(IntervalInner {
                period,
                next: Instant::now() + period,
                waker: None,
            })),
        };
        let ticking = Arc::clone(&interval.inner);
        thread::Builder::new()
            .name("control-center-setInterval".into())
            .spawn(move || loop {
                thread::sleep(period);
                let waker = { ticking.lock().unwrap().waker.take() };
                if let Some(waker) = waker {
                    waker.wake();
                }
            })
            .expect("failed to spawn the counter interval timer");
        interval
    }

    /// Wait until the next tick and return how many periods have elapsed since
    /// the last one (normally `1`).
    async fn tick(&self) -> u32 {
        poll_fn(|cx| {
            let mut inner = self.inner.lock().unwrap();
            let now = Instant::now();
            if now >= inner.next {
                let mut ticks = 0;
                loop {
                    let due = now >= inner.next;
                    if !due {
                        break;
                    }
                    let period = inner.period;
                    inner.next += period;
                    ticks += 1;
                }
                Poll::Ready(ticks)
            } else {
                inner.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}

#[rustfmt::skip]
const HOME_STYLE: &str = r#"
    @keyframes circleSlide {
        from { transform: translateX(0px); }
        50%  { transform: translateX(1954px); }
        to   { transform: translateX(0px); }
    }

    .stage {
        position: relative;
        width: 100%; height: 100%;
        display: flex; flex-direction: column;
        justify-content: center; align-items: flex-start;
        background: #0b0e14; overflow: hidden;
    }
    .ball {
        width: 22px; height: 22px; margin-left: 16px;
        background: #7aa2f7; border-radius: 50%;
        animation: circleSlide 60s linear infinite;
    }
    .counter {
        position: absolute; top: 6px; right: 10px;
        padding: 1px 8px;
        font-size: 11px; line-height: 14px;
        color: #c0caf5; background: #24283b;
        border: 1px solid #414868; border-radius: 3px;
    }
"#;

#[component]
pub fn Page() -> Element {
    // `setInterval(..., 500)`: one `Interval` timer, started once; each tick
    // advances the counter state and re-renders the chip in the corner.
    let mut count = use_signal(|| 0i32);
    let interval = use_hook(|| Interval::new(Duration::from_millis(10)));
    use_future(move || {
        let interval = interval.clone();
        async move {
            loop {
                let ticks = interval.tick().await;
                count += ticks as i32;
            }
        }
    });

    rsx! {
        div {
            class: "stage",
            style { {HOME_STYLE} }
            div {
                class: "counter",
                "Count: {count}"
            }
            div { class: "ball" }
        }
    }
}
