//! Keyboard event input. This is the exact hop measured as the ONLY difference
//! between the known-good state and the stall:
//!
//!   subscription.recv().await  →  latest.set(Some(event))  →  [Dioxus reactive
//!   scheduler]  →  host wake  →  TouchbardSystem::frame  →  render → present
//!
//! The upstream path (kernel, DRM, page flip, present) was proven fast while
//! typing (kmscube sustained ~302 fps on card2), so the only remaining suspect
//! is this reactive hop. Controlled here so we can measure both sides without
//! changing renderer/DRM/pacing behaviour:
//!
//!   * TOUCHBARD_KBD_FEED=1  re-enables `latest.set` (reproduces the stall for
//!     measurement). Off by default: preserves the current working state.
//!   * TOUCHBARD_KBD_DIAG=1   prints recv→set timestamps via eprintln (no
//!     tracing dependency required by this example crate).

use dioxus::prelude::*;
use touchbard_keyboard::{KeyEvent, Keyboard};

fn feed_enabled() -> bool {
    std::env::var("TOUCHBARD_KBD_FEED").map(|v| v == "1").unwrap_or(false)
}

fn diag_enabled() -> bool {
    std::env::var("TOUCHBARD_KBD_DIAG").map(|v| v == "1").unwrap_or(false)
}

pub fn use_key_events() -> Signal<Option<KeyEvent>> {
    let mut latest = use_signal(|| None);
    let subscription = use_hook(|| Keyboard::global().subscribe());
    let feed = use_hook(feed_enabled);
    let diag = use_hook(diag_enabled);
    use_future(move || {
        let subscription = subscription.clone();
        async move {
            while let Some(event) = subscription.recv().await {
                let hop = std::time::Instant::now();
                if feed {
                    latest.set(Some(event));
                }
                if diag {
                    let now_ms = hop.elapsed().as_micros();
                    eprintln!(
                        "[kbd-diag] feed={} recv->set={}us",
                        feed, now_ms
                    );
                }
            }
        }
    });
    latest
}
