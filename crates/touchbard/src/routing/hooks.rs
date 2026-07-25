//! Reactive routing hooks for pages and layouts.

use crate::routing::core::{Navigation, Navigator, RouteParams};
use std::str::FromStr;

/// Fetch the navigation context, panicking with a useful message if the hook is
/// used outside the generated router (`app_router!()`).
fn navigation() -> Navigation {
    dioxus::prelude::try_consume_context::<Navigation>().unwrap_or_else(|| {
        panic!(
            "routing hook used outside the app_router!() generated component. \
             Pass `touchbard::routing::app_router!()` straight to `touchbard::run`."
        )
    })
}

/// Returns a [`Navigator`] to push/replace routes.
///
/// ```ignore
/// let navigate = use_navigate();
/// navigate.push("/settings/general");
/// ```
pub fn use_navigate() -> Navigator {
    Navigator {
        path: navigation().path,
    }
}

/// The current route, reactively. The component re-renders when navigation
/// changes the route.
pub fn use_route() -> String {
    (navigation().path)()
}

/// All parameters of the current route (dynamic segments and catch-all values).
pub fn use_route_params() -> RouteParams {
    (navigation().params)()
}

/// Look up a single route parameter and parse it.
///
/// Missing or unparseable values yield `None`.
///
/// ```ignore
/// let section = use_route_param::<String>("section");
/// let id = use_route_param::<u32>("id");
/// ```
pub fn use_route_param<T>(key: &str) -> Option<T>
where
    T: FromStr + 'static,
{
    (navigation().params)().get(key).and_then(|v| v.parse().ok())
}