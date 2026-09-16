//! Routing runtime for the Touchbard framework.
//!
//! The route *tree* is discovered from the filesystem at build time by the
//! [`app_router!`] proc-macro (`touchbard-macros`). This module is the
//! framework side of that contract: everything the generated code and the app
//! code need to navigate and to read the current route.
//!
//! Concepts (Next.js-style, adapted to a single-view, single-threaded native
//! strip):
//!
//! * The generated router component — produced by the [`app_router!`]
//!   proc-macro. The macro is an *expression* that expands to the root router
//!   component (`fn() -> Element`), so apps pass it straight to
//!   [`touchbard::run`](crate::run()):
//!
//!   ```text
//!   touchbard::run(touchbard::routing::app_router!(), TouchbardConfig { backend })
//!   ```
//!
//!   The generated component matches the current path against the build-time
//!   route tree, renders the winning page inside its ancestor `layout.rs`
//!   components, and provides navigation state.
//! * [`use_navigate`] — returns a [`Navigator`] with `push` / `replace` /
//!   `current`.
//! * [`use_route`] — reactive current path.
//! * [`use_route_params`] / [`use_route_param`] — dynamic / catch-all params.
//!
//! `app/` route naming convention (kept inside the owning application, e.g.
//! `examples/control-center/app/`; `app_router!()` resolves it relative to the
//! invoking source file):
//!
//! ```text
//! app/
//!   layout.rs                 -> root layout (fn Layout(children: Element))
//!   page.rs                   -> the `/` page
//!   about.rs                  -> `/about`
//!   settings/
//!     layout.rs               -> nested layout applied under /settings
//!     page.rs                 -> `/settings`
//!     [section]/page.rs       -> `/settings/:section`
//!   docs/[...chapter]/page.rs -> `/docs/*`
//!   (admin)/layout.rs         -> group layout: wraps only the (admin) routes
//!   (admin)/dashboard.rs      -> `/dashboard` (route group: no path segment)
//!   not_found.rs              -> fallback page
//!   loading.rs                -> Suspense fallback (inert without async routes)
//!   error.rs                  -> ErrorBoundary fallback
//! ```

pub mod core;
pub mod hooks;

pub use core::{split_path, Navigation, Navigator, RouteParams};
pub use hooks::{use_navigate, use_route, use_route_param, use_route_params};

/// Re-export of the build-time routing macro (defined in `touchbard-macros`).
/// An *expression* macro: expands to the generated root router component.
pub use touchbard_macros::app_router;
