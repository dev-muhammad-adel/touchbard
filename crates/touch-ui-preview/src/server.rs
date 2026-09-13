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

use crate::protocol::{self, Hello};
use touch_ui_renderer::{
    Backend, FrameSource, PointerButton, PointerEvent, PointerEventKind, Viewport,
};

use futures_util::{SinkExt, StreamExt};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tracing::{info, warn};

/// Static files served by the preview HTTP server, keyed by path.
pub const INDEX_HTML: &str = include_str!("../../../preview/index.html");
pub const PREVIEW_JS: &str = include_str!("../../../preview/preview.js");

/// Default framebuffer size (physical pixels) for the preview canvas.
pub const DEFAULT_WIDTH: u32 = 2008;
/// Default framebuffer size (physical pixels) for the preview canvas.
pub const DEFAULT_HEIGHT: u32 = 60;
/// Default scale factor (physical pixels per logical pixel).
pub const DEFAULT_SCALE: f64 = 2.0;
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
    /// Read overrides from `TOUCH_UI_WIDTH`, `TOUCH_UI_HEIGHT`, `TOUCH_UI_SCALE`
    /// (physical pixels / scale factor) and `TOUCH_UI_BIND` (server address);
    /// invalid values fall back to defaults.
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(value) = std::env::var("TOUCH_UI_WIDTH") {
            if let Ok(width) = value.parse() {
                config.width = width;
            }
        }
        if let Ok(value) = std::env::var("TOUCH_UI_HEIGHT") {
            if let Ok(height) = value.parse() {
                config.height = height;
            }
        }
        if let Ok(value) = std::env::var("TOUCH_UI_SCALE") {
            if let Ok(scale) = value.parse() {
                config.scale_factor = scale;
            }
        }
        if let Ok(value) = std::env::var("TOUCH_UI_BIND") {
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

    loop {
        let (stream, addr) = listener.accept().await?;
        let source = Rc::clone(&source);
        tokio::task::spawn_local(async move {
            if let Err(e) = handle_connection(stream, viewport, source).await {
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
        let frame = source.borrow_mut().poll_and_render();
        let _ = ws
            .send(protocol::frame(frame.width, frame.height, &frame.data))
            .await;
    }

    loop {
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
                            let frame = {
                                let mut source = source.borrow_mut();
                                source.handle_pointer_event(event);
                                source.poll_and_render()
                            };
                            let _ = ws
                                .send(protocol::frame(frame.width, frame.height, &frame.data))
                                .await;
                        } else if text.trim() == "PING" {
                            let _ = ws.send(Message::Text("PONG".to_string().into())).await;
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        // Reserved for future binary input encoding.
                        info!("Received binary message of {} bytes", bytes.len());
                    }
                    Some(Ok(Message::Ping(_))) => {
                        let _ = ws.send(Message::Pong(Vec::new().into())).await;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        warn!("WebSocket error: {e}");
                        break;
                    }
                    None => break,
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                let _ = ws.send(protocol::ping(nanos_now())).await;
            }
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
        assert_eq!(config.scale_factor, 2.0);
        assert_eq!(config.bind_addr, "127.0.0.1:8888");
    }

    /// Backend initialization must provide the configured/default viewport:
    /// 2008×60 @ 2.0 with no overrides.
    #[test]
    fn test_preview_backend_initialize_default_viewport() {
        let mut backend = PreviewBackend::new();
        let viewport = backend
            .initialize()
            .expect("preview initialization is infallible");
        assert_eq!(viewport.width, 2008);
        assert_eq!(viewport.height, 60);
        assert_eq!(viewport.scale_factor, 2.0);
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