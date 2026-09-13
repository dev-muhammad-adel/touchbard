# Touch UI

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
    → backend (WebSocket preview now, DRM/KMS later)
```

## Quick start

```text
cargo run --example control-center -- --preview    # browser preview (default)
cargo run --example control-center -- --drm        # not implemented yet
```

The example is deliberately thin: an app component, CLI parsing, backend
construction, and one call to the public entry point:

```rust
let backend: Box<dyn touch_ui::Backend> = Box::new(PreviewBackend::from_env());
touch_ui::run(app_router!(), TouchUiConfig { backend })
```

## Public configuration model

```rust
pub struct TouchUiConfig { pub backend: Box<dyn Backend> }

// Backend is a trait in touch-ui-renderer; the example wires the concrete
// backend in the entry crate (so the runtime crate stays backend-agnostic).
```

### `PreviewConfig` (touch-ui-preview)

Owns everything the preview backend needs; there is no flat mega-config.

| field          | default           | env override     |
|----------------|-------------------|------------------|
| `width`        | `2008` (physical px) | `TOUCH_UI_WIDTH` |
| `height`       | `60`  (physical px)  | `TOUCH_UI_HEIGHT`|
| `scale_factor` | `2.0`             | `TOUCH_UI_SCALE` |
| `bind_addr`    | `127.0.0.1:8888`  | `TOUCH_UI_BIND`  |

`PreviewConfig::from_env()` applies the environment overrides. A thin wrapper
struct `PreviewBackend` implements the shared [`Backend`] trait so it can be
boxed and handed to `touch_ui::run`; its initialization returns exactly the
configured/default viewport (2008×60 @ 2.0 by default).

### `DrmConfig` / `DrmBackend` (touch-ui-drm)

Deliberately minimal: no width/height/scale/bind, because DRM dimensions will
come from the display connector at runtime. `DrmBackend` implements
[`Backend`] and its `initialize` always returns a clean "not implemented"
error (no fake viewport).

## Backend lifecycle

The runtime performs backend initialization first, then builds the UI system,
then hands control to the backend:

```text
backend.initialize()   →  authoritative Viewport   →  TouchUiSystem  →  backend.run(source)
        (discover/probe/configure the display)      (created at that viewport)  (presents frames + input)
```

`Backend::initialize` may fail cleanly (e.g. DRM not implemented yet); the
runtime reports that as a backend-initialization error and never creates a UI
system or fabricate a viewport.

## Architecture and dependency direction

```text
control-center (example/entry)
  ├→ touch-ui               → touch-ui-renderer, touch-ui-macros
  ├→ touch-ui-preview       → touch-ui-renderer
  └→ touch-ui-drm           → touch-ui-renderer
```

The core runtime crate (`touch-ui`) depends **only** on `touch-ui-renderer`
(and the macros crate). It does **not** depend on `touch-ui-preview` or
`touch-ui-drm`. The concrete backends depend only on `touch-ui-renderer` for
the shared boundary (`Frame`, `PointerEvent`, `Viewport`, `FrameSource`,
`Backend`), and the entry crate (`control-center`) is where they are wired up
into a `Box<dyn Backend>` and handed to the runtime.

The two key boundary traits live in `touch-ui-renderer`:

- **`FrameSource`** (the app/runtime side): `handle_pointer_event`,
  `poll_and_render`. Implemented by `TouchUiSystem` and consumed by backends.
  It is exclusively about the rendered UI/frame side - it does not expose
  physical display properties.
- **`Backend`** (the output/display side): `initialize` → [`Viewport`], `run`.
  Implemented by `PreviewBackend` and `DrmBackend` and consumed by the runtime.

The [`Viewport`] returned by `Backend::initialize` is the single authoritative
viewport: the runtime creates `TouchUiSystem` at exactly those dimensions and
no other accessor exposes a copy.

## Workspace layout

| package              | responsibility                                                                 |
|----------------------|---------------------------------------------------------------------------------|
| `control-center`     | Workspace root; `examples/control-center/main.rs` is the thin application       |
| `touch-ui`           | Public API (`run`, config, system), Dioxus+Blitz document, input dispatch       |
| `touch-ui-renderer`  | Boundary types `Frame`/`PointerEvent`, `FrameSource`/`Backend` traits, CPU renderer |
| `touch-ui-preview`   | [`PreviewBackend`], WebSocket server, `PreviewConfig`, binary protocol (v1)     |
| `touch-ui-drm`       | [`DrmBackend`] scaffold + `DrmConfig` only                                     |

The whole pipeline is single-threaded (Blitz documents are not `Send`). Each
backend owns its own runtime: the preview backend builds a current-thread Tokio
runtime + `LocalSet` inside its own `Backend::run` method. The core runtime
(`touch_ui::run`) initializes the backend (getting the authoritative viewport),
creates the `TouchUiSystem` at that viewport, renders once to prove the
pipeline works, then hands control to the backend.

## Frame pixel format

The canonical framebuffer format is **premultiplied RGBA8** (R,G,B,A byte order).
`vello_cpu`'s `render_to_buffer` writes premultiplied alpha, and the bytes flow
into `Frame.data` untouched. A backend that needs a different format must convert
at its own boundary (e.g. the preview browser un-premultiplies because
`canvas.putImageData` requires straight alpha).

## File-based routing (`touch_ui::routing`)

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
touch_ui::run(touch_ui::routing::app_router!(), TouchUiConfig { backend })
```

`app_router!()` is an expression macro: it expands to the generated root
router component (a `fn() -> Element`), so it plugs directly into
`touch_ui::run` — no separate `Router` symbol to import.

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
