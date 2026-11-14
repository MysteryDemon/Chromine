"use strict";

const HOST = "com.chromine.agent";
let port = null;
let progressTimer = null;

function toMs(iso) {
  const t = Date.parse(iso || "");
  return Number.isFinite(t) ? t : 0;
}

function web(url) {
  return /^https?:\/\//i.test(url || "") && url.length <= 4096 ? url : "";
}

function describe(item) {
  return {
    id: item.id,
    start_ms: toMs(item.startTime),
    filename: item.filename || "",
    state: item.state,
    received: item.bytesReceived || 0,
    total: item.totalBytes > 0 ? item.totalBytes : 0,
    file_size: item.fileSize > 0 ? item.fileSize : 0,
    exists: item.exists !== false,
    error: item.error || null,
    incognito: !!item.incognito,
    url: web(item.url),
    referrer: web(item.referrer),
  };
}

function send(message) {
  if (!port) connect();
  try {
    port.postMessage(message);
  } catch (e) {
    port = null;
  }
}

function connect() {
  if (port) return;
  port = chrome.runtime.connectNative(HOST);
  port.onMessage.addListener((msg) => {
    if (msg && msg.cmd === "cancel" && Number.isInteger(msg.id)) {
      chrome.downloads.cancel(msg.id).catch(() => {});
    }
  });
  port.onDisconnect.addListener(() => {
    port = null;
    setTimeout(connect, 2000);
  });
  poll();
  chrome.downloads.search({ state: "complete", orderBy: ["-endTime"], limit: 50 }, (items) => {
    items.forEach((i) => send({ event: "complete", download: describe(i), replay: true }));
  });
}

function poll() {
  chrome.downloads.search({ state: "in_progress" }, (items) => {
    send({ event: "snapshot", downloads: items.map(describe) });
    if (items.length) startProgress();
    else stopProgress();
  });
}

function startProgress() {
  if (!progressTimer) progressTimer = setInterval(poll, 2000);
}

function stopProgress() {
  clearInterval(progressTimer);
  progressTimer = null;
}

chrome.downloads.onCreated.addListener((item) => {
  send({ event: "created", download: describe(item) });
  startProgress();
});

chrome.downloads.onChanged.addListener((delta) => {
  const state = delta.state && delta.state.current;
  if (!state && !delta.filename) return;
  chrome.downloads.search({ id: delta.id }, (items) => {
    const item = items && items[0];
    if (item && state === "complete") send({ event: "complete", download: describe(item) });
    else if (item && state === "interrupted") send({ event: "interrupted", download: describe(item) });
    else if (item && item.state === "in_progress") send({ event: "progress", download: describe(item) });
    poll();
  });
});

chrome.downloads.onErased.addListener(poll);

chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
connect();
