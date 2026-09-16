//! Preview server: WebSocket + minimal static file hosting.
//!
//! The Rust side renders real RGBA frames and streams them to browsers over
//! WebSocket. Browsers display them on an HTML canvas and send pointer events
//! back. This server also serves the `preview/index.html` page and
//! `preview/preview.js` so a single TCP port serves everything.
//!
//! This crate is a concrete [`Backend`]: [`PreviewBackend`] implements
//! [`Backend::initialize`] (from [`PreviewConfig`]) and [`Backend::run`],
//! which owns the current-thread Tokio runtime + `LocalSet` and runs the
//! WebSocket server loop.
//!
//! Note on threading: Blitz documents are not `Send` (they hold thread-local
//! font/layout contexts), so the whole pipeline must run on a single thread.
//! Hence `Rc<RefCell<dyn FrameSource>>` and `spawn_local`, driven by a
//! current-thread Tokio runtime + `LocalSet`.
//!
//! Known limitation with multiple connections: every connection drives the one
//! shared runtime, which holds a single host waker — the last connection to
//! arm it wins. Incoming events on one tab while another has the waker armed
//! can wake the wrong connection, delaying that frame until the next wake it
//! does receive (a keepalive tick, the 16 ms animation tick while animating,
//! or its own input). The initial-present guarantee holds for the first
//! connection. Single-tab use is the supported scenario, so the server accepts
//! exactly one live preview client and rejects a second connection gracefully
//! (WebSocket policy-close) instead of sharing the runtime between them.

use crate::protocol::{self, Hello};
use touchbard_renderer::{
    Backend, FrameSource, PointerButton, PointerEvent, PointerEventKind, Viewport,
};

use futures_util::{SinkExt, StreamExt};
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Wake, Waker};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tracing::{info, warn};

/// RAII guard that decrements the active-client count when a connection drops.
struct ActiveClientGuard(Rc<Cell<usize>>);

impl Drop for ActiveClientGuard {
    fn drop(&mut self) {
        self.0.set(0);
    }
}

/// Upper bound on the animation cadence (≈60 Hz): while the runtime reports it
/// is animating, the connection loop asks for a frame every [`ANIM_TICK`]. Not
/// a fixed render rate — an idle document blocks on the WebSocket/wake select.
const ANIM_TICK: Duration = Duration::from_millis(16);

/// The [`Wake`] that completes a connection's [`Notify`]: Dioxus scheduler
/// wakeups and shell redraw requests unblock the connection's select loop so
/// it can ask the runtime whether there is a frame to send.
struct FrameWake {
    notify: Notify,
}

impl Wake for FrameWake {
    fn wake(self: Arc<Self>) {
        self.notify.notify_waiters();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notify.notify_waiters();
    }
}

/// Static files served by the preview HTTP server, keyed by path.
pub const INDEX_HTML: &str = include_str!("../../../preview/index.html");
pub const PREVIEW_JS: &str = include_str!("../../../preview/preview.js");

/// Default framebuffer size (physical pixels) for the preview canvas.
pub const DEFAULT_WIDTH: u32 = 2008;
/// Default framebuffer size (physical pixels) for the preview canvas.
pub const DEFAULT_HEIGHT: u32 = 60;
/// Default bias: the `control-center` pages are authored at 2008×60 logical
/// pixels (1:1 with the Touch Bar's native resolution), so the default is `1.0`.
/// Raise it with `TOUCHBARD_SCALE` for a smaller CSS-pixel grid.
pub const DEFAULT_SCALE: f64 = 1.0;
/// Default bind address for the HTTP/WebSocket server.
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8888";

