# Touchbard

A single-window UI runtime for a Touch Bar / hyper-strip display, plus the
**Control Center** application that runs on it.

The pipeline renders a Dioxus UI through Blitz (DOM/layout/style), AnyRender and
Vello CPU into a premultiplied RGBA framebuffer shared by every display backend:

```text
Dioxus VirtualDom
    → Blitz DOM/layout/style (dioxus-native-dom + blitz-paint)
    → AnyRender scene
    → Vello CPU rasterization
    → Frame (premultiplied RGBA8 framebuffer)
    → backend (WebSocket preview, DRM/KMS)
```

## Quick start

```text
cargo run --example control-center -- --preview    # browser preview (default)
cargo run --example control-center -- --drm        # physical Touch Bar
```

The example is deliberately thin: an app component, CLI parsing, backend
construction, and one call to the public entry point:

```rust
let backend: Box<dyn touchbard::Backend> = Box::new(PreviewBackend::from_env());
touchbard::run(app_router!(), TouchbardConfig { backend })
```

## Public configuration model

```rust
pub struct TouchbardConfig { pub backend: Box<dyn Backend> }

// Backend is a trait in touchbard-renderer; the example wires the concrete
// backend in the entry crate (so the runtime crate stays backend-agnostic).
```

### `PreviewConfig` (touchbard-preview)

Owns everything the preview backend needs; there is no flat mega-config.

| field          | default           | env override     |
|----------------|-------------------|------------------|
| `width`        | `2008` (physical px) | `TOUCHBARD_WIDTH` |
| `height`       | `60`  (physical px)  | `TOUCHBARD_HEIGHT`|
| `scale_factor` | `2.0`             | `TOUCHBARD_SCALE` |
| `bind_addr`    | `127.0.0.1:8888`  | `TOUCHBARD_BIND`  |

`PreviewConfig::from_env()` applies the environment overrides. A thin wrapper
struct `PreviewBackend` implements the shared [`Backend`] trait so it can be
boxed and handed to `touchbard::run`; its initialization returns exactly the
configured/default viewport (2008×60 @ 2.0 by default).

### `DrmConfig` / `DrmBackend` (touchbard-drm)

No width/height/scale/bind: DRM dimensions come from the connected display
connector at runtime (2008×60 landscape on the Touch Bar, with the physical
panel transpose handled internally). `DrmBackend` implements [`Backend`]:
`initialize` runs discovery → open → resources → modeset and returns the
physical viewport, `run` presents CPU-rendered frames through the scanout loop.
`--drm` selects it.

## Backend lifecycle

The runtime performs backend initialization first, then builds the UI system,
then hands control to the backend:

```text
backend.initialize()   →  authoritative Viewport   →  TouchbardSystem  →  backend.run(source)
        (discover/probe/configure the display)      (created at that viewport)  (presents frames + input)
```

`Backend::initialize` may fail cleanly; the runtime reports that as a
backend-initialization error and never creates a UI system or fabricate a
viewport.

## Architecture and dependency direction

```text
control-center (example/entry)
  ├→ touchbard               → touchbard-renderer, touchbard-macros
  ├→ touchbard-preview       → touchbard-renderer
  └→ touchbard-drm           → touchbard-renderer
```

The core runtime crate (`touchbard`) depends **only** on `touchbard-renderer`
(and the macros crate). It does **not** depend on `touchbard-preview` or
`touchbard-drm`. The concrete backends depend only on `touchbard-renderer` for
the shared boundary (`Frame`, `PointerEvent`, `Viewport`, `FrameSource`,
`Backend`), and the entry crate (`control-center`) is where they are wired up
into a `Box<dyn Backend>` and handed to the runtime.

The two key boundary traits live in `touchbard-renderer`:

- **`FrameSource`** (the app/runtime side): `handle_pointer_event`, `frame`,
  `needs_redraw`. Implemented by `TouchbardSystem` and consumed by backends.
  It is exclusively about the rendered UI/frame side - it does not expose
  physical display properties. Frame production is event-driven: `frame` returns
  `Some(Frame)` only when the document changed, requested a redraw, or is
  animating; a backend blocks on its own wake (DRM eventfd / preview async
  select) while `None`, and bounds its wait (~60 Hz upper bound) only while
  `needs_redraw` is true. A backend that arms its host waker is guaranteed at
  least one frame (its initial present), even if the runtime already pre-rendered
  a frame with no waker armed.
- **`Backend`** (the output/display side): `initialize` → [`Viewport`], `run`.
  Implemented by `PreviewBackend` and `DrmBackend` and consumed by the runtime.

The [`Viewport`] returned by `Backend::initialize` is the single authoritative
viewport: the runtime creates `TouchbardSystem` at exactly those dimensions and
no other accessor exposes a copy.

