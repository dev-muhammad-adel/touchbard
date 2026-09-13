//! Control Center - the Touch Bar application.
//!
//! A deliberately thin entry point: CLI parsing, configuration construction and
//! the file-based routing tree. All runtime/plumbing lives behind [`touch_ui::run`].
//!
//! The route tree lives in `app/` next to this source file
//! (`examples/control-center/app/`) and is discovered at build time by
//! `touch_ui::routing::app_router!()`, which resolves it relative to the
//! invoking file. The macro is an *expression*: it expands to the generated
//! root router component, which is handed to [`touch_ui::run`] as the app
//! argument — no separate `Router` symbol is needed.
//!
//! The display backend is selected here (the entry crate): the browser preview
//! (`touch-ui-preview`) or the DRM/KMS scaffold (`touch-ui-drm`), both boxed
//! behind the [`touch_ui::Backend`] boundary so the runtime stays independent
//! of them.
//!
//! Run:
//! ```text
//! cargo run --example control-center -- --preview   # default
//! cargo run --example control-center -- --drm       # not implemented yet
//! ```
//!
//! Preview dimensions come from `TOUCH_UI_WIDTH`, `TOUCH_UI_HEIGHT` and
//! `TOUCH_UI_SCALE` (see [`touch_ui_preview::PreviewConfig`]).
//!
//! The Blitz DOM is not `Send` (thread-local contexts), so the whole pipeline
//! runs on one thread inside the preview's own current-thread Tokio runtime +
//! `LocalSet`.

use touch_ui::{Backend, TouchUiConfig};
use touch_ui_drm::{DrmBackend, DrmConfig};
use touch_ui_preview::PreviewBackend;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: cargo run --example control-center -- [--preview | --drm]");
        return;
    }

    let backend: Box<dyn Backend> = if args.iter().any(|a| a == "--drm") {
        Box::new(DrmBackend::new(DrmConfig))
    } else {
        Box::new(PreviewBackend::from_env())
    };

    if let Err(e) = touch_ui::run(
        touch_ui::routing::app_router!(),
        TouchUiConfig { backend },
    ) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}