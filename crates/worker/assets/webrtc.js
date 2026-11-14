import Keyboard from "./core/input/keyboard.js";
import GestureHandler from "./core/input/gesturehandler.js";
import KeyTable from "./core/input/keysym.js";
import keysyms from "./core/input/keysymdef.js";

const screenEl = document.getElementById("screen");
const video = document.getElementById("video");
const statusEl = document.getElementById("status");
const textInput = document.getElementById("keyboard-input");
const keyboardButton = document.getElementById("keyboard");

const WHEEL_STEP = 50;
const WHEEL_LINE = 19;
const GESTURE_SCROLL = 50;
const GESTURE_ZOOM = 75;
const BUTTON_BITS = [1, 2, 4];
const TEXT_SENTINEL = "_".repeat(100);

let pc = null;
let ws = null;
let channel = null;
let generation = 0;
let retryTimer = 0;
let retryDelay = 1000;
let connectedOnce = false;
let mask = 0;
let wheelX = 0;
let wheelY = 0;
let gestureX = 0;
let gestureY = 0;
let lastSize = "";
let soundOn = false;
let lastText = TEXT_SENTINEL;

function setStatus(text) {
  statusEl.textContent = text || "";
  statusEl.hidden = !text;
}

function send(msg) {
  if (channel && channel.readyState === "open") channel.send(JSON.stringify(msg));
}

function toRemote(clientX, clientY) {
  const w = video.videoWidth;
  const h = video.videoHeight;
  const r = video.getBoundingClientRect();
  if (!w || !h || !r.width || !r.height) return null;
  const scale = Math.min(r.width / w, r.height / h);
  const left = r.left + (r.width - w * scale) / 2;
  const top = r.top + (r.height - h * scale) / 2;
  return {
    x: Math.max(0, Math.min(w - 1, Math.round((clientX - left) / scale))),
    y: Math.max(0, Math.min(h - 1, Math.round((clientY - top) / scale))),
  };
}

function pointer(clientX, clientY, nextMask = mask) {
  const p = toRemote(clientX, clientY);
  mask = nextMask;
  if (p) send(["p", p.x, p.y, mask]);
}

function click(clientX, clientY, bit) {
  pointer(clientX, clientY);
  pointer(clientX, clientY, mask | bit);
  pointer(clientX, clientY, mask & ~bit);
}

function wheelStep(clientX, clientY, bit) {
  const p = toRemote(clientX, clientY);
  if (!p) return;
  send(["p", p.x, p.y, mask | bit]);
  send(["p", p.x, p.y, mask]);
}

function key(keysym) {
  send(["k", keysym, 1]);
  send(["k", keysym, 0]);
}

function sendSize() {
  const size = `${Math.round(window.innerWidth)}x${Math.round(window.innerHeight)}`;
  if (size === lastSize) return;
  lastSize = size;
  send(["s", Math.round(window.innerWidth), Math.round(window.innerHeight)]);
}

function frameScale() {
  try {
    const frame = window.frameElement;
    if (frame) return Math.min(1, frame.getBoundingClientRect().width / window.innerWidth);
  } catch {}
  return 1;
}

function setCursor(hotX, hotY, w, h, b64) {
  if (!w || !h) {
    screenEl.style.cursor = "none";
    return;
  }
  const k = frameScale();
  const src = document.createElement("canvas");
  src.width = w;
  src.height = h;
  src.getContext("2d").putImageData(new ImageData(Uint8ClampedArray.from(atob(b64), (c) => c.charCodeAt(0)), w, h), 0, 0);
  const out = document.createElement("canvas");
  out.width = Math.max(1, Math.round(w * k));
  out.height = Math.max(1, Math.round(h * k));
  out.getContext("2d").drawImage(src, 0, 0, out.width, out.height);
  screenEl.style.cursor = `url(${out.toDataURL()}) ${Math.round(hotX * k)} ${Math.round(hotY * k)}, default`;
}

function onMessage(data) {
  let msg;
  try {
    msg = JSON.parse(data);
  } catch {
    return;
  }
  if (msg[0] === "cursor") setCursor(msg[1], msg[2], msg[3], msg[4], msg[5]);
  if (msg[0] === "clip" && navigator.clipboard?.writeText) navigator.clipboard.writeText(msg[1]).catch(() => {});
}

function teardown() {
  generation++;
  if (channel) channel.onopen = channel.onmessage = null;
  if (ws) {
    ws.onopen = ws.onmessage = ws.onclose = null;
    ws.close();
  }
  if (pc) {
    pc.ontrack = pc.onconnectionstatechange = null;
    pc.close();
  }
  pc = ws = channel = null;
}

