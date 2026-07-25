//! `app_router!()` — build-time, file-based routing for Touchbard.
//!
//! This is an *expression* macro: invoking it inside the app source file (e.g.
//! `examples/control-center/main.rs`) expands to the generated root Dioxus
//! component (`fn() -> Element`), ready to hand straight to [`touchbard::run`]:
//!
//! ```text
//! touchbard::run(
//!     touchbard::routing::app_router!(),        // discovers the `app/` tree next to the source file
//!     TouchbardConfig { backend },
//! )
//! touchbard::run(
//!     touchbard::routing::app_router!("app"),   // explicit path (relative to the source file)
//!     TouchbardConfig { backend },
//! )
//! ```
//!
//! The route tree is resolved relative to the *invoking source file*. For an
//! example target the app directory is `<manifest>/examples/<bin>/app`; for a
//! plain bin/lib crate it is `<manifest>/app`. This keeps each application
//! self-contained next to its own entry point while leaving the framework crates
//! free of app source.
//!
//! It walks the `app/` directory at macro-expansion (compile) time and
//! expands to a block expression containing:
//!
//! * a private module that `include!`s every page/layout source file,
//! * an exhaustive route matching function(s) with static > dynamic > catch-all
//!   precedence,
//! * a `Router()` component that renders the matched page wrapped in its
//!   ancestor layouts, an [`dioxus::prelude::ErrorBoundary`] and — when the app
//!   provides `loading.rs` — a [`dioxus::prelude::SuspenseBoundary`],
//! * a trailing reference to that private component, making the whole macro
//!   expansion evaluate to the root router function.
//!
//! The expansion does **not** declare a caller-visible `Router` symbol: the
//! generated component is referenced as the expression value itself, so apps
//! pass the macro directly to [`touchbard::run`].
//!
//! Route groups `(name)` follow the Next.js App Router semantics: they add a
//! directory but no URL segment. A group's `layout.rs` acts like a normal
//! directory layout (wrapping only routes under that group) except the group
//! never appears in the URL; groups nest and multiple groups may share one URL
//! level. The generated matcher hoists every group's routes into its host
//! level's match function, keeping static > dynamic > catch-all precedence, and
//! build-time duplicate detection rejects two groups resolving to the same URL.
//!
//! The generated code only contains plain Rust + `include!`; no dynamic
//! loading, evaluation, or interpretation of app source happens. The route
//! *engine* (navigation state, hooks, path splitting, param storage) lives in
//! [`touchbard::routing`].

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as Tokens2};
use quote::{format_ident, quote};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// File-system model
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum SegKind {
    Static, // plain segment, e.g. `about`
    Cap,    // route group `(admin)`, contributes no path segment
    Dyn,    // dynamic `[id]`
    Catch,  // catch-all `[...slug]`
}

#[derive(Default)]
struct Node {
    /// Module identifiers from the generated root down to this directory.
    module_path: Vec<String>,
    layout_rs: Option<PathBuf>,
    page_rs: Option<PathBuf>,
    leaf_rs: Option<PathBuf>, // a static `<name>.rs` page living directly in this module
    not_found_rs: Option<PathBuf>,
    loading_rs: Option<PathBuf>,
    error_rs: Option<PathBuf>,
    /// RouteId of this node's own page (filled during route collection).
    own_route_id: Option<proc_macro2::Ident>,
    /// Static children keyed by their URL segment (dirs and `<name>.rs` files).
    statics: BTreeMap<String, Node>,
    /// The (single) dynamic child `[id]`: (param key, node).
    dyns: Vec<(String, Node)>,
    /// The (single) catch-all child `[...slug]`: (param key, node).
    catches: Vec<(String, Node)>,
    /// Route groups `(name)` — transparent to the URL, in filesystem order.
    groups: Vec<(String, Node)>,
}

// ---------------------------------------------------------------------------
// Name helpers
// ---------------------------------------------------------------------------

fn is_keyword(s: &str) -> bool {
    matches!(
        s,
        "as" | "break"
            | "const"
            | "continue"
            | "crate"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "async"
            | "await"
    )
}

/// Validate that `s` can be a plain module identifier.
fn module_ident(s: &str, ctx: &str) -> Result<String, String> {
    if s.is_empty() {
        return Err(format!("{ctx}: empty name"));
    }
    if is_keyword(s) {
        return Err(format!("{ctx}: `{s}` is a Rust keyword, not a usable module name"));
    }
    for (i, c) in s.chars().enumerate() {
        if i == 0 {
            if !(c == '_' || c.is_ascii_alphabetic()) {
                return Err(format!(
                    "{ctx}: `{s}` is not a valid Rust identifier (must start with a letter or `_`)"
                ));
            }
        } else if !(c == '_' || c.is_ascii_alphanumeric()) {
            return Err(format!(
                "{ctx}: `{s}` is not a valid Rust identifier (use only a-z, A-Z, 0-9, `_`)"
            ));
        }
    }
    Ok(s.to_string())
}

/// Turn a dynamic/catch/group name into the `__prefix_name` module identifier.
fn prefixed_ident(prefix: &str, raw: &str) -> String {
    let mut out = String::from(prefix);
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.len() == prefix.len() {
        out.push('x');
    }
    out
}

fn pascal(s: &str) -> String {
    let mut out = String::new();
    let mut cap = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if cap {
                out.push(c.to_ascii_uppercase());
                cap = false;
            } else {
                out.push(c);
            }
        } else {
            cap = true;
        }
    }
    out
}

