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
//! Multiple clients: the runtime keeps a single host waker (the scheduler and
//! shell-redraw bridges store exactly one), so exactly one **producer** task
//! owns the waker, renders frames and broadcasts them to a `tokio::sync::broadcast`
//! group. Every connected browser subscribes to the group and receives the same
//! frames; pointer events from any tab feed the one shared runtime, so all tabs
//! show a single shared UI state.

use crate::protocol::{self, Hello};
use touchbard_renderer::{
    Backend, FrameSource, PointerButton, PointerEvent, PointerEventKind, Viewport,
};

use futures_util::{SinkExt, StreamExt};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Wake, Waker};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tracing::{info, warn};

/// The [`Wake`] that completes a connection's [`Notify`]: Dioxus scheduler
/// wakeups and shell redraw requests unblock the producer's select loop so it
/// can ask the runtime whether there is a frame to send.
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

/// Most recently broadcast frame, so a freshly connected client can show the
/// current UI immediately instead of waiting for the next change.
///
/// `Message` clones are cheap for binary frames (a `Bytes` refcount bump), so
/// the snapshot recorded here shares the producer's encoded buffer.
type LatestFrame = Rc<RefCell<Option<Message>>>;

/// Capacity of the frame fan-out channel: a small ring buffer is plenty — a
/// slow subscriber that falls behind resubscribes and jumps to the latest.
const BROADCAST_CAPACITY: usize = 4;

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
/// Default bind address for the HTTP/WebSocket server. A new instance takes
/// this port over from a stale one (see [`bind_listener`]).
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

/// Create a fresh socket, enable `SO_REUSEADDR` and bind+listen on `sock_addr`.
/// A fresh socket per call, so a failed (EADDRINUSE) bind can be retried.
fn bind_one(
    domain: socket2::Domain,
    sock_addr: &socket2::SockAddr,
) -> Result<tokio::net::TcpListener, std::io::Error> {
    let sock = socket2::Socket::new(domain, socket2::Type::STREAM, None)?;
    sock.set_reuse_address(true)?;
    sock.set_nonblocking(true)?;
    sock.bind(sock_addr)?;
    sock.listen(1024)?;
    let std_listener: std::net::TcpListener =
        sock.try_into().expect("socket -> TcpListener conversion never fails");
    tokio::net::TcpListener::from_std(std_listener)
}

/// Bind the preview listener to `address`, taking the port over from a stale
/// instance if one is still listening on it.
///
/// The socket gets `SO_REUSEADDR` so a killed predecessor's lingering
/// `TIME_WAIT` sockets can never block a rebind, and on `Address already in
/// use` the stale instance of this app holding the port is killed and the bind
/// is retried — a fresh `--preview` run always displaces its predecessor
/// instead of dying with `os error 98`.
async fn bind_listener(
    address: &str,
) -> Result<tokio::net::TcpListener, Box<dyn std::error::Error + Send + Sync>> {
    let (domain, sock_addr) = match address.parse::<std::net::SocketAddr>() {
        Ok(addr @ std::net::SocketAddr::V4(_)) => {
            (socket2::Domain::IPV4, socket2::SockAddr::from(addr))
        }
        Ok(addr @ std::net::SocketAddr::V6(_)) => {
            (socket2::Domain::IPV6, socket2::SockAddr::from(addr))
        }
        Err(_) => {
            return Err(format!("invalid bind address: {address}").into());
        }
    };

    match bind_one(domain, &sock_addr) {
        Ok(listener) => return Ok(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            let port = sock_addr.as_socket().map_or(0, |a| a.port());
            warn!("{address} is already in use; killing the stale instance to take the port over");
            kill_stale_listeners(port);
        }
        Err(e) => return Err(Box::new(e)),
    }

    // The killed process must die and hand back the socket before the rebind
    // can succeed; retry briefly.
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Ok(listener) = bind_one(domain, &sock_addr) {
            return Ok(listener);
        }
    }
    Err(format!("{address} is still in use after reclaiming it").into())
}

