use dioxus::prelude::*;
use touchbard_keyboard::{KeyEvent, Keyboard};

pub fn use_key_events() -> Signal<Option<KeyEvent>> {
    let mut latest = use_signal(|| None);
    let subscription = use_hook(|| Keyboard::global().subscribe());
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