/// Re-derive the (kind, name) pattern from a module path. Synthetic trailing
/// modules (`page`, `layout`, `not_found`, `loading`, `error`) are ignored.
fn path_pattern(module_path: &[String]) -> Vec<(SegKind, String)> {
    let mut out = Vec::new();
    for m in module_path {
        if let Some(rest) = m.strip_prefix("__param_") {
            out.push((SegKind::Dyn, rest.to_string()));
        } else if let Some(rest) = m.strip_prefix("__catch_") {
            out.push((SegKind::Catch, rest.to_string()));
        } else if let Some(rest) = m.strip_prefix("__group_") {
            out.push((SegKind::Cap, rest.to_string()));
        } else if matches!(
            m.as_str(),
            "page" | "layout" | "not_found" | "loading" | "error"
        ) {
            // synthetic module hosting a route file, skip
        } else {
            out.push((SegKind::Static, m.clone()));
        }
    }
    out
}

fn _route_id_from_pattern(pattern: &[(SegKind, String)]) -> proc_macro2::Ident {
    if pattern.is_empty() {
        return format_ident!("HomePage");
    }
    let mut name = String::new();
    for (kind, s) in pattern {
        name.push_str(&pascal(s));
        match kind {
            SegKind::Dyn => name.push_str("Param"),
            SegKind::Catch => name.push_str("Catch"),
            _ => {}
        }
    }
    name.push_str("Page");
    format_ident!("{}", name)
}

/// Human-readable URL of a module path (route groups are transparent).
fn url_label(module_path: &[String]) -> String {
    let mut segs = Vec::new();
    for m in module_path {
        if let Some(rest) = m.strip_prefix("__group_") {
            let _ = rest; // transparent, no segment
        } else if let Some(rest) = m.strip_prefix("__param_") {
            segs.push(format!(":{rest}"));
        } else if let Some(rest) = m.strip_prefix("__catch_") {
            segs.push(format!("[...{rest}]"));
        } else {
            segs.push(m.clone());
        }
    }
    if segs.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segs.join("/"))
    }
}

/// URL-space claims at one level of the path tree. Route groups are transparent
/// (they add no segment), so every `(group)` at a given level shares this state
/// with the directory that hosts them — which is what lets us reject two
/// siblings that would map to the same URL after stripping group segments.
struct UrlLevel {
    /// Static segment -> file/dir path that claimed it (for error messages).
    statics: BTreeMap<String, PathBuf>,
    /// The `page.rs` (if any) claiming this level's own URL, and its path.
    page: Option<PathBuf>,
    /// The dynamic segment `[param]` claiming this level's fallback.
    dyn_param: Option<(String, PathBuf)>,
    /// The catch-all `[...param]` claiming this level's fallback.
    catch_param: Option<(String, PathBuf)>,
}

impl Default for UrlLevel {
    fn default() -> Self {
        Self {
            statics: BTreeMap::new(),
            page: None,
            dyn_param: None,
            catch_param: None,
        }
    }
}

/// Join a URL prefix with one more segment (`"/"` + `x` → `"/x"`).
fn join_url(base: &str, seg: &str) -> String {
    if base == "/" {
        format!("/{seg}")
    } else {
        format!("{base}/{seg}")
    }
}

/// Claim a static URL segment at this level; error if a transparent group
/// already claimed the same segment.
fn claim_static(url: &str, level: &mut UrlLevel, seg: &str, path: &Path) -> Result<(), String> {
    let full = join_url(url, seg);
    if let Some(prev) = level.statics.insert(seg.to_string(), path.to_path_buf()) {
        return Err(format!(
            "duplicate route URL `{full}`: `{}` and `{}` both map to the same route after removing route groups",
            prev.display(),
            path.display()
        ));
    }
    Ok(())
}

/// Claim the page slot of a URL level; error when a transparent sibling group
/// has already claimed it (both would resolve to the same URL).
fn claim_page(node: &Node, level: &mut UrlLevel, path: &Path) -> Result<(), String> {
    let url = url_label(&node.module_path);
    if let Some(prev) = level.page.as_ref() {
        return Err(format!(
            "duplicate route URL `{url}`: `{}` and `{}` both define the page for `{url}` after removing route groups",
            prev.display(),
            path.display()
        ));
    }
    level.page = Some(path.to_path_buf());
    Ok(())
}

// ---------------------------------------------------------------------------
// Build the file-system tree
// ---------------------------------------------------------------------------

enum Classified {
    Static,
    Dyn(String),
    Catch(String),
    Cap(String),
    OptionalCatch(String),
}

fn classify_dir(name: &str) -> Result<Classified, String> {
    if name.starts_with("[[") && name.ends_with("]]") {
        return Ok(Classified::OptionalCatch(name[2..name.len() - 2].to_string()));
    }
    if name.starts_with('[') && name.ends_with(']') {
        let inner = &name[1..name.len() - 1];
        if let Some(rest) = inner.strip_prefix("...") {
            if rest.is_empty() {
                return Err(format!("`{name}`: catch-all needs a name ([...slug])"));
            }
            Ok(Classified::Catch(rest.to_string()))
        } else {
            if inner.is_empty() {
                return Err(format!("`{name}`: dynamic segment needs a name ([id])"));
            }
            Ok(Classified::Dyn(inner.to_string()))
        }
    } else if name.starts_with('(') && name.ends_with(')') {
        let inner = &name[1..name.len() - 1];
        if inner.is_empty() {
            return Err(format!("`{name}`: route group needs a name ((auth))"));
        }
        Ok(Classified::Cap(inner.to_string()))
    } else if name.contains('[') || name.contains(']') || name.contains('(') || name.contains(')') {
        Err(format!("`{name}`: unsupported segment naming"))
    } else {
        Ok(Classified::Static)
    }
}

fn read_entries(dir: &Path) -> Result<Vec<(String, bool)>, String> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry
            .file_type()
            .map_err(|e| format!("cannot stat {name}: {e}"))?
            .is_dir();
        entries.push((name, is_dir));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

