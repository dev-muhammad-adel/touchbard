//! Control Center - the Touch Bar application.
//!
//! A deliberately thin entry point: CLI parsing, configuration construction and
//! the file-based routing tree. All runtime/plumbing lives behind [`touchbard::run`].
//!
//! The route tree lives in `app/` next to this source file
//! (`examples/control-center/app/`) and is discovered at build time by
//! `touchbard::routing::app_router!()`, which resolves it relative to the
//! invoking file. The macro is an *expression*: it expands to the generated
//! root router component, which is handed to [`touchbard::run`] as the app
//! argument — no separate `Router` symbol is needed.
//!
//! The display backend is selected here (the entry crate): the browser preview
//! (`touchbard-preview`) or the DRM/KMS backend (`touchbard-drm`), both boxed
//! behind the [`touchbard::Backend`] boundary so the runtime stays independent
//! of them.
//!
//! Run:
//! ```text
//! cargo run --example control-center -- --preview   # default
//! cargo run --example control-center -- --drm       # the DRM/KMS backend (the Touch Bar itself)
//! ```
//!
//! Preview dimensions come from `TOUCHBARD_WIDTH`, `TOUCHBARD_HEIGHT` and
//! `TOUCHBARD_SCALE` (see [`touchbard_preview::PreviewConfig`]).
//!
//! The Blitz DOM is not `Send` (thread-local contexts), so the whole pipeline
//! runs on one thread inside the preview's own current-thread Tokio runtime +
//! `LocalSet`.

use touchbard::{Backend, TouchbardConfig};
use touchbard_drm::{DrmBackend, DrmConfig};
use touchbard_preview::PreviewBackend;

#[path = "hooks/use_key_events.rs"]
mod keyboard;

#[path = "hooks/use_pointer_moving.rs"]
mod pointer;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: cargo run --example control-center -- [--preview | --drm]");
        return;
    }

    let backend: Box<dyn Backend> = if args.iter().any(|a| a == "--drm") {
        Box::new(DrmBackend::new(DrmConfig::default()))
    } else {
        Box::new(PreviewBackend::from_env())
    };

    if let Err(e) = touchbard::run(
        touchbard::routing::app_router!(),
        TouchbardConfig { backend },
    ) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