## Workspace layout

| package              | responsibility                                                                 |
|----------------------|---------------------------------------------------------------------------------|
| `control-center`     | Workspace root; `examples/control-center/main.rs` is the thin application       |
| `touchbard`           | Public API (`run`, config, system), Dioxus+Blitz document, input dispatch       |
| `touchbard-renderer`  | Boundary types `Frame`/`PointerEvent`, `FrameSource`/`Backend` traits, CPU renderer |
| `touchbard-preview`   | [`PreviewBackend`], WebSocket server, `PreviewConfig`, binary protocol (v1)     |
| `touchbard-drm`       | [`DrmBackend`]: discovery, USB preparation, KMS resources, modeset, CPU scanout |

The whole pipeline is single-threaded (Blitz documents are not `Send`). Each
backend owns its own runtime: the preview backend builds a current-thread Tokio
runtime + `LocalSet` inside its own `Backend::run` method. The core runtime
(`touchbard::run`) initializes the backend (getting the authoritative viewport),
creates the `TouchbardSystem` at that viewport, renders one frame up front so the
initial render log carries real data, then hands control to the backend.

## Frame pixel format

The canonical framebuffer format is **premultiplied RGBA8** (R,G,B,A byte order).
`vello_cpu`'s `render_to_buffer` writes premultiplied alpha, and the bytes flow
into `Frame.data` untouched. A backend that needs a different format must convert
at its own boundary (e.g. the preview browser un-premultiplies because
`canvas.putImageData` requires straight alpha).

## File-based routing (`touchbard::routing`)

The `control-center` example is a **file-based app**: route components live in
`examples/control-center/app/`, next to the entry point that owns them, and are
wired up at compile time by the `app_router!()` proc-macro. The macro resolves
the tree relative to the invoking source file (`examples/<bin>/app` for example
targets, `<manifest>/app` otherwise), so framework crates carry no app source.

```text
examples/control-center/
├── main.rs
└── app/
    ├── layout.rs                # root layout (nav strip, persists across navigation)
    ├── page.rs                  # pages: page.rs = index, <name>.rs = named route
    ├── about.rs
    ├── settings/
    │   ├── layout.rs            # nested layout for /settings/*
    │   ├── page.rs              # /settings
    │   └── [section]/page.rs    # dynamic segment        →  /settings/:section
    ├── docs/[...chapter]/page.rs# catch-all segments     →  /docs/*/...
    ├── (admin)/dashboard.rs     # route group (flattened)→  /dashboard
    ├── not_found.rs             # 404 fallback (root)
    └── error.rs                 # error-boundary fallback (root)
```

The generated `Router` component provides `Navigation` (via `provide_context`)
and renders layout pages around the matching page. Hooks:

| hook                 | purpose                                              |
|----------------------|------------------------------------------------------|
| `use_navigate()`     | returns a `Navigation` handle; `push(&str)` nav       |
| `use_route()`        | current route string e.g. `"/settings/audio"`        |
| `use_route_param(n)` | dynamic segment at index `n` (from `[section]`)      |

Semantics implemented: `page.rs` index, `layout.rs` nested layouts, `[id]`
single/inner dynamic segments, `[...slug]` catch-all (leaf only), `(group)` route
groups, and root-level `not_found.rs` / `error.rs` fallbacks. Not yet: optional
catch-alls (`[[...slug]]`, rejected with a clear error), API routes, metadata.

The example entry is still thin:

```rust
touchbard::run(touchbard::routing::app_router!(), TouchbardConfig { backend })
```

`app_router!()` is an expression macro: it expands to the generated root
router component (a `fn() -> Element`), so it plugs directly into
`touchbard::run` — no separate `Router` symbol to import.

## Preview protocol

`PROTOCOL_VERSION = 1`. `FRAME` messages are binary:
`0x02` + width u32 BE + height u32 BE + premultiplied RGBA8. Text messages:
`HELLO`/`RESIZE` (JSON), `INPUT` (pointer events in logical/CSS px from the
browser), `PING`/`PONG` keepalive. The browser client lives in `preview/`.

## Tests

```text
cargo test --workspace
```

Covers protocol framing, input conversion, the renderer, framebuffer types, the
routing runtime (`split_path`, route normalization, params), the backend trait
boundary, and a regression suite (click direction for both buttons, framebuffer
diff, and the stale-hover fix: a release without a preceding `pointermove` still
routes to the node under the release point).

End-to-end (live preview): `node /tmp/opencode/e2e_routes.mjs ws://127.0.0.1:8888/ws`
walks the nav strip, verifies the home page's 10 buttons, and confirms
navigation to General/Audio/Docs/Dash/404/Boom each re-renders the strip.
