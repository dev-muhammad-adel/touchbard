//! Core routing state: navigation signal pair, navigator, route params and
//! path utilities.

use dioxus::prelude::{ReadableExt, Signal, WritableExt};
use std::collections::HashMap;

/// The navigation state provided by the generated [`Router`](crate::routing::app_router)
/// through Dioxus context.
///
/// Holding both signals in one context allows the generated `Router` to expose
/// the current path and the (matching-derived) params, while pages and layouts
/// use the reactive hooks in [`crate::routing::hooks`] to subscribe to them.
#[derive(Clone, Copy)]
pub struct Navigation {
    /// Current route, e.g. `"/settings/general"`.
    pub path: Signal<String>,
    /// Params extracted from the current route.
    pub params: Signal<RouteParams>,
}

/// A handle to push/replace navigation and read the current route.
///
/// Obtained with [`use_navigate`](crate::routing::hooks::use_navigate).
#[derive(Clone, Copy)]
pub struct Navigator {
    pub(crate) path: Signal<String>,
}

impl Navigator {
    /// Navigate to `to`, updating the current route.
    pub fn push(&self, to: &str) {
        self.set_path(to);
    }

    /// Same as [`Navigator::push`]. A single-view strip keeps no history stack,
    /// so replace is equivalent; both are provided for API completeness.
    pub fn replace(&self, to: &str) {
        self.set_path(to);
    }

    /// The current route as a string.
    pub fn current(&self) -> String {
        self.path.peek().clone()
    }

    fn set_path(&self, to: &str) {
        let mut p = self.path;
        p.set(normalize_route(to));
    }
}

/// Normalize a raw navigation target into a well-formed absolute route
/// (leading `/`, no empty segments).
pub fn normalize_route(to: &str) -> String {
    let trimmed = to.trim();
    if trimmed.is_empty() {
        return "/".to_string();
    }
    let absolute = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };
    let parts: Vec<&str> = absolute.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

/// Split an absolute route into its path segments.
///
/// * `"/"`, `""`           → `[]`
/// * `"/users"`            → `["users"]`
/// * `"/docs/a/b"`         → `["docs", "a", "b"]`
pub fn split_path(path: &str) -> Vec<&str> {
    let t = path.trim();
    if t.is_empty() || t == "/" {
        return Vec::new();
    }
    t.trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect()
}

/// Route parameters extracted by the generated matcher: dynamic `[id]`
/// segments and catch-all `[...slug]` values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RouteParams {
    inner: HashMap<String, String>,
}

impl RouteParams {
    /// Insert a parameter (used by the generated matching code).
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.inner.insert(key.into(), value.into());
    }

    /// Look up a parameter by its segment name.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.inner.get(key).map(String::as_str)
    }

    /// Whether any parameter is present.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Number of parameters.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Iterate over `(name, value)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.inner.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

impl From<HashMap<String, String>> for RouteParams {
    fn from(inner: HashMap<String, String>) -> Self {
        Self { inner }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_path_basics() {
        assert_eq!(split_path("/"), Vec::<&str>::new());
        assert_eq!(split_path(""), Vec::<&str>::new());
        assert_eq!(split_path("  "), Vec::<&str>::new());
        assert_eq!(split_path("/users"), vec!["users"]);
        assert_eq!(split_path("/users/42"), vec!["users", "42"]);
        assert_eq!(split_path("/docs/a/b/c"), vec!["docs", "a", "b", "c"]);
        assert_eq!(split_path("//a//b//"), vec!["a", "b"]);
    }

    #[test]
    fn normalize_route_basics() {
        assert_eq!(normalize_route(""), "/");
        assert_eq!(normalize_route("/"), "/");
        assert_eq!(normalize_route("settings"), "/settings");
        assert_eq!(normalize_route("/settings"), "/settings");
        assert_eq!(normalize_route("/settings/"), "/settings");
        assert_eq!(normalize_route("//a//b//"), "/a/b");
    }

    #[test]
    fn route_params_roundtrip() {
        let mut p = RouteParams::default();
        assert!(p.is_empty());
        p.insert("id", "42");
        p.insert("slug", "a/b/c");
        assert_eq!(p.len(), 2);
        assert_eq!(p.get("id"), Some("42"));
        assert_eq!(p.get("slug"), Some("a/b/c"));
        assert_eq!(p.get("nope"), None);
        let collected: Vec<_> = p.iter().collect();
        assert!(collected.contains(&("id", "42")));
        assert!(collected.contains(&("slug", "a/b/c")));
    }
}
