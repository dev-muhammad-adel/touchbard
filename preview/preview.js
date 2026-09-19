// Touchbard Preview client.
//
// Receives real RGBA framebuffers over WebSocket and draws them to a canvas.
// Sends pointer events back to the Rust runtime. This is a display/debug
// client only — it does NOT rebuild the UI as HTML.

const PROTOCOL_VERSION = 1;

const MSG = {
  HELLO: 0x01,
  FRAME: 0x02,
  INPUT: 0x03,
  RESIZE: 0x04,
  PING: 0x05,
  PONG: 0x06,
  RAW_FRAME: 0x07,
};

const canvas = document.getElementById("stage");
const hud = document.getElementById("hud");
const ctx = canvas.getContext("2d");

let ws = null;
let frameWidth = 0;
let frameHeight = 0;
let scaleFactor = 1.0;
let reconnecting = false;

// ---- Optional frame-timing metering (?diag=1) -----------------------------
// Shows browser-observed cadence: WebSocket arrival interval, the per-frame
// unpremultiply/putImageData cost, and the rAF (vsync) tick cadence. Enabled
// only on demand so the normal preview path keeps zero overhead.
const DIAG = new URLSearchParams(location.search).has("diag");
const diagEl = DIAG ? document.getElementById("diaghud") : null;
const arrTimes = []; // perf.now() at each FRAME receive (last ~120)
const decodeUs = []; // ms spent preparing + putting each frame
const rafDeltas = []; // ms between consecutive rAF callbacks (last ~120)
if (DIAG) {
  diagEl.hidden = false;
  const k = { last: performance.now() };
  const tick = (t) => {
    rafDeltas.push(t - k.last);
    if (rafDeltas.length > 120) rafDeltas.shift();
    k.last = t;
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  setInterval(() => {
    while (arrTimes.length > 120) arrTimes.shift();
    const iv = arrTimes.length >= 2 ? arrTimes.slice(1).map((t, i) => t - arrTimes[i]) : [];
    const rAF = rafDeltas.filter((d) => d > 5); // ignore tab-switch stalls
    const avg = (xs) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : 0);
    const pct = (xs, q) => {
      const s = [...xs].sort((a, b) => a - b);
      return s.length ? s[Math.min(s.length - 1, Math.floor((s.length - 1) * q))] : 0;
    };
    const fps = iv.length ? 1000 / avg(iv) : 0;
    const dec = decodeUs.length ? avg(decodeUs) : 0;
    diagEl.textContent =
      `fps ${fps.toFixed(1)} · arrive ${avg(iv).toFixed(1)}(p99 ${pct(iv, 0.99).toFixed(1)})ms · ` +
      `decode ${dec.toFixed(2)}ms · rAF ${avg(rAF).toFixed(2)}ms`;
  }, 500);
}

const DEFAULT_HOST = "127.0.0.1:8888";

// Set when the server rejects this connection because another preview client is
// already connected (BUSY). A rejected tab stops reconnecting instead of
// hammering the server every second.
let rejected = false;

function connect() {
  // When the page is opened directly from disk (file://) there is no server
  // host; fall back to the preview server's default address.
  const host = location.host || DEFAULT_HOST;
  const proto = location.protocol === "https:" ? "wss" : "ws";
  rejected = false;
  ws = new WebSocket(`${proto}://${host}/ws`);
  ws.binaryType = "arraybuffer";

  ws.onopen = () => {
    if (rejected) return;
    hud.textContent = "connected";
  };

  ws.onmessage = (e) => {
    if (typeof e.data === "string") {
      handleText(e.data);
    } else {
      handleBinary(new Uint8Array(e.data));
    }
  };

  ws.onclose = () => {
    if (rejected) {
      // Another tab owns the preview; do not fight it for the slot.
      hud.textContent = "another preview is already connected";
      return;
    }
    hud.textContent = "disconnected — reconnecting…";
    setTimeout(connect, 1000);
  };

  ws.onerror = () => {
    ws.close();
  };
}

function handleText(text) {
  if (text.startsWith("BUSY")) {
    // The server already has a live preview client; stop trying.
    rejected = true;
    hud.textContent = "another preview is already connected";
    ws.close();
    return;
  }
  if (text.startsWith("HELLO ")) {
    const h = JSON.parse(text.slice(6));
    if (h.protocol_version !== PROTOCOL_VERSION) {
      hud.textContent = `protocol mismatch (server v${h.protocol_version}, client v${PROTOCOL_VERSION})`;
      return;
    }
    scaleFactor = h.scale_factor || 1.0;
    applySize(h.width, h.height);
    hud.textContent = `v${h.protocol_version} · ${h.width}×${h.height} · ${h.scale_factor}x`;
  } else if (text.startsWith("RESIZE ")) {
    const r = JSON.parse(text.slice(7));
    scaleFactor = r.scale_factor || 1.0;
    applySize(r.width, r.height);
    hud.textContent = `resized · ${r.width}×${r.height}`;
  } else if (text.startsWith("PONG")) {
    // keepalive
  }
}