/// Configuration for the preview backend.
///
/// Owns the framebuffer dimensions and scale used to create the runtime plus
/// the bind address of the preview server. The DRM backend has no such fields;
/// its dimensions will come from the connector at runtime.
#[derive(Debug, Clone)]
pub struct PreviewConfig {
    /// Framebuffer width in physical pixels.
    pub width: u32,
    /// Framebuffer height in physical pixels.
    pub height: u32,
    /// Scale factor (physical pixels per logical pixel).
    pub scale_factor: f64,
    /// Bind address, e.g. `127.0.0.1:8888`.
    pub bind_addr: String,
}

impl PreviewConfig {
    /// Read overrides from `TOUCHBARD_WIDTH`, `TOUCHBARD_HEIGHT`, `TOUCHBARD_SCALE`
    /// (physical pixels / scale factor) and `TOUCHBARD_BIND` (server address);
    /// invalid values fall back to defaults.
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(value) = std::env::var("TOUCHBARD_WIDTH") {
            if let Ok(width) = value.parse() {
                config.width = width;
            }
        }
        if let Ok(value) = std::env::var("TOUCHBARD_HEIGHT") {
            if let Ok(height) = value.parse() {
                config.height = height;
            }
        }
        if let Ok(value) = std::env::var("TOUCHBARD_SCALE") {
            if let Ok(scale) = value.parse() {
                config.scale_factor = scale;
            }
        }
        if let Ok(value) = std::env::var("TOUCHBARD_BIND") {
            if !value.trim().is_empty() {
                config.bind_addr = value.trim().to_string();
            }
        }
        config
    }
}

impl Default for PreviewConfig {
    fn default() -> Self {
        Self {
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            scale_factor: DEFAULT_SCALE,
            bind_addr: DEFAULT_BIND_ADDR.to_string(),
        }
    }
}

/// The browser-preview [`Backend`]: a boxable trait-object implementation that
/// owns a [`PreviewConfig`] and, when driven by the runtime, opens the browser
/// and runs the WebSocket/HTTP preview server.
///
/// There is no stored viewport state: the authoritative viewport is a pure
/// function of the immutable [`PreviewConfig`] ([`preview_viewport`]),
/// produced by [`Backend::initialize`] and re-derived unconditionally
/// inside [`Backend::run`] for the things run needs it for (the info log and
/// the `HELLO` frame size).
pub struct PreviewBackend {
    config: PreviewConfig,
}

impl PreviewBackend {
    /// Build the backend from the default framebuffer size/scale/bind address.
    pub fn new() -> Self {
        Self::with_config(PreviewConfig::default())
    }

    /// Build the backend from environment overrides
    /// (see [`PreviewConfig::from_env`]).
    pub fn from_env() -> Self {
        Self::with_config(PreviewConfig::from_env())
    }

    /// Build the backend from an explicit configuration.
    pub fn with_config(config: PreviewConfig) -> Self {
        Self { config }
    }

    /// The configuration backing this backend.
    pub fn config(&self) -> &PreviewConfig {
        &self.config
    }
}

impl Default for PreviewBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Derive the authoritative [`Viewport`] from a [`PreviewConfig`].
fn preview_viewport(config: &PreviewConfig) -> Viewport {
    Viewport {
        width: config.width,
        height: config.height,
        scale_factor: config.scale_factor,
    }
}

impl Backend for PreviewBackend {
    fn initialize(&mut self) -> Result<Viewport, Box<dyn std::error::Error + Send + Sync>> {
        Ok(preview_viewport(&self.config))
    }

    fn run(
        &mut self,
        source: Rc<RefCell<dyn FrameSource>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let addr = self.config.bind_addr.clone();
        let viewport = preview_viewport(&self.config);
        match open_browser(&addr) {
            Ok(()) => info!(
                "Opening http://{addr} in your browser to preview the Touch Bar UI ({}x{} @ {:.0}x)",
                viewport.width, viewport.height, viewport.scale_factor
            ),
            Err(()) => warn!(
                "Could not open a browser automatically; visit http://{addr} yourself"
            ),
        }

        let local = tokio::task::LocalSet::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        local.block_on(&runtime, serve(self.config.clone(), viewport, source))
    }
}