function retry() {
  teardown();
  setStatus(statusEl.dataset.error || "Reconnecting…");
  clearTimeout(retryTimer);
  retryTimer = setTimeout(connect, retryDelay);
  retryDelay = Math.min(retryDelay * 2, 10000);
}

function connect() {
  teardown();
  const gen = generation;
  const current = () => gen === generation;
  setStatus(connectedOnce ? "Reconnecting…" : "Connecting…");
  const stream = new MediaStream();
  pc = new RTCPeerConnection({ bundlePolicy: "max-bundle" });
  pc.addTransceiver("video", { direction: "recvonly" });
  pc.addTransceiver("audio", { direction: "recvonly" });
  channel = pc.createDataChannel("input", { ordered: true });
  channel.onopen = () => {
    lastSize = "";
    sendSize();
  };
  channel.onmessage = (e) => onMessage(e.data);
  pc.ontrack = (e) => {
    stream.addTrack(e.track);
    video.srcObject = stream;
    video.muted = !soundOn;
    video.play().catch(() => {
      video.muted = true;
      video.play().catch(() => {});
    });
  };
  pc.onconnectionstatechange = () => {
    if (!current()) return;
    const s = pc.connectionState;
    if (s === "connected") {
      connectedOnce = true;
      retryDelay = 1000;
      delete statusEl.dataset.error;
      setStatus(null);
    } else if (s === "disconnected") {
      setStatus("Reconnecting…");
    } else if (s === "failed" || s === "closed") {
      retry();
    }
  };
  const base = location.pathname.replace(/[^/]*$/, "");
  ws = new WebSocket(`${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}${base}webrtc`);
  ws.onopen = async () => {
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    if (current()) ws.send(JSON.stringify({ type: "offer", sdp: pc.localDescription.sdp }));
  };
  ws.onmessage = async (e) => {
    if (!current()) return;
    const msg = JSON.parse(e.data);
    if (msg.type === "answer") await pc.setRemoteDescription({ type: "answer", sdp: msg.sdp });
    if (msg.type === "error") statusEl.dataset.error = msg.message;
  };
  ws.onclose = () => {
    if (current()) retry();
  };
}

const isMouse = (e) => e.pointerType !== "touch";

screenEl.addEventListener("pointerdown", (e) => {
  if (!isMouse(e)) return;
  e.preventDefault();
  screenEl.focus({ preventScroll: true });
  screenEl.setPointerCapture?.(e.pointerId);
  pointer(e.clientX, e.clientY, mask | (BUTTON_BITS[e.button] ?? 0));
});
screenEl.addEventListener("pointerup", (e) => {
  if (!isMouse(e)) return;
  e.preventDefault();
  pointer(e.clientX, e.clientY, mask & ~(BUTTON_BITS[e.button] ?? 0));
});
screenEl.addEventListener("pointermove", (e) => {
  if (isMouse(e)) pointer(e.clientX, e.clientY);
});
screenEl.addEventListener("contextmenu", (e) => e.preventDefault());
screenEl.addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    const unit = e.deltaMode === 0 ? 1 : WHEEL_LINE;
    wheelX += e.deltaX * unit;
    wheelY += e.deltaY * unit;
    if (Math.abs(wheelX) >= WHEEL_STEP) {
      wheelStep(e.clientX, e.clientY, wheelX < 0 ? 32 : 64);
      wheelX = 0;
    }
    if (Math.abs(wheelY) >= WHEEL_STEP) {
      wheelStep(e.clientX, e.clientY, wheelY < 0 ? 8 : 16);
      wheelY = 0;
    }
  },
  { passive: false },
);