function applySize(width, height) {
  frameWidth = width;
  frameHeight = height;
  canvas.width = width;
  canvas.height = height;
  // Keep the whole strip visible at the exact 2008:60 ratio: scale it to fit
  // the stage area (header/footer included), never above 2x.
  const fit = scaleToFit(width, height);
  // canvas.style.width = `${width * fit}px`;
  canvas.style.height = `${height * fit}px`;
}

// Available display area: the stage host (minus the bezel wrapper padding) when
// present, otherwise the window. Keeps both dimensions in physical px.
function scaleToFit(width, height) {
  const host = document.getElementById("stageHost");
  const margin = 26; // bezel padding + borders on both sides
  const availW = Math.max(100, (host ? host.clientWidth : window.innerWidth) - margin);
  const availH = Math.max(60, (host ? host.clientHeight : window.innerHeight) - margin);
  return Math.max(0.2, Math.min(2.0, availW / width, availH / height));
}

// Re-scale whenever the stage area changes (window resizes, bar heights move)
// so the whole strip stays visible and the ratio exact.
function rescale() {
  if (frameWidth > 0 && frameHeight > 0) applySize(frameWidth, frameHeight);
}
window.addEventListener("resize", rescale);
const stageHost = document.getElementById("stageHost");
if (stageHost && typeof ResizeObserver === "function") {
  new ResizeObserver(rescale).observe(stageHost);
}

function handleBinary(bytes) {
  const type = bytes[0];
  switch (type) {
    case MSG.FRAME: {
      if (DIAG) arrTimes.push(performance.now());
      const decodeStart = performance.now();
      const width = readU32(bytes, 1);
      const height = readU32(bytes, 5);
      const pixels = bytes.subarray(9);

      // The Rust side sends premultiplied RGBA; create a non-premultiplied
      // ImageData for accurate display.
      const imageData = ctx.createImageData(width, height);
      const out = imageData.data;
      for (let i = 0; i < out.length; i++) {
        out[i] = 255;
      }
      for (let i = 0; i < out.length; i += 4) {
        const a = pixels[i + 3] / 255;
        if (a > 0) {
          out[i]     = Math.min(255, Math.max(0, Math.round(pixels[i] / a)));
          out[i + 1] = Math.min(255, Math.max(0, Math.round(pixels[i + 1] / a)));
          out[i + 2] = Math.min(255, Math.max(0, Math.round(pixels[i + 2] / a)));
          out[i + 3] = pixels[i + 3];
        } else {
          out[i] = out[i + 1] = out[i + 2] = 0;
          out[i + 3] = 0;
        }
      }
      ctx.putImageData(imageData, 0, 0);
      if (DIAG) {
        decodeUs.push(performance.now() - decodeStart);
        if (decodeUs.length > 120) decodeUs.shift();
      }
      break;
    }
    case MSG.RAW_FRAME: {
      const width = readU32(bytes, 1);
      const height = readU32(bytes, 5);
      const pixels = bytes.subarray(9);
      const imageData = ctx.createImageData(width, height);
      imageData.data.set(pixels);
      ctx.putImageData(imageData, 0, 0);
      break;
    }
    case MSG.PING: {
      // Reply to server ping.
      if (ws && ws.readyState === WebSocket.OPEN) {
        const pong = new Uint8Array(9);
        pong[0] = MSG.PONG;
        pong.set(bytes.subarray(1, 9), 1);
        ws.send(pong);
      }
      break;
    }
    default:
      console.warn("unknown message type", type);
  }
}

function readU32(bytes, offset) {
  return (
    (bytes[offset] << 24) |
    (bytes[offset + 1] << 16) |
    (bytes[offset + 2] << 8) |
    bytes[offset + 3]
  ) >>> 0;
}

// ---- Input ---------------------------------------------------------------

function sendInput(type, e) {
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  const rect = canvas.getBoundingClientRect();
  // CSS-pixel coordinates relative to the canvas, scaled back to logical px.
  const x = ((e.clientX - rect.left) / rect.width) * frameWidth / scaleFactor;
  const y = ((e.clientY - rect.top) / rect.height) * frameHeight / scaleFactor;
  const msg = {
    type,
    x,
    y,
    button: e.button ?? 0,
    buttons: e.buttons ?? 0,
  };
  ws.send(`INPUT ${JSON.stringify(msg)}`);
}

["pointerdown", "pointerup", "pointermove"].forEach((type) => {
  canvas.addEventListener(type, (e) => {
    e.preventDefault();
    sendInput(type, e);
    canvas.focus();
  });
});

// Prevent scrolling/gestures from hijacking the canvas.
canvas.addEventListener("contextmenu", (e) => e.preventDefault());
canvas.style.touchAction = "none";

connect();