/// Build the whole tree for `dir` into `node`.
///
/// `level` tracks URL-space claims for this level of the path tree. Route
/// groups share their host's level (they are transparent to the URL), while
/// every static / dynamic / catch-all child starts a fresh level for its own
/// contents.
fn build_dir(dir: &Path, node: &mut Node, level: &mut UrlLevel) -> Result<(), String> {
    let entries = read_entries(dir)?;
    let mut seen: HashSet<String> = HashSet::new();
    // URL of this directory, for error messages ("/about", "/").
    let url = url_label(&node.module_path);

    for (name, is_dir_entry) in entries {
        let path = dir.join(&name);
        if !is_dir_entry {
            match name.as_str() {
                "layout.rs" | "page.rs" => {
                    if name == "layout.rs" {
                        set_once(&mut node.layout_rs, path, &name)?;
                    } else {
                        set_once(&mut node.page_rs, path.clone(), &name)?;
                        claim_page(node, level, &path)?;
                    }
                }
                "not_found.rs" | "loading.rs" | "error.rs" => {
                    if !node.module_path.is_empty() {
                        return Err(format!(
                            "`{name}` is only supported at the app root (found in `{}`)",
                            dir.display()
                        ));
                    }
                    let slot = if name == "not_found.rs" {
                        &mut node.not_found_rs
                    } else if name == "loading.rs" {
                        &mut node.loading_rs
                    } else {
                        &mut node.error_rs
                    };
                    set_once(slot, path, &name)?;
                }
                other if other.ends_with(".rs") => {
                    let stem = &other[..other.len() - 3];
                    if stem.contains('[') || stem.contains(']') || stem.contains('(') {
                        return Err(format!(
                            "`{name}`: dynamic / catch-all / group segments must be directories, not files"
                        ));
                    }
                    module_ident(stem, &format!("file `{}/`{name}`", dir.display()))?;
                    if seen.contains(stem) {
                        return Err(format!(
                            "`{}`: duplicate route module named `{stem}` (a `<name>.rs` file and a `<name>/` directory would both map to `/.../{stem}`, which is ambiguous)",
                            dir.display()
                        ));
                    }
                    seen.insert(stem.to_string());
                    claim_static(&url, level, stem, &path)?;
                    let mut leaf = Node::default();
                    leaf.module_path = node.module_path.clone();
                    leaf.module_path.push(stem.to_string());
                    leaf.leaf_rs = Some(path);
                    node.statics.insert(stem.to_string(), leaf);
                }
                other => {
                    return Err(format!(
                        "`{}` contains unsupported file `{other}` (recognized: page.rs, layout.rs, not_found.rs, loading.rs, error.rs, <name>.rs)",
                        dir.display()
                    ));
                }
            }
        } else {
            match classify_dir(&name)? {
                Classified::OptionalCatch(inner) => {
                    return Err(format!(
                        "`{}`: optional catch-all `[[...{inner}]]` is not supported yet",
                        dir.display()
                    ));
                }
                Classified::Cap(group) => {
                    // Route groups are transparent: they add no URL segment and
                    // share this level, so a group's layout/page/routes fold
                    // into the same URL space as its host directory.
                    let gmod = prefixed_ident("__group_", &group);
                    if seen.contains(&gmod) {
                        return Err(format!(
                            "`{}`: route group module `{gmod}` collides with another entry",
                            dir.display()
                        ));
                    }
                    seen.insert(gmod.clone());
                    let mut gnode = Node::default();
                    gnode.module_path = node.module_path.clone();
                    gnode.module_path.push(gmod);
                    build_dir(&path, &mut gnode, level)?;
                    node.groups.push((group, gnode));
                }
                Classified::Static => {
                    module_ident(&name, &format!("directory `{}/`{name}`", dir.display()))?;
                    if seen.contains(&name) {
                        return Err(format!(
                            "`{}`: duplicate route module named `{name}` (a `<name>.rs` file and a `<name>/` directory would both map to `/.../{name}`, which is ambiguous)",
                            dir.display()
                        ));
                    }
                    seen.insert(name.clone());
                    claim_static(&url, level, &name, &path)?;
                    let child = node.statics.entry(name.clone()).or_default();
                    child.module_path = node.module_path.clone();
                    child.module_path.push(name.clone());
                    let mut child_level = UrlLevel::default();
                    build_dir(&path, child, &mut child_level)?;
                }
                Classified::Dyn(param) => {
                    if let Some((_prev, prev_path)) = &level.dyn_param {
                        let full = join_url(&url, &format!(":{param}"));
                        return Err(format!(
                            "duplicate route URL `{full}`: `{}` and `{}` both define a dynamic segment at `{}` after removing route groups (a route group may still hold at most one `[name]` per level)",
                            prev_path.display(),
                            path.display(),
                            if url == "/" { "the root" } else { &url }
                        ));
                    }
                    level.dyn_param = Some((param.clone(), path.clone()));
                    let mod_name = prefixed_ident("__param_", &param);
                    if seen.contains(&mod_name) {
                        return Err(format!(
                            "`{}`: module name `{mod_name}` collides with another entry",
                            dir.display()
                        ));
                    }
                    seen.insert(mod_name.clone());
                    let mut child = Node::default();
                    child.module_path = node.module_path.clone();
                    child.module_path.push(mod_name);
                    let mut child_level = UrlLevel::default();
                    build_dir(&path, &mut child, &mut child_level)?;
                    node.dyns.push((param, child));
                }
                Classified::Catch(param) => {
                    if let Some((_prev, prev_path)) = &level.catch_param {
                        let full = join_url(&url, &format!("[...{param}]"));
                        return Err(format!(
                            "duplicate route URL `{full}`: `{}` and `{}` both define a catch-all at `{}` after removing route groups (a route group may still hold at most one `[...name]` per level)",
                            prev_path.display(),
                            path.display(),
                            if url == "/" { "the root" } else { &url }
                        ));
                    }
                    level.catch_param = Some((param.clone(), path.clone()));
                    let mod_name = prefixed_ident("__catch_", &param);
                    if seen.contains(&mod_name) {
                        return Err(format!(
                            "`{}`: module name `{mod_name}` collides with another entry",
                            dir.display()
                        ));
                    }
                    seen.insert(mod_name.clone());
                    let mut child = Node::default();
                    child.module_path = node.module_path.clone();
                    child.module_path.push(mod_name);
                    let mut child_level = UrlLevel::default();
                    build_dir(&path, &mut child, &mut child_level)?;
                    node.catches.push((param, child));
                }
            }
        }
    }

    // A catch-all directory must be a leaf: it consumes all remaining path
    // segments, so it may only contain page.rs (and optionally layout.rs).
    for (_, catch) in &node.catches {
        if !catch.statics.is_empty()
            || !catch.dyns.is_empty()
            || !catch.catches.is_empty()
            || !catch.groups.is_empty()
        {
            return Err(format!(
                "`[...]` catch-all directory `{}` must contain only page.rs (and optionally layout.rs)",
                catch.module_path.join("/")
            ));
        }
        if catch.page_rs.is_none() {
            return Err(format!(
                "`[...]` catch-all directory `{}` needs a page.rs",
                catch.module_path.join("/")
            ));
        }
    }

    Ok(())
}

