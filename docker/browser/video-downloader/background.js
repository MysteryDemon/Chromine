const HOST = "com.chromine.video";
const FINAL = new Set(["done", "unsupported", "protected", "unavailable", "failed"]);
const QUALITIES = new Set([360, 480, 720, 1080]);
const tabs = new Map();
const pending = new Map();
let port = null;

function entry(tabId) {
  let e = tabs.get(tabId);
  if (!e) {
    e = { page: "", frames: new Set(), media: new Set() };
    tabs.set(tabId, e);
  }
  return e;
}

function toTab(tabId, msg, frameId = 0) {
  chrome.tabs.sendMessage(tabId, msg, { frameId }).catch(() => {});
}

function reply(ref, msg) {
  const to = pending.get(ref);
  if (!to) return;
  toTab(to.tabId, { type: "status", ref, ...msg }, to.frameId);
  if (FINAL.has(msg.state)) pending.delete(ref);
}

function connect() {
  if (port) return port;
  port = chrome.runtime.connectNative(HOST);
  port.onMessage.addListener((m) => {
    if (!m || m.type !== "status" || !pending.has(m.ref)) return;
    reply(m.ref, { state: m.state, progress: typeof m.progress === "number" ? m.progress : null });
    if (pending.size === 0 && port) {
      port.disconnect();
      port = null;
    }
  });
  port.onDisconnect.addListener(() => {
    const why = (chrome.runtime.lastError && chrome.runtime.lastError.message) || "";
    const state = /not found|forbidden|not allowed/i.test(why) ? "unavailable" : "failed";
    port = null;
    for (const ref of [...pending.keys()]) reply(ref, { state, progress: null });
  });
  return port;
}

function httpUrl(u) {
  return typeof u === "string" && /^https?:\/\//i.test(u) && u.length <= 4096;
}

chrome.runtime.onMessage.addListener((msg, sender) => {
  const tabId = sender.tab && sender.tab.id;
  const frameId = sender.frameId || 0;
  if (tabId === undefined || !msg || typeof msg.type !== "string") return;
  if (msg.type === "page" && frameId === 0) {
    const e = entry(tabId);
    if (e.page !== msg.url) {
      e.page = msg.url;
      e.frames.clear();
      e.media.clear();
    }
    return;
  }
  if (msg.type === "video") {
    const e = entry(tabId);
    if (frameId !== 0 && httpUrl(msg.frame)) e.frames.add(msg.frame);
    if (httpUrl(msg.media)) e.media.add(msg.media);
    return;
  }
  if (msg.type === "reveal") {
    toTab(tabId, { type: "show" });
    return;
  }
  if (msg.type === "download" && typeof msg.ref === "string" && msg.ref.length <= 100) {
    const e = entry(tabId);
    const urls = [...new Set([msg.page, msg.media, e.page, ...e.frames, ...e.media].filter(httpUrl))];
    pending.set(msg.ref, { tabId, frameId });
    if (!urls.length) {
      reply(msg.ref, { state: "unsupported", progress: null });
      return;
    }
    try {
      const quality = QUALITIES.has(msg.quality) ? msg.quality : null;
      connect().postMessage({ type: "download", ref: msg.ref, urls, quality });
    } catch {
      reply(msg.ref, { state: "failed", progress: null });
    }
  }
});

chrome.tabs.onRemoved.addListener((tabId) => tabs.delete(tabId));