/// Best-effort: open the browser on the host.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn open_browser(addr: &str) -> Result<(), ()> {
    #[cfg(target_os = "linux")]
    let cmd = format!("xdg-open http://{addr}");
    #[cfg(target_os = "macos")]
    let cmd = format!("open http://{addr}");
    #[cfg(target_os = "windows")]
    let cmd = format!("start http://{addr}");

    use std::process::Command;
    let _ = Command::new("sh").arg("-c").arg(&cmd).spawn();
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn open_browser(_addr: &str) -> Result<(), ()> {
    Err(())
}

/// Run the preview server (async body of [`PreviewBackend::run`]).
///
/// `source` is the shared [`FrameSource`] (`Rc<RefCell<_>>`) and must stay on
/// the calling thread (see module docs). `viewport` is the authoritative
/// viewport produced by [`PreviewBackend::initialize`]; it sizes the `HELLO`
/// the browser receives.
///
/// Call this inside `tokio::task::LocalSet::run_until(...)` on a current-thread
/// runtime (as [`PreviewBackend::run`] does).
async fn serve(
    config: PreviewConfig,
    viewport: Viewport,
    source: Rc<RefCell<dyn FrameSource>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    info!("Preview server listening on http://{}", config.bind_addr);

    // Single-client server: at most one live preview connection at a time (see
    // module docs). Later connections are rejected with a policy close.
    let active_clients = Rc::new(Cell::new(0usize));

    loop {
        let (stream, addr) = listener.accept().await?;
        let source = Rc::clone(&source);
        let active_clients = Rc::clone(&active_clients);
        tokio::task::spawn_local(async move {
            if let Err(e) = handle_connection(stream, viewport, source, active_clients).await {
                warn!("Connection {addr} error: {e}");
            }
        });
    }
}

const MAX_HEADER_SIZE: usize = 16 * 1024;

/// Information extracted from the HTTP request head.
struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(|s| s.as_str())
    }
}

/// Read and parse the HTTP request head (headers only, up to CRLFCRLF).
async fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, std::io::Error> {
    let mut buffer = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];

    loop {
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed during request",
            ));
        }
        buffer.push(byte[0]);

        // Detect the end of the header section (empty line).
        if buffer.len() >= 4 && &buffer[buffer.len() - 4..] == b"\r\n\r\n" {
            break;
        }
        if buffer.len() >= MAX_HEADER_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request headers too large",
            ));
        }
    }

    let head = String::from_utf8_lossy(&buffer);
    let mut lines = head.split("\r\n");

    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    Ok(HttpRequest {
        method,
        path,
        headers,
    })
}

async fn serve_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await
}

/// Compute the `Sec-WebSocket-Accept` value mandated by RFC 6455.
fn websocket_accept(key: &str) -> String {
    use base64::Engine;
    use sha1::Digest;

    const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let mut hasher = sha1::Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(GUID.as_bytes());
    let digest = hasher.finalize().to_vec();
    base64::engine::general_purpose::STANDARD.encode(digest)
}