fn set_once(slot: &mut Option<PathBuf>, path: PathBuf, name: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!(
            "duplicate `{name}` in `{}`",
            path.parent().unwrap().display()
        ));
    }
    *slot = Some(path);
    Ok(())
}

// ---------------------------------------------------------------------------
// Route collection
// ---------------------------------------------------------------------------

struct Route {
    id: proc_macro2::Ident,
    page_mods: Vec<String>, // module path to the module containing `Page`
    layouts: Vec<Vec<String>>, // ancestor layout module paths, root -> leaf
}

struct Specials {
    not_found: Option<PathBuf>,
    loading: Option<PathBuf>,
    error: Option<PathBuf>,
}

struct Gen {
    routes: Vec<Route>,
    ids: HashSet<String>,
    mod_paths: Vec<(Vec<String>, PathBuf)>,
    root_layouts: Vec<Vec<String>>,
    specials: Option<Specials>,
    match_fns: Vec<Tokens2>,
}

fn collect(node: &Node, layouts_so_far: &[Vec<String>], gen: &mut Gen) -> Result<(), String> {
    // Layout chain up to and including this node.
    let mut chain = layouts_so_far.to_vec();
    if let Some(lf) = &node.layout_rs {
        let mut lm = node.module_path.clone();
        lm.push("layout".to_string());
        chain.push(lm.clone());
        gen.mod_paths.push((lm, lf.clone()));
    }

    if node.module_path.is_empty() {
        gen.root_layouts = chain.clone();
        gen.specials = Some(Specials {
            not_found: node.not_found_rs.clone(),
            loading: node.loading_rs.clone(),
            error: node.error_rs.clone(),
        });
    }

    // Own page (this directory is itself a page, e.g. app/page.rs).
    if let Some(file) = &node.page_rs {
        let id = node
            .own_route_id
            .clone()
            .expect("own_route_id must be assigned before collect");
        ensure_unique(&id, &node.module_path, gen)?;
        let mut pm = node.module_path.clone();
        pm.push("page".to_string());
        gen.routes.push(Route {
            id: id.clone(),
            page_mods: pm.clone(),
            layouts: chain.clone(),
        });
        gen.mod_paths.push((pm, file.clone()));
    }

    // Static children: leaf pages (with leaf_rs) and directories.
    for (_seg, child) in &node.statics {
        if let Some(file) = &child.leaf_rs {
            let id = child
                .own_route_id
                .clone()
                .expect("leaf page id must be assigned before collect");
            ensure_unique(&id, &child.module_path, gen)?;
            gen.routes.push(Route {
                id: id.clone(),
                page_mods: child.module_path.clone(),
                layouts: chain.clone(),
            });
            // `child.module_path` already names the leaf module for this file
            // (segment stem); register the include at that path.
            gen.mod_paths.push((child.module_path.clone(), file.clone()));
        } else {
            collect(child, &chain, gen)?;
        }
    }
    for (_, child) in &node.dyns {
        collect(child, &chain, gen)?;
    }
    for (_, child) in &node.catches {
        collect(child, &chain, gen)?;
    }
    for (_, group) in &node.groups {
        collect(group, &chain, gen)?;
    }
    Ok(())
}

fn ensure_unique(
    id: &proc_macro2::Ident,
    module_path: &[String],
    gen: &mut Gen,
) -> Result<(), String> {
    let key = id.to_string();
    if !gen.ids.insert(key.clone()) {
        return Err(format!(
            "route id `{key}` collides (route paths resolve to the same id; rename a segment)"
        ));
    }
    let _ = module_path;
    Ok(())
}

// ---------------------------------------------------------------------------
// own_route_id assignment (pre-pass, maps module path -> id)
// ---------------------------------------------------------------------------

