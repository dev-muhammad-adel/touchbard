//! Keyboard integration for the Control Center app.
//!
//! This is the *use* side of `touchbard-keyboard`: a thin bridge between the
//! background keyboard reader and Dioxus state. The reader runs off the app's
//! main loop entirely — it opens the `/dev/input/event*` device in its own
//! worker thread ([`Keyboard::global()`](touchbard_keyboard::Keyboard)) and
//! just publishes events. This hook subscribes to the stream and turns the
//! *latest* event into a reactive Dioxus signal the UI can display.

use dioxus::prelude::*;
use touchbard_keyboard::{KeyEvent, Keyboard};

/// Expose the most recent physical key event as a reactive signal (or `None`
/// until the first event arrives).
///
/// How it works:
/// * `use_hook(|| Keyboard::global().subscribe())` subscribes exactly once —
///   [`use_hook`] only runs its initializer on the first render, so repeated
///   re-renders (navigating, clicking buttons) do not create more readers or
///   leak subscriptions.
/// * `use_future` then drives the async subscription: each awaited event
///   overwrites the signal, which wakes the Dioxus scheduler and re-renders
///   whatever reads it (the layout's `kbd:` readout, see `app/layout.rs`).
///
/// Reading the signal elsewhere (e.g. `keyboard_event()`) takes the last event
/// as-is; gestures like double-tap / long-press are already collapsed into a
/// single [`KeyEvent`] by the reader.
pub fn use_key_events() -> Signal<Option<KeyEvent>> {
    // Holds the most recent event; starts `None` so the UI can show "waiting".
    let mut latest = use_signal(|| None);
    // One subscription for the component's lifetime, not one per re-render.
    let subscription = use_hook(|| Keyboard::global().subscribe());
    // Pump the subscription forever: every event replaces `latest`, and the
    // signal write re-triggers the UI (no manual polling of the device here —
    // that is the reader's job).
    use_future(move || {
        let subscription = subscription.clone();
        async move {
            while let Some(event) = subscription.recv().await {
                latest.set(Some(event));
            }
        }
    });
    latest
}