async fn handle_connection(
    mut stream: TcpStream,
    viewport: Viewport,
    source: Rc<RefCell<dyn FrameSource>>,
    active_clients: Rc<Cell<usize>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    stream.set_nodelay(true).ok();

    let request = read_http_request(&mut stream).await?;

    if request.method != "GET" {
        serve_response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            "Method Not Allowed",
        )
        .await?;
        return Ok(());
    }

    // Static file serving.
    match request.path.as_str() {
        "/" => {
            serve_response(&mut stream, "200 OK", "text/html", INDEX_HTML).await?;
            return Ok(());
        }
        "/preview.js" => {
            serve_response(&mut stream, "200 OK", "text/javascript", PREVIEW_JS).await?;
            return Ok(());
        }
        "/ws" => {}
        _ => {
            serve_response(&mut stream, "404 Not Found", "text/plain", "Not Found").await?;
            return Ok(());
        }
    }

    // WebSocket upgrade.
    let Some(sec_key) = request.header("sec-websocket-key").map(str::to_string) else {
        serve_response(
            &mut stream,
            "400 Bad Request",
            "text/plain",
            "Missing Sec-WebSocket-Key",
        )
        .await?;
        return Ok(());
    };

    let accept = websocket_accept(&sec_key);
    let response = match request.header("sec-websocket-protocol") {
        Some(proto) if !proto.is_empty() => format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\nSec-WebSocket-Protocol: {proto}\r\n\r\n"
        ),
        _ => format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        ),
    };
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;

    // We read only the HTTP request head (which ends before any WebSocket
    // frames, since the client waits for the 101), so it is safe to wrap the
    // raw socket now.
    let mut ws = WebSocketStream::from_raw_socket(
        stream,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;

    info!("WebSocket connection established");

    // Single-client policy: the shared runtime holds exactly one host waker;
    // a second live connection would steal wakes from the first. Reject extras
    // with a clean protocol-close instead of sharing the runtime.
    if active_clients.get() > 0 {
        info!("Rejecting duplicate preview client (single-client server)");
        let _ = ws
            .send(Message::Close(Some(CloseFrame {
                code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Policy,
                reason: "only one preview client is supported".into(),
            })))
            .await;
        return Ok(());
    }
    active_clients.set(1);
    let _active_guard = ActiveClientGuard(active_clients);

    // Host wake bridge: the runtime registers this waker on the Dioxus
    // scheduler and the shell redraw bridge; either unblocks the select below
    // so it asks the runtime for a frame. Dioxus needs a `&'static` waker, so
    // one is leaked per connection (bounded: connections run for the process
    // lifetime).
    let frame_wake = Arc::new(FrameWake {
        notify: Notify::new(),
    });
    let waker: &'static Waker = Box::leak(Box::new(Waker::from(Arc::clone(&frame_wake))));

    // Send HELLO with framebuffer dimensions (the authoritative viewport
    // discovered at backend initialization), then push the current frame so the
    // canvas shows content immediately on connect/reconnect.
    {
        let hello = Hello {
            protocol_version: protocol::PROTOCOL_VERSION,
            width: viewport.width,
            height: viewport.height,
            scale_factor: viewport.scale_factor,
        };
        if ws.send(protocol::hello(&hello)).await.is_err() {
            return Ok(());
        }
        // Bind the frame to a local so the `RefMut` borrow drops before the
        // await below; holding `source.borrow_mut()` across `ws.send().await`
        // panics ("already borrowed") the moment a second connection tries to
        // borrow the same shell (see frame()/waker scheduling docs).
        let frame = source.borrow_mut().frame(Some(waker));
        if let Some(frame) = frame {
            let _ = ws
                .send(protocol::frame(frame.width, frame.height, &frame.data))
                .await;
        }
    }

    let mut close = false;
    let mut last_ping = std::time::Instant::now();
    while !close {
        // The animation-tick branch is armed only while the document is
        // animating; otherwise the connection blocks on input/wake/keepalive.
        let animating = source.borrow().needs_redraw();

        let wait_start = std::time::Instant::now();
        tokio::select! {
            msg = ws.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(input) = protocol::parse_input_message(&text) {
                            // Clocks down/up so we can see the user's clicks land.
                            match input.r#type.as_str() {
                                "pointerdown" => info!(
                                    "input down @({:.1},{:.1}) button={}",
                                    input.x, input.y, input.button
                                ),
                                "pointerup" => info!(
                                    "input up @({:.1},{:.1}) button={}",
                                    input.x, input.y, input.button
                                ),
                                _ => {}
                            }
                            let event = translate_input(&input, 0.0);
                            // Borrow-scope discipline: never hold a RefCell borrow
                            // across an await point.
                            {
                                let mut source = source.borrow_mut();
                                source.handle_pointer_event(event);
                            }
                        } else if text.trim() == "PING" {
                            let _ = ws.send(Message::Text("PONG".to_string().into())).await;
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        if let Some(reflected_ns) = protocol::parse_pong(&bytes) {
                            // Client echoed our PING timestamp; the elapsed
                            // wall time is the network round-trip.
                            let rtt_us = nanos_now().saturating_sub(reflected_ns) / 1000;
                            touchbard_renderer::diag::record(
                                touchbard_renderer::diag::Ev::Pong { rtt_us },
                            );
                            if rtt_us > 1000 {
                                info!("preview client round-trip {rtt_us}us");
                            }
                        } else {
                            info!("Received binary message of {} bytes", bytes.len());
                        }
                    }
                    Some(Ok(Message::Ping(_))) => {
                        let _ = ws.send(Message::Pong(Vec::new().into())).await;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        warn!("WebSocket error: {e}");
                        close = true;
                    }
                    None => close = true,
                }
            }
            _ = frame_wake.notify.notified() => {}
            _ = tokio::time::sleep(ANIM_TICK), if animating => {}
        }

        // Keepalive/RTT probe: the sleep-arm version above never fires while
        // the animation tick keeps the select busy, so ping on a plain elapsed
        // check instead. The browser echoes the timestamp in a binary PONG
        // that is decoded in the `ws.next()` arm above.
        if last_ping.elapsed() > Duration::from_secs(1) {
            let _ = ws.send(protocol::ping(nanos_now())).await;
            last_ping = std::time::Instant::now();
        }

        // The client is gone: drop out without the post-close best-effort
        // frame send.
        if close {
            break;
        }

        // One scheduling decision per wake, whichever reason woke the select:
        // present a frame only when the runtime says there is something new.
        touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::Wait {
            wait_us: wait_start.elapsed().as_micros() as u64,
        });
        // Bind the frame to a local so the `RefMut` borrow drops before the
        // await below; holding `source.borrow_mut()` across `ws.send().await`
        // panics ("already borrowed") the moment a second connection tries to
        // borrow the same shell.
        let frame = source.borrow_mut().frame(Some(waker));
        if let Some(frame) = frame {
            touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::SendStart);
            let present_start = std::time::Instant::now();
            let _ = ws
                .send(protocol::frame(frame.width, frame.height, &frame.data))
                .await;
            touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::Present {
                present_us: present_start.elapsed().as_micros() as u64,
            });
        }
    }

    info!("WebSocket connection closed");
    Ok(())
}

/// Convert a browser pointer event into a framework `PointerEvent` (logical/CSS px).
///
/// The JS client already converts CSS pixels to logical pixels
/// (`x_css / rect.width * frameWidth / scale_factor`), and Blitz hit-tests in
/// logical coordinates, so coordinates pass through unchanged (no scaling).
fn translate_input(input: &crate::protocol::InputEvent, _scale_factor: f64) -> PointerEvent {
    let (kind, button) = match input.r#type.as_str() {
        "pointerdown" => (PointerEventKind::Down, input.button),
        "pointerup" => (PointerEventKind::Up, input.button),
        "pointermove" => (PointerEventKind::Move, input.button),
        _ => (PointerEventKind::Move, input.button),
    };

    let button = match button {
        0 => PointerButton::Main,
        1 => PointerButton::Auxiliary,
        2 => PointerButton::Secondary,
        3 => PointerButton::Fourth,
        4 => PointerButton::Fifth,
        _ => PointerButton::Main,
    };

    PointerEvent {
        x: input.x as f32,
        y: input.y as f32,
        button,
        buttons: input.buttons,
        kind,
    }
}

fn nanos_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_websocket_accept() {
        // RFC 6455 example.
        let accept = websocket_accept("dGhlIHNhbXBsZSBub25jZQ==");
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn test_preview_config_defaults() {
        let config = PreviewConfig::default();
        assert_eq!(config.width, 2008);
        assert_eq!(config.height, 60);
        assert_eq!(config.scale_factor, 1.0);
        assert_eq!(config.bind_addr, "127.0.0.1:8888");
    }

    /// Backend initialization must provide the configured/default viewport:
    /// 2008×60 @ 1.0 with no overrides.
    #[test]
    fn test_preview_backend_initialize_default_viewport() {
        let mut backend = PreviewBackend::new();
        let viewport = backend
            .initialize()
            .expect("preview initialization is infallible");
        assert_eq!(viewport.width, 2008);
        assert_eq!(viewport.height, 60);
        assert_eq!(viewport.scale_factor, 1.0);
    }

    /// Backend initialization must follow the configuration: a custom
    /// PreviewConfig changes the produced viewport.
    #[test]
    fn test_preview_backend_initialize_uses_config() {
        let mut config = PreviewConfig::default();
        config.width = 1280;
        config.height = 40;
        config.scale_factor = 1.0;
        let mut backend = PreviewBackend::with_config(config);
        let viewport = backend
            .initialize()
            .expect("preview initialization is infallible");
        assert_eq!(viewport.width, 1280);
        assert_eq!(viewport.height, 40);
        assert_eq!(viewport.scale_factor, 1.0);
    }

    #[test]
    fn test_translate_input_passes_coordinates_through_unchanged() {
        let input = crate::protocol::InputEvent {
            r#type: "pointermove".into(),
            x: 540.0,
            y: 15.0,
            button: 0,
            buttons: 0,
        };
        let ev = translate_input(&input, 2.0);
        assert_eq!(ev.x, 540.0);
        assert_eq!(ev.y, 15.0);
        assert_eq!(ev.kind, PointerEventKind::Move);
    }

    #[test]
    fn test_translate_input_down() {
        let input = crate::protocol::InputEvent {
            r#type: "pointerdown".into(),
            x: 10.0,
            y: 5.0,
            button: 0,
            buttons: 1,
        };
        let ev = translate_input(&input, 1.0);
        assert_eq!(ev.kind, PointerEventKind::Down);
        assert_eq!(ev.button, PointerButton::Main);
        assert_eq!(ev.buttons, 1);
    }
}