/// Kill stale instances of this app that are listening on `port` so a new
/// process can take the port over. Only processes whose `comm` matches this
/// binary are killed — never an unrelated app. No-op on non-Linux.
#[cfg(target_os = "linux")]
fn kill_stale_listeners(port: u16) {
    use std::collections::HashSet;
    use std::fs;

    if port == 0 {
        return;
    }

    // Map port -> listening socket inodes by scanning the kernel's TCP tables.
    let port_hex = format!("{port:X}");
    let mut listeners = HashSet::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(contents) = fs::read_to_string(table) else {
            continue;
        };
        for line in contents.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() <= 9 {
                continue;
            }
            // local_address is `IP:PORT` in hex, state `0A` = LISTEN.
            if fields[3] != "0A" {
                continue;
            }
            if !fields[1].to_ascii_uppercase().ends_with(&format!(":{port_hex}")) {
                continue;
            }
            if let Ok(inode) = fields[9].parse::<u64>() {
                listeners.insert(inode);
            }
        }
    }
    if listeners.is_empty() {
        return;
    }

    // Walk every process, owning the listener inode via a socket: fd symlink.
    let Ok(entries) = fs::read_dir("/proc") else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let pid_dir = entry.path();
        let owns = || -> bool {
            let Ok(fds) = fs::read_dir(pid_dir.join("fd")) else {
                return false;
            };
            for fd in fds.flatten() {
                let Ok(target) = fs::read_link(fd.path()) else {
                    continue;
                };
                let target = target.to_string_lossy().to_string();
                if let Some(inode) = target
                    .strip_prefix("socket:[")
                    .and_then(|s| s.strip_suffix(']'))
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    if listeners.contains(&inode) {
                        return true;
                    }
                }
            }
            false
        };
        if !owns() {
            continue;
        }
        // Safety: never kill an unrelated app — only this binary's comm.
        let Ok(comm) = fs::read_to_string(pid_dir.join("comm")) else {
            continue;
        };
        if comm.trim() != "control-center" {
            continue;
        }
        info!("Killing stale control-center pid {pid} holding the preview port");
        let _ = std::process::Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .status();
    }
}

#[cfg(not(target_os = "linux"))]
fn kill_stale_listeners(_port: u16) {}

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
    let listener = bind_listener(&config.bind_addr).await?;
    info!("Preview server listening on http://{}", config.bind_addr);

    // Fan-out group: one producer owns the single host waker, renders frames
    // and broadcasts them; every connected client subscribes to the same group.
    let (tx, _) = tokio::sync::broadcast::channel::<Message>(BROADCAST_CAPACITY);
    let latest: LatestFrame = Rc::default();
    tokio::task::spawn_local(run_frame_producer(
        Rc::clone(&source),
        tx.clone(),
        Rc::clone(&latest),
    ));

    // Graceful Ctrl+C: stop accepting and return, so the process exits and the
    // socket is released. With no handler SIGINT kills the process abruptly;
    // with one, shutdown is explicit and logged.
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, addr) = accepted?;
                let source = Rc::clone(&source);
                let latest = Rc::clone(&latest);
                let rx = tx.subscribe();
                tokio::task::spawn_local(async move {
                    if let Err(e) = handle_connection(stream, viewport, source, latest, rx).await {
                        warn!("Connection {addr} error: {e}");
                    }
                });
            }
            _ = &mut shutdown => {
                info!("Shutting down on Ctrl+C — releasing {}", config.bind_addr);
                return Ok(());
            }
        }
    }
}

/// Upper bound on the animation cadence (≈60 Hz): while the runtime reports it
/// is animating, the producer asks for a frame every [`ANIM_TICK`]. Not a fixed
/// render rate — an idle document blocks until woken.
const ANIM_TICK: Duration = touchbard_renderer::frame_source::FRAME_CADENCE;

