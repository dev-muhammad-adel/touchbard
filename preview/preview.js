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

function connect() {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  ws = new WebSocket(`${proto}://${location.host}/ws`);
  ws.binaryType = "arraybuffer";

  ws.onopen = () => {
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
    hud.textContent = "disconnected — reconnecting…";
    setTimeout(connect, 1000);
  };

  ws.onerror = () => {
    ws.close();
  };
}

function handleText(text) {
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
  // Keep the whole strip visible: scale it down to fit the window, never above 2x.
  const fit = scaleToFit(width, height);
  canvas.style.width = `${width * fit}px`;
  canvas.style.height = `${height * fit}px`;
}

function scaleToFit(width, height) {
  const margin = 24;
  const availW = Math.max(100, window.innerWidth - margin);
  const availH = Math.max(60, window.innerHeight - margin);
  return Math.max(0.2, Math.min(2.0, availW / width, availH / height));
}

// Re-scale on window changes so the whole strip stays visible/clickable.
window.addEventListener("resize", () => {
  if (frameWidth > 0 && frameHeight > 0) applySize(frameWidth, frameHeight);
});

function handleBinary(bytes) {
  const type = bytes[0];
  switch (type) {
    case MSG.FRAME: {
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