const gestures = new GestureHandler();
gestures.attach(screenEl);
function onGesture(e) {
  const d = e.detail;
  const x = d.clientX;
  const y = d.clientY;
  if (e.type === "gesturestart") {
    if (document.activeElement !== textInput) screenEl.focus({ preventScroll: true });
    if (d.type === "onetap") click(x, y, 1);
    else if (d.type === "twotap") click(x, y, 4);
    else if (d.type === "threetap") click(x, y, 2);
    else if (d.type === "drag") pointer(x, y, mask | 1);
    else if (d.type === "longpress") pointer(x, y, mask | 4);
    else if (d.type === "twodrag") {
      gestureX = d.magnitudeX;
      gestureY = d.magnitudeY;
      pointer(x, y);
    } else if (d.type === "pinch") {
      gestureX = Math.hypot(d.magnitudeX, d.magnitudeY);
      pointer(x, y);
    }
  } else if (e.type === "gesturemove") {
    if (d.type === "drag" || d.type === "longpress") pointer(x, y);
    else if (d.type === "twodrag") {
      while (d.magnitudeY - gestureY > GESTURE_SCROLL) {
        wheelStep(x, y, 8);
        gestureY += GESTURE_SCROLL;
      }
      while (d.magnitudeY - gestureY < -GESTURE_SCROLL) {
        wheelStep(x, y, 16);
        gestureY -= GESTURE_SCROLL;
      }
      while (d.magnitudeX - gestureX > GESTURE_SCROLL) {
        wheelStep(x, y, 32);
        gestureX += GESTURE_SCROLL;
      }
      while (d.magnitudeX - gestureX < -GESTURE_SCROLL) {
        wheelStep(x, y, 64);
        gestureX -= GESTURE_SCROLL;
      }
    } else if (d.type === "pinch") {
      const magnitude = Math.hypot(d.magnitudeX, d.magnitudeY);
      if (Math.abs(magnitude - gestureX) > GESTURE_ZOOM) {
        send(["k", KeyTable.XK_Control_L, 1]);
        while (magnitude - gestureX > GESTURE_ZOOM) {
          wheelStep(x, y, 8);
          gestureX += GESTURE_ZOOM;
        }
        while (magnitude - gestureX < -GESTURE_ZOOM) {
          wheelStep(x, y, 16);
          gestureX -= GESTURE_ZOOM;
        }
        send(["k", KeyTable.XK_Control_L, 0]);
      }
    }
  } else if (d.type === "drag") pointer(x, y, mask & ~1);
  else if (d.type === "longpress") pointer(x, y, mask & ~4);
}
for (const type of ["gesturestart", "gesturemove", "gestureend"]) screenEl.addEventListener(type, onGesture);

const keyboard = new Keyboard(screenEl);
keyboard.onkeyevent = (keysym, code, down) => send(["k", keysym, down ? 1 : 0]);
keyboard.grab();

window.addEventListener(
  "keydown",
  (e) => {
    if (document.activeElement !== screenEl || e.code !== "KeyV" || !(e.ctrlKey || e.metaKey)) return;
    if (!navigator.clipboard?.readText) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    const viaMeta = e.metaKey && !e.ctrlKey;
    navigator.clipboard
      .readText()
      .then((text) => {
        if (text) send(["c", text]);
      })
      .catch(() => {})
      .finally(() => {
        if (viaMeta) send(["k", KeyTable.XK_Control_L, 1]);
        key(KeyTable.XK_v);
        if (viaMeta) send(["k", KeyTable.XK_Control_L, 0]);
      });
  },
  true,
);

function resetTextInput() {
  textInput.value = TEXT_SENTINEL;
  lastText = TEXT_SENTINEL;
  textInput.setSelectionRange(TEXT_SENTINEL.length, TEXT_SENTINEL.length);
}

textInput.addEventListener("input", () => {
  const value = textInput.value;
  let added = value.length - lastText.length;
  let removed = added < 0 ? -added : 0;
  for (let i = 0; i < Math.min(value.length, lastText.length); i++) {
    if (value[i] !== lastText[i]) {
      added = value.length - i;
      removed = lastText.length - i;
      break;
    }
  }
  for (let i = 0; i < removed; i++) key(KeyTable.XK_BackSpace);
  for (const ch of value.slice(value.length - Math.max(added, 0))) {
    key(ch === "\n" ? KeyTable.XK_Return : keysyms.lookup(ch.codePointAt(0)));
  }
  if (value.length < 1 || value.length > 2 * TEXT_SENTINEL.length) resetTextInput();
  else lastText = value;
});
textInput.addEventListener("keydown", (e) => {
  const special = { Enter: KeyTable.XK_Return, Escape: KeyTable.XK_Escape, Tab: KeyTable.XK_Tab }[e.key];
  if (special) {
    e.preventDefault();
    key(special);
  }
});
textInput.addEventListener("focus", resetTextInput);

if (navigator.maxTouchPoints > 0 || "ontouchstart" in window) keyboardButton.hidden = false;
keyboardButton.addEventListener("pointerdown", (e) => e.preventDefault());
keyboardButton.addEventListener("click", () => {
  if (document.activeElement === textInput) {
    textInput.blur();
    screenEl.focus({ preventScroll: true });
  } else {
    textInput.focus();
  }
});

window.addEventListener("message", (e) => {
  if (e.origin !== location.origin || typeof e.data?.sound !== "boolean") return;
  soundOn = e.data.sound;
  video.muted = !soundOn;
  if (soundOn) video.play().catch(() => {});
});

let resizeTimer = 0;
window.addEventListener("resize", () => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(sendSize, 250);
});

screenEl.focus({ preventScroll: true });
connect();