/// The one task that drives the shared runtime: it owns the single host waker,
/// renders frames when woken and broadcasts every present to the client group.
///
/// The runtime keeps exactly one armed waker (see module docs), so no client
/// task may call [`FrameSource::frame`]. A freshly connected client receives
/// the [`LatestFrame`] snapshot immediately; the producer's broadcast keeps it
/// current from then on.
async fn run_frame_producer(
    source: Rc<RefCell<dyn FrameSource>>,
    tx: tokio::sync::broadcast::Sender<Message>,
    latest: LatestFrame,
) {
    let frame_wake = Arc::new(FrameWake {
        notify: Notify::new(),
    });
    let waker: &'static Waker = Box::leak(Box::new(Waker::from(Arc::clone(&frame_wake))));

    // Initial present: the first `frame(Some(waker))` is guaranteed non-empty
    // (see `FrameSource::frame`) and arms the host waker for every later
    // scheduler/shell wake — mirroring the per-connection initial push the old
    // single-client loop did on connect.
    let frame = source.borrow_mut().frame(Some(waker));
    if let Some(frame) = frame {
        publish_frame(tx.clone(), &latest, frame);
    }

    loop {
        // The animation-tick branch is armed only while the document is
        // animating (or a frame is coalesced pending); otherwise the producer
        // blocks on the host wake.
        let animating = source.borrow().needs_redraw();
        let pending = source.borrow().frame_pending();
        // Wait only the remaining slice to the cadence boundary whenever a
        // frame is due — a coalesced one or the next animation tick — not a
        // full ANIM_TICK (which would restart the period after the last present
        // *and its render*, stretching the present interval and making
        // animation advance by unequal steps).
        let deadline = source.borrow().frame_deadline();

        let wait_start = std::time::Instant::now();
        tokio::select! {
            _ = frame_wake.notify.notified() => {}
            _ = tokio::time::sleep(deadline.unwrap_or(ANIM_TICK)), if animating || pending => {}
        }

        // One scheduling decision per wake, whichever reason woke the select:
        // present a frame only when the runtime says there is something new.
        touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::Wait {
            wait_us: wait_start.elapsed().as_micros() as u64,
        });
        let frame = source.borrow_mut().frame(Some(waker));
        if let Some(frame) = frame {
            publish_frame(tx.clone(), &latest, frame);
        }
    }
}