#[cfg(test)]
mod ws_integration_tests {
    use super::*;
    use crate::protocol::MsgType;
    use dioxus::prelude::*;
    use tokio_tungstenite::connect_async;
    use touchbard::TouchbardSystem;

    const ANIM_CSS: &str = r#"
        @keyframes preview-demo {
            from { opacity: 1.0; }
            to   { opacity: 0.2; }
        }
        .preview-demo {
            animation: preview-demo 1s linear infinite;
        }
    "#;

    fn animated_app() -> Element {
        rsx! {
            div {
                style: "width: 100%; height: 100%; background: #000;",
                style { {ANIM_CSS} }
                div {
                    class: "preview-demo",
                    style: "width: 40px; height: 8px; background-color: #fff;",
                }
            }
        }
    }

    /// The server must keep producing frames without any input while the
    /// document is animating (the old loop only rendered when an input message
    /// arrived, which froze CSS animations in the browser).
    #[test]
    fn preview_advances_animations_without_input() {
        let viewport = Viewport {
            width: 160,
            height: 60,
            scale_factor: 2.0,
        };
        let source: Rc<RefCell<dyn FrameSource>> =
            Rc::new(RefCell::new(TouchbardSystem::new(animated_app, viewport)));

        let local = tokio::task::LocalSet::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");

        local.block_on(&runtime, async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("local addr");

            let server_source = Rc::clone(&source);
            let server_viewport = viewport;
            let active_clients = Rc::new(Cell::new(0usize));
            local.spawn_local(async move {
                let (stream, _) = listener.accept().await.expect("accept");
                handle_connection(stream, server_viewport, server_source, active_clients)
                    .await
                    .expect("connection handled");
            });

            // Let the server arm its accept before the client connects.
            tokio::task::yield_now().await;

            let (mut ws, _) = connect_async(format!("ws://{addr}/ws"))
                .await
                .expect("websocket upgrade");

            // No input is sent. The document animates forever, so the server
            // must keep streaming frames on its own.
            let mut frames = 0u32;
            let deadline = tokio::time::sleep(Duration::from_millis(800));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    msg = ws.next() => match msg {
                        Some(Ok(Message::Binary(bytes))) => {
                            if bytes.first() == Some(&(MsgType::Frame as u8)) {
                                frames += 1;
                                if frames >= 3 {
                                    break;
                                }
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => panic!("ws error: {e}"),
                        None => panic!("connection closed unexpectedly"),
                    },
                    _ = &mut deadline => break,
                }
            }
            assert!(
                frames >= 3,
                "animated preview must stream frames without input; got {frames}"
            );
        });
    }

    /// The server supports exactly one live preview client: a second connection
    /// must be rejected with a clean policy close (not a panic). Regression for
    /// the duplicate-connection crash caused by holding a `RefCell` borrow
    /// across `ws.send(...).await` while a second connection borrowed the same
    /// shell.
    #[test]
    fn duplicate_connection_is_rejected_with_policy_close() {
        let viewport = Viewport {
            width: 160,
            height: 60,
            scale_factor: 2.0,
        };
        let source: Rc<RefCell<dyn FrameSource>> =
            Rc::new(RefCell::new(TouchbardSystem::new(animated_app, viewport)));

        let local = tokio::task::LocalSet::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");

        local.block_on(&runtime, async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("local addr");

            let server_source = Rc::clone(&source);
            let server_viewport = viewport;
            let active_clients = Rc::new(Cell::new(0usize));
            local.spawn_local(async move {
                for _ in 0..2 {
                    let (stream, _) = listener.accept().await.expect("accept");
                    let source = Rc::clone(&server_source);
                    let active = Rc::clone(&active_clients);
                    tokio::task::spawn_local(async move {
                        let _ = handle_connection(stream, server_viewport, source, active).await;
                    });
                }
            });

            // Let the server arm its accept before the clients connect.
            tokio::task::yield_now().await;

            let (mut first, _) = connect_async(format!("ws://{addr}/ws"))
                .await
                .expect("first websocket upgrade");
            tokio::task::yield_now().await;

            let (mut second, _) = connect_async(format!("ws://{addr}/ws"))
                .await
                .expect("second websocket upgrade");

            // The second connection must receive the policy close (and the
            // first must keep streaming frames, i.e. the guard freed the shell
            // for the first client).
            let deadline = tokio::time::sleep(Duration::from_millis(500));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    msg = second.next() => match msg {
                        Some(Ok(Message::Close(_frame))) => break,
                        Some(Ok(Message::Binary(bytes))) if bytes.first() == Some(&(MsgType::Frame as u8)) => continue,
                        Some(Ok(_)) => {}
                        Some(Err(e)) => panic!("second ws unexpected error: {e}"),
                        None => panic!("second ws closed without a policy frame"),
                    },
                    _ = &mut deadline => panic!("second connection was not rejected"),
                }
            }

            // The first connection must still be usable (frames flowing).
            let mut frames = 0u32;
            let deadline = tokio::time::sleep(Duration::from_millis(500));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    msg = first.next() => match msg {
                        Some(Ok(Message::Binary(bytes))) if bytes.first() == Some(&(MsgType::Frame as u8)) => {
                            frames += 1;
                            if frames >= 2 { break; }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => panic!("first ws error: {e}"),
                        None => panic!("first ws closed unexpectedly"),
                    },
                    _ = &mut deadline => break,
                }
            }
            assert!(frames >= 2, "first client must keep receiving frames; got {frames}");
        });
    }
}