fn assign_own_ids(
    node: &mut Node,
    occ: &mut BTreeMap<String, proc_macro2::Ident>,
) -> Result<(), String> {
    if node.page_rs.is_some() || node.leaf_rs.is_some() {
        let pattern = path_pattern(&node.module_path);
        let id = _route_id_from_pattern(&pattern);
        let key = node.module_path.join("/");
        if occ.insert(key, id.clone()).is_some() {
            // two nodes claiming the same module path share the same id; only
            // occurs for genuinely duplicate routes (already rejected above)
        }
        // own page nodes store the id on-page; leaf ids are derived from the
        // module path and stored here.
        if node.page_rs.is_some() {
            node.own_route_id = Some(id);
        } else {
            node.own_route_id = Some(id);
        }
    }
    let mut children: Vec<&mut Node> = Vec::new();
    children.extend(node.statics.values_mut());
    children.extend(node.dyns.iter_mut().map(|(_, n)| n));
    children.extend(node.catches.iter_mut().map(|(_, n)| n));
    children.extend(node.groups.iter_mut().map(|(_, n)| n));
    for c in children {
        assign_own_ids(c, occ)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Match function generation
// ---------------------------------------------------------------------------

fn fn_name(node: &Node) -> proc_macro2::Ident {
    if node.module_path.is_empty() {
        format_ident!("__m_root")
    } else {
        let mut s = String::new();
        let mut first = true;
        for m in &node.module_path {
            if !first {
                s.push('_');
            }
            s.push_str(m);
            first = false;
        }
        format_ident!("__m_{}", s)
    }
}

/// Everything one URL level can match: the transparent unions of this
/// directory with all its (nested) route groups. Groups add no segment, so
/// their statics/pages/dynamics/catch-alls are hoisted into the host's level.
struct View<'a> {
    statics: Vec<(String, &'a Node)>,
    page: Option<&'a Node>,
    dyns: Vec<(String, &'a Node)>,
    catches: Vec<(String, &'a Node)>,
}

fn collect_view<'a>(node: &'a Node, view: &mut View<'a>) {
    for (seg, child) in &node.statics {
        view.statics.push((seg.clone(), child));
    }
    if node.page_rs.is_some() {
        view.page = Some(node);
    }
    for (key, child) in &node.dyns {
        view.dyns.push((key.clone(), child));
    }
    for (key, child) in &node.catches {
        view.catches.push((key.clone(), child));
    }
    for (_, group) in &node.groups {
        collect_view(group, view);
    }
}

/// Emit the match fn for one non-group node of the generated tree. The fn
/// matches `segs.first()` against the node's URL level, which already includes
/// every route group nested in this directory (they are transparent). Static
/// arms keep `static > dynamic > catch-all` precedence.
fn emit_node_fn(node: &Node, gen: &mut Gen) {
    let fname = fn_name(node);
    let mut view = View {
        statics: Vec::new(),
        page: None,
        dyns: Vec::new(),
        catches: Vec::new(),
    };
    collect_view(node, &mut view);

    let mut lits = Vec::new();
    for (seg, child) in view.statics {
        let child_fn = fn_name(child);
        lits.push(quote!(Some(&#seg) => #child_fn(&segs[1..], params),));
    }

    let none_arm = match &view.page {
        Some(page) => {
            let id = page
                .own_route_id
                .clone()
                .unwrap_or_else(|| format_ident!("__NotFound"));
            quote!(None => __RouteId::#id,)
        }
        None => quote!(None => __RouteId::__NotFound,),
    };

    let dyn_child = view.dyns.first();
    let catch_child = view.catches.first();
    let some_arm = match (dyn_child, catch_child) {
        (None, None) => quote!(Some(_) => __RouteId::__NotFound,),
        (Some((key, child)), None) => {
            let child_fn = fn_name(child);
            quote!(
                Some(_) => {
                    params.insert(#key, segs[0].to_owned());
                    #child_fn(&segs[1..], params)
                },
            )
        }
        (Some((key, child)), Some((ckey, _catch))) => {
            let child_fn = fn_name(child);
            let catch_id = _catch
                .own_route_id
                .clone()
                .unwrap_or_else(|| format_ident!("__NotFound"));
            quote!(
                Some(_) => {
                    params.insert(#key, segs[0].to_owned());
                    let r = #child_fn(&segs[1..], params);
                    if r == __RouteId::__NotFound {
                        params.insert(#ckey, segs.join("/"));
                        __RouteId::#catch_id
                    } else {
                        r
                    }
                },
            )
        }
        (None, Some((ckey, catch))) => {
            let catch_id = catch
                .own_route_id
                .clone()
                .unwrap_or_else(|| format_ident!("__NotFound"));
            quote!(
                Some(_) => {
                    params.insert(#ckey, segs.join("/"));
                    __RouteId::#catch_id
                },
            )
        }
    };

    gen.match_fns.push(quote! {
        fn #fname(segs: &[&str], params: &mut touchbard::routing::RouteParams) -> __RouteId {
            match segs.first() {
                #(#lits)*
                #none_arm
                #some_arm
            }
        }
    });
}

/// Emit every match fn reachable under (and including) `node`. Only non-group
/// nodes get a fn: groups are transparent, so the fn for each level hoists all
/// its groups' contents and the groups themselves never dispatch.
fn emit_match_fns(node: &Node, gen: &mut Gen) {
    emit_node_fn(node, gen);
    for child in node.statics.values() {
        emit_match_fns(child, gen);
    }
    for (_, child) in &node.dyns {
        emit_match_fns(child, gen);
    }
    for (_, child) in &node.catches {
        emit_match_fns(child, gen);
    }
    for (_, group) in &node.groups {
        emit_group_subtree(group, gen);
    }
}

/// Emit the subtree of a route group: all its children are part of the host's
/// URL level, but their *own* levels still need dedicated match fns.
fn emit_group_subtree(group: &Node, gen: &mut Gen) {
    for child in group.statics.values() {
        emit_match_fns(child, gen);
    }
    for (_, child) in &group.dyns {
        emit_match_fns(child, gen);
    }
    for (_, child) in &group.catches {
        emit_match_fns(child, gen);
    }
    for (_, nested) in &group.groups {
        emit_group_subtree(nested, gen);
    }
}

// ---------------------------------------------------------------------------
// Module tree emission
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ModTree {
    children: BTreeMap<String, ModTree>,
    files: Vec<(String, PathBuf)>,
}

/// Register a source file as a real sub-module at `path` (the last element of
/// `path` is the module name; the rest are the nesting directories).
fn insert_mod(root: &mut ModTree, path: &[String], file: &Path) {
    let (name, dirs) = path.split_last().expect("module path never empty");
    let mut node = root;
    for p in dirs {
        node = node.children.entry(p.clone()).or_default();
    }
    node.files.push((name.clone(), file.to_path_buf()));
}

/// Emit `mod a { mod b { #[path = "..."] mod page; } }` for every file. Using
/// `#[path]` file modules (instead of `include!`) keeps doc comments and item
/// scoping perfectly normal inside each page/layout source file.
fn emit_mod_tree(root: &ModTree) -> Tokens2 {
    let mut parts = Vec::new();
    for (name, child) in &root.children {
        let ident = proc_macro2::Ident::new(name, Span::call_site());
        let body = emit_mod_tree(child);
        parts.push(quote!(pub mod #ident { #body }));
    }
    for (name, file) in &root.files {
        let ident = proc_macro2::Ident::new(name, Span::call_site());
        let path = file.to_string_lossy().into_owned();
        parts.push(quote! {
            #[path = #path]
            pub mod #ident;
        });
    }
    quote!(#(#parts)*)
}

// ---------------------------------------------------------------------------
// Render chains + Router
// ---------------------------------------------------------------------------

fn join_scope(mods: &[String]) -> String {
    mods.iter().cloned().collect::<Vec<_>>().join("::")
}

fn wrap_layouts(inner: &str, layouts: &[Vec<String>]) -> String {
    let mut out = inner.to_string();
    for lm in layouts.iter().rev() {
        out = format!("{}::Layout {{ {out} }}", join_scope(lm));
    }
    out
}

fn wrap_error_loading(inner: String, specials: &Specials) -> String {
    let mut body = inner;
    if specials.loading.is_some() {
        body = format!(
            "SuspenseBoundary {{ fallback: |_s: SuspenseContext| {{ rsx! {{ loading::Page {{}} }} }}, {body} }}"
        );
    }
    let err_fb = if specials.error.is_some() {
        "rsx! { error::Page {} }".to_string()
    } else {
        "rsx! { div { style: \"padding: 4px 8px; color: #e5534b;\", \"Application error\" } }"
            .to_string()
    };
    format!("ErrorBoundary {{ handle_error: |_e: ErrorContext| {{ {err_fb} }}, {body} }}")
}

fn route_body(route: &Route, specials: &Specials) -> String {
    let page = format!("{}::Page", join_scope(&route.page_mods));
    let inner = wrap_layouts(&format!("{page} {{}}"), &route.layouts);
    wrap_error_loading(inner, specials)
}

fn parse_tokens(s: &str) -> Tokens2 {
    s.parse::<Tokens2>()
        .unwrap_or_else(|e| panic!("generated invalid tokens: {e}"))
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[proc_macro]
pub fn app_router(input: TokenStream) -> TokenStream {
    match build(input) {
        Ok(ts) => ts.into(),
        Err(err) => {
            let msg = err.to_string();
            quote! {
                ::core::compile_error!(#msg);
            }
            .into()
        }
    }
}

/// Candidate base directories for resolving the route tree, closest to the
/// invoking source file first. For an example target (`examples/<bin>/main.rs`)
/// the app directory lives next to that source file; for any other target it is
/// resolved relative to the crate manifest. `<bin>` is taken from the
/// `CARGO_BIN_NAME` env var Cargo sets while compiling the target.
fn candidate_bases(manifest: &Path, bin_name: Option<&str>) -> Vec<PathBuf> {
    let mut bases = Vec::new();
    if let Some(bin) = bin_name.filter(|b| !b.trim().is_empty()) {
        bases.push(manifest.join("examples").join(bin));
    }
    bases.push(manifest.to_path_buf());
    bases
}

/// Resolve `<rel>` relative to the invoking source file: the first existing
/// directory among `<manifest>/examples/<bin>/<rel>` (when `bin_name` is given)
/// and `<manifest>/<rel>` wins.
fn resolve_app_root(
    manifest: &Path,
    bin_name: Option<&str>,
    rel: &str,
) -> Result<PathBuf, String> {
    let candidates: Vec<PathBuf> = candidate_bases(manifest, bin_name)
        .into_iter()
        .map(|base| base.join(rel))
        .collect();
    for candidate in &candidates {
        if candidate.is_dir() {
            return Ok(candidate.clone());
        }
    }
    Err(format!(
        "app_router!: route source directory `{rel}/` not found. \
         Resolved relative to the invoking source file; tried: {}",
        candidates
            .iter()
            .map(|c| format!("`{}`", c.display()))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn build(input: TokenStream) -> Result<Tokens2, String> {
    // Optional single string argument: relative path to the app source tree.
    let rel_arg = if input.is_empty() {
        None
    } else {
        let lit: syn::LitStr = syn::parse(input).map_err(|e| {
            format!(
                "app_router!: expected an optional literal path string, got {e}. \
                 Usage: app_router!(); or app_router!(\"path/to/app\");"
            )
        })?;
        Some(lit.value())
    };
    let rel = rel_arg.unwrap_or_else(|| "app".to_string());

    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .ok_or("cannot determine the crate directory (CARGO_MANIFEST_DIR not set)")?;

    let bin_name = std::env::var("CARGO_BIN_NAME").ok();
    let app_root = resolve_app_root(&manifest, bin_name.as_deref(), &rel)?;

    generate(&app_root)
}

/// Generate the whole routing module for an already-resolved app directory.
/// Kept separate from [`build`] so unit tests can drive it with temporary
/// trees.
fn generate(app_root: &Path) -> Result<Tokens2, String> {
    let mut root = Node::default();
    build_dir(app_root, &mut root, &mut UrlLevel::default())?;

    let mut occ = BTreeMap::new();
    assign_own_ids(&mut root, &mut occ)?;

    let mut gen = Gen {
        routes: Vec::new(),
        ids: HashSet::new(),
        mod_paths: Vec::new(),
        root_layouts: Vec::new(),
        specials: None,
        match_fns: Vec::new(),
    };
    collect(&root, &[], &mut gen)?;
    let specials = gen.specials.take().unwrap_or(Specials {
        not_found: None,
        loading: None,
        error: None,
    });

    // Build the module tree: every page/layout/special file becomes a
    // `#[path]`-anchored sub-module of the routing tree.
    let mut mod_tree = ModTree::default();
    for (path, file) in &gen.mod_paths {
        insert_mod(&mut mod_tree, path, file);
    }
    if let Some(f) = &specials.not_found {
        insert_mod(&mut mod_tree, &["not_found".to_string()], f);
    }
    if let Some(f) = &specials.loading {
        insert_mod(&mut mod_tree, &["loading".to_string()], f);
    }
    if let Some(f) = &specials.error {
        insert_mod(&mut mod_tree, &["error".to_string()], f);
    }

    emit_match_fns(&root, &mut gen);

    // RouteId enum variants.
    let ids: Vec<&proc_macro2::Ident> = gen.routes.iter().map(|r| &r.id).collect();

    // Router arms.
    let mut arms = Vec::new();
    for r in &gen.routes {
        let body = parse_tokens(&format!("rsx! {{ {} }}", route_body(r, &specials)));
        let id = &r.id;
        arms.push(quote!(__RouteId::#id => { #body },));
    }

    // Not-found arm (root fallback, inside the root layout chain).
    let nf_content = if specials.not_found.is_some() {
        "not_found::Page {}".to_string()
    } else {
        "div { style: \"padding: 4px 8px; color: #b4befe;\", \"Page not found\" }".to_string()
    };
    let nf_chain = wrap_layouts(&nf_content, &gen.root_layouts);
    let nf_body = parse_tokens(&format!("rsx! {{ {} }}", wrap_error_loading(nf_chain, &specials)));
    let notfound_arm = quote!(__RouteId::__NotFound => { #nf_body },);

    let mods = emit_mod_tree(&mod_tree);
    let match_fns = &gen.match_fns;

    let output = quote! {
        {
            #[allow(dead_code, unused_imports, non_snake_case, non_upper_case_globals, clippy::all)]
            mod ___touchbard_app_router {
                use dioxus::prelude::*;

                #mods

                #[derive(Debug, Clone, Copy, PartialEq, Eq)]
                enum __RouteId {
                    #(#ids,)*
                    __NotFound,
                }

                #(#match_fns)*

                /// Root router component, wired to `touchbard::routing` navigation state.
                pub fn Router() -> Element {
                    use touchbard::routing::{Navigation, RouteParams};

                    let path = use_signal(|| String::from("/"));
                    let mut params = use_signal(RouteParams::default);
                    provide_context(Navigation { path, params });

                    let current = path();
                    let mut matched = RouteParams::default();
                    let id = __m_root(&touchbard::routing::split_path(&current), &mut matched);
                    if *params.peek() != matched {
                        params.set(matched);
                    }

                    match id {
                        #(#arms)*
                        #notfound_arm
                    }
                }
            }

            ___touchbard_app_router::Router
        }
    };

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch dir for one test run; removed (best-effort) afterwards.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "touchbard_macros_test_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0)
            ));
            fs::create_dir_all(&dir).expect("create scratch dir");
            Scratch(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resolves_relative_to_the_invoking_example() {
        let s = Scratch::new();
        let manifest = s.0.join("pkg");
        let app = manifest.join("examples").join("control-center").join("app");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(manifest.join("app")).unwrap();

        let root = resolve_app_root(&manifest, Some("control-center"), "app").unwrap();
        assert_eq!(root, app, "example app must shadow the manifest-level app");
    }

    #[test]
    fn falls_back_to_manifest_when_example_app_is_absent() {
        let s = Scratch::new();
        let manifest = s.0.join("pkg");
        let app = manifest.join("app");
        fs::create_dir_all(&app).unwrap();

        let root = resolve_app_root(&manifest, Some("control-center"), "app").unwrap();
        assert_eq!(root, app);
    }

    #[test]
    fn resolves_relative_to_manifest_without_a_bin_name() {
        let s = Scratch::new();
        let manifest = s.0.join("pkg");
        let app = manifest.join("app");
        fs::create_dir_all(&app).unwrap();

        let root = resolve_app_root(&manifest, None, "app").unwrap();
        assert_eq!(root, app);
    }

    #[test]
    fn supports_an_explicit_rel_path() {
        let s = Scratch::new();
        let manifest = s.0.join("pkg");
        let routes = manifest.join("examples").join("ctl").join("routes");
        fs::create_dir_all(&routes).unwrap();

        let root = resolve_app_root(&manifest, Some("ctl"), "routes").unwrap();
        assert_eq!(root, routes);
    }

    #[test]
    fn reports_all_tried_locations_on_missing_tree() {
        let s = Scratch::new();
        let manifest = s.0.join("pkg");
        fs::create_dir_all(&manifest).unwrap();

        let err = resolve_app_root(&manifest, Some("ctl"), "app").unwrap_err();
        assert!(err.contains("app/"), "mentions the requested dir: {err}");
        assert!(
            err.contains("examples/ctl/app") && err.contains("/app"),
            "lists every candidate: {err}"
        );
    }

    #[test]
    fn candidate_bases_prefer_the_example_dir() {
        let s = Scratch::new();
        let manifest = s.0.join("pkg");
        let bases = candidate_bases(&manifest, Some("ctl"));
        assert_eq!(bases.len(), 2);
        assert_eq!(bases[0], manifest.join("examples").join("ctl"));
        assert_eq!(bases[1], manifest);

        let no_bin = candidate_bases(&manifest, None);
        assert_eq!(no_bin, vec![manifest.clone()]);
    }

    /// Create an empty file (and its parent dirs) under `root`.
    fn touch(root: &Path, rel: &str) -> PathBuf {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().expect("rel has a parent dir")).unwrap();
        fs::write(&p, "").unwrap();
        p
    }

    fn gen(root: &Path) -> String {
        generate(root).expect("generate should succeed").to_string()
    }

    #[test]
    fn group_without_layout_is_transparent_in_url_matcher() {
        let s = Scratch::new();
        touch(&s.0, "page.rs");
        touch(&s.0, "(admin)/dashboard.rs");

        let out = gen(&s.0);

        // The group's route is hoisted into the level-0 matcher under its own
        // URL segment, with no `(admin)` segment anywhere.
        assert!(out.contains("__m___group_admin_dashboard"), "hoisted leaf fn: {out}");
        assert!(out.contains("\"dashboard\""), "static arm for /dashboard: {out}");
        assert!(!out.contains("\"admin\""), "no arm for the transparent group: {out}");
        assert!(out.contains("AdminDashboardPage"), "id from URL-visible segments: {out}");
    }

    #[test]
    fn group_layout_wraps_only_its_own_routes() {
        let s = Scratch::new();
        touch(&s.0, "layout.rs");
        touch(&s.0, "page.rs");
        touch(&s.0, "(admin)/layout.rs");
        touch(&s.0, "(admin)/dashboard.rs");

        let out = gen(&s.0);
        assert!(out.contains("\"dashboard\""), "dashboard is still a route: {out}");
        assert!(!out.contains("\"admin\""), "no admin URL segment: {out}");

        // Dashboard must be wrapped by the *group* layout, which itself sits
        // inside the root layout: root :: group :: page, in that order.
        let group_wrap = "__group_admin :: layout :: Layout";
        assert!(out.contains(group_wrap), "group layout in generated tree: {out}");
        let inner = format!("{group_wrap} {{ __group_admin :: dashboard :: Page");
        assert!(out.contains(&inner), "group layout wraps only group pages: {out}");
        assert!(
            out.contains("layout :: Layout { __group_admin :: layout :: Layout"),
            "root layout sits outside the group layout: {out}"
        );
    }

    #[test]
    fn nested_route_groups_flatten_into_one_url_level() {
        let s = Scratch::new();
        touch(&s.0, "(a)/(b)/dashboard.rs");

        let out = gen(&s.0);
        assert!(out.contains("__m___group_a___group_b_dashboard"), "nested hoist: {out}");
        assert!(out.contains("\"dashboard\""), "static arm for /dashboard: {out}");
        assert!(!out.contains("\"a\""), "no arm for (a): {out}");
        assert!(!out.contains("\"b\""), "no arm for (b): {out}");
        assert!(out.contains("ABDashboardPage"), "id from the URL-visible segment: {out}");
    }

    #[test]
    fn group_with_dynamic_route() {
        let s = Scratch::new();
        touch(&s.0, "(admin)/[id]/page.rs");

        let out = gen(&s.0);
        assert!(out.contains("__m___group_admin___param_id"), "dynamic inside group: {out}");
        assert!(
            out.contains("\"id\"") && out.contains("segs [0] . to_owned ()"),
            "dynamic param extracted from the first segment: {out}"
        );
        assert!(!out.contains("\"admin\""), "no admin URL segment: {out}");
        assert!(out.contains("AdminIdParamPage"), "id includes the param: {out}");
    }

    #[test]
    fn group_with_catchall_route() {
        let s = Scratch::new();
        touch(&s.0, "(admin)/[...slug]/page.rs");

        let out = gen(&s.0);
        assert!(out.contains("__m___group_admin___catch_slug"), "catch-all inside group: {out}");
        assert!(
            out.contains("\"slug\"") && out.contains("segs . join (\"/\")"),
            "catch-all joined from the remaining segments: {out}"
        );
        assert!(!out.contains("\"admin\""), "no admin URL segment: {out}");
        assert!(out.contains("AdminSlugCatchPage"), "id includes the catch-all: {out}");
    }

    #[test]
    fn duplicate_urls_after_removing_group_segments_are_rejected() {
        let s = Scratch::new();
        // A group page.rs and the host page.rs both resolve to `/`.
        touch(&s.0, "page.rs");
        touch(&s.0, "(g)/page.rs");
        let err = generate(&s.0).unwrap_err();
        assert!(
            err.contains("duplicate route URL `/`") && err.contains("after removing route groups"),
            "group vs host page collision: {err}"
        );

        // Two sibling groups claiming the same leaf segment both map to `/x`.
        let s2 = Scratch::new();
        touch(&s2.0, "(a)/x.rs");
        touch(&s2.0, "(b)/x.rs");
        let err = generate(&s2.0).unwrap_err();
        assert!(
            err.contains("duplicate route URL `/x`") && err.contains("after removing route groups"),
            "group vs group leaf collision: {err}"
        );

        // A dynamic segment is a per-level claim shared with transparent groups.
        let s3 = Scratch::new();
        touch(&s3.0, "[id]/page.rs");
        touch(&s3.0, "(g)/[other]/page.rs");
        let err = generate(&s3.0).unwrap_err();
        assert!(
            err.contains("duplicate route URL") && err.contains("dynamic segment"),
            "group vs host dynamic collision: {err}"
        );
    }

    #[test]
    fn expands_to_router_expression_without_a_caller_visible_router() {
        let s = Scratch::new();
        touch(&s.0, "page.rs");
        touch(&s.0, "(admin)/dashboard.rs");

        let out = gen(&s.0);
        let trimmed = out.trim();

        // Expression macro: the whole expansion is a block whose value is the
        // generated root component (fn item, coercible to `fn() -> Element`).
        assert!(trimmed.starts_with('{'), "expansion is a block expression: {out}");
        assert!(
            trimmed.ends_with("___touchbard_app_router :: Router }"),
            "block tail evaluates to the private router fn: {out}"
        );
        assert!(
            out.contains("mod ___touchbard_app_router"),
            "private module still generated: {out}"
        );
        assert!(
            !out.contains("pub use"),
            "no caller-visible Router re-export is emitted: {out}"
        );
    }
}