/// Encode one frame once and fan it out: record it as the latest snapshot and
/// broadcast the shared buffer to every subscriber. Sending to a channel with
/// no live receivers errors and is ignored — nobody is looking.
fn publish_frame(tx: tokio::sync::broadcast::Sender<Message>, latest: &LatestFrame, frame: touchbard_renderer::Frame) {
    touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::SendStart);
    let present_start = std::time::Instant::now();
    let message = protocol::frame(frame.width, frame.height, &frame.data);
    // Snapshot first so a client subscribing right now either sees it here or
    // receives it on the group — never a gap (a duplicate is harmless).
    *latest.borrow_mut() = Some(message.clone());
    let _ = tx.send(message);
    touchbard_renderer::diag::record(touchbard_renderer::diag::Ev::Present {
        present_us: present_start.elapsed().as_micros() as u64,
    });
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
    latest: LatestFrame,
    mut rx: tokio::sync::broadcast::Receiver<Message>,
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
    // discovered at backend initialization), then the latest frame snapshot so
    // the canvas shows current content immediately on connect/reconnect, even
    // while the document is idle.
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
        if let Some(snapshot) = latest.borrow().clone() {
            if ws.send(snapshot).await.is_err() {
                return Ok(());
            }
        }
    }

    let mut close = false;
    let mut last_ping = std::time::Instant::now();
    while !close {
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
            frame = rx.recv() => {
                match frame {
                    // Frame from the shared producer: forward it as-is. Binary
                    // payloads share the producer's buffer (a refcount bump).
                    Ok(msg) => {
                        if ws.send(msg).await.is_err() {
                            close = true;
                        }
                    }
                    // Fell behind the producer: resubscribe to jump to the
                    // latest frame instead of replaying the backlog stale ones.
                    Err(RecvError::Lagged(_)) => {
                        rx = rx.resubscribe();
                    }
                    // Producer gone (server shutting down): close.
                    Err(RecvError::Closed) => close = true,
                }
            }
        }

        // Keepalive/RTT probe: the browser echoes the timestamp in a binary
        // PONG decoded in the `ws.next()` arm above.
        if last_ping.elapsed() > Duration::from_secs(1) {
            let _ = ws.send(protocol::ping(nanos_now())).await;
            last_ping = std::time::Instant::now();
        }

        // The client is gone: drop out without the post-close best-effort
        // frame send.
        if close {
            break;
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

    /// Spin up the fan-out preview server (one producer + per-connection
    /// handlers) on an ephemeral port and return the WebSocket URL.
    ///
    /// Mirrors what `serve` does after binding: the producer owns the single
    /// host waker and broadcasts frames; every accepted connection subscribes
    /// to the group.
    async fn spawn_server(
        source: Rc<RefCell<dyn FrameSource>>,
        viewport: Viewport,
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");

        let (tx, _) = tokio::sync::broadcast::channel::<Message>(BROADCAST_CAPACITY);
        let latest: LatestFrame = Rc::default();
        tokio::task::spawn_local(run_frame_producer(
            Rc::clone(&source),
            tx.clone(),
            Rc::clone(&latest),
        ));
        tokio::task::spawn_local({
            let source = Rc::clone(&source);
            let latest = Rc::clone(&latest);
            async move {
                loop {
                    let (stream, _) = listener.accept().await.expect("accept");
                    let source = Rc::clone(&source);
                    let latest = Rc::clone(&latest);
                    let rx = tx.subscribe();
                    tokio::task::spawn_local(async move {
                        let _ = handle_connection(stream, viewport, source, latest, rx).await;
                    });
                }
            }
        });

        format!("ws://{addr}/ws")
    }

    /// Count `FRAME` binary messages arriving on `ws` until `want` are seen or
    /// the deadline passes.
    async fn count_frames(ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, want: u32, by: Duration) -> u32 {
        let mut frames = 0u32;
        let deadline = tokio::time::sleep(by);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                msg = ws.next() => match msg {
                    Some(Ok(Message::Binary(bytes))) if bytes.first() == Some(&(MsgType::Frame as u8)) => {
                        frames += 1;
                        if frames >= want {
                            return frames;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => panic!("ws error: {e}"),
                    None => panic!("connection closed unexpectedly"),
                },
                _ = &mut deadline => return frames,
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
            let url = spawn_server(Rc::clone(&source), viewport).await;
            let (mut ws, _) = connect_async(&url).await.expect("websocket upgrade");

            // No input is sent. The document animates forever, so the producer
            // must keep streaming frames on its own.
            let frames = count_frames(&mut ws, 3, Duration::from_millis(800)).await;
            assert!(
                frames >= 3,
                "animated preview must stream frames without input; got {frames}"
            );
        });
    }

    /// The fan-out group serves every connected preview client: each tab
    /// subscribes to the producer's broadcast and receives the same frames
    /// concurrently — no BUSY rejection, and no starvation from the single
    /// shared runtime waker (which the producer alone arms).
    #[test]
    fn all_clients_receive_broadcast_frames() {
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
            let url = spawn_server(Rc::clone(&source), viewport).await;

            let (mut first, _) = connect_async(&url).await.expect("first websocket upgrade");
            tokio::task::yield_now().await;
            let (mut second, _) = connect_async(&url).await.expect("second websocket upgrade");

            // Both connections must stream the same animation concurrently.
            let (f1, f2) = tokio::join!(
                count_frames(&mut first, 3, Duration::from_millis(800)),
                count_frames(&mut second, 3, Duration::from_millis(800)),
            );
            assert!(f1 >= 3, "first client must stream frames; got {f1}");
            assert!(f2 >= 3, "second client must stream frames; got {f2}");
        });
    }
}
