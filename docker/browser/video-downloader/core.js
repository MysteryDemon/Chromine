(() => {
  if (globalThis.chromineVideo) return;

  const TOP = window === window.top;
  const SVG = "http://www.w3.org/2000/svg";
  const MIN_W = 200;
  const MIN_H = 110;
  const NO_REPLY = 20000;
  const QUALITIES = [
    [0, "Best"],
    [1080, "1080p"],
    [720, "720p"],
    [480, "480p"],
    [360, "360p"],
  ];
  const MESSAGES = {
    done: "Download complete",
    unsupported: "This website is not supported",
    protected: "This video is DRM-protected and can't be downloaded",
    unavailable: "Downloading isn't available: the Chromine helper isn't connected",
    failed: "Download failed",
  };

  const qualityListeners = new Set();
  const downloadListeners = new Set();
  const showListeners = new Set();
  const dl = { ref: null, state: "idle", progress: null, replied: false, video: null };
  let watchdog = 0;
  let reported = "";

  function send(msg) {
    try {
      chrome.runtime.sendMessage(msg).catch(() => {});
    } catch {}
  }

  function icon(paths, extra) {
    const svg = document.createElementNS(SVG, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "2");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.setAttribute("aria-hidden", "true");
    for (const d of paths) {
      const p = document.createElementNS(SVG, "path");
      p.setAttribute("d", d);
      svg.appendChild(p);
    }
    if (extra) svg.appendChild(extra);
    return svg;
  }

  function circle(cx, cy, r) {
    const c = document.createElementNS(SVG, "circle");
    c.setAttribute("cx", String(cx));
    c.setAttribute("cy", String(cy));
    c.setAttribute("r", String(r));
    return c;
  }

  const ICONS = {
    download: () => icon(["M12 15V3", "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4", "m7 10 5 5 5-5"]),
    done: () => icon(["M20 6 9 17l-5-5"]),
    error: () => icon(["M12 8v4", "M12 16h.01"], circle(12, 12, 10)),
    check: () => icon(["M20 6 9 17l-5-5"]),
    chevron: () => icon(["m6 9 6 6 6-6"]),
  };

  function large(v) {
    const r = v.getBoundingClientRect();
    return r.width >= MIN_W && r.height >= MIN_H;
  }

  function loaded(v) {
    return v.readyState > 0 || !!v.currentSrc || !!v.src || !!v.querySelector("source[src]");
  }

  function findVideo() {
    for (const v of document.querySelectorAll("video")) if (large(v) && loaded(v)) return v;
    return null;
  }

  function mediaUrl(v) {
    const s = v.currentSrc || v.src || (v.querySelector("source[src]") || {}).src || "";
    return /^https?:\/\//i.test(s) ? s : "";
  }

  function labelFor(q) {
    return (QUALITIES.find(([v]) => v === q) || QUALITIES[0])[1];
  }

  function ours(ev) {
    return ev.composedPath().some((n) => n instanceof Element && n.tagName.startsWith("CHROMINE-"));
  }

  function notify(final) {
    for (const fn of downloadListeners) fn(dl, final);
  }

  function finish(state) {
    clearTimeout(watchdog);
    dl.ref = null;
    dl.state = state in MESSAGES ? state : "failed";
    dl.progress = null;
    notify(true);
  }

  const C = {
    TOP,
    SVG,
    MIN_W,
    MIN_H,
    QUALITIES,
    MESSAGES,
    ICONS,
    quality: 0,
    send,
    circle,
    large,
    loaded,
    findVideo,
    mediaUrl,
    labelFor,
    ours,
    setQuality(value, save) {
      C.quality = QUALITIES.some(([q]) => q === value) ? value : 0;
      for (const fn of qualityListeners) fn(C.quality);
      if (!save) return;
      try {
        chrome.storage.local.set({ quality: C.quality }).catch(() => {});
      } catch {}
    },
    onQuality(fn) {
      qualityListeners.add(fn);
    },
    onShow(fn) {
      showListeners.add(fn);
    },
    download: {
      state: dl,
      get busy() {
        return !!dl.ref;
      },
      subscribe(fn) {
        downloadListeners.add(fn);
        return () => downloadListeners.delete(fn);
      },
      start(video) {
        if (dl.ref) return false;
        dl.video = video || null;
        if (video && video.mediaKeys) {
          finish("protected");
          return false;
        }
        dl.ref = "r" + Date.now().toString(36) + Math.random().toString(36).slice(2, 10);
        dl.state = "queued";
        dl.progress = null;
        dl.replied = false;
        notify(false);
        clearTimeout(watchdog);
        watchdog = setTimeout(() => {
          if (dl.ref && !dl.replied) finish("unavailable");
        }, NO_REPLY);
        send({ type: "download", ref: dl.ref, page: location.href, media: video ? mediaUrl(video) : "", quality: C.quality });
        return true;
      },
      status(m) {
        if (!dl.ref || m.ref !== dl.ref) return;
        dl.replied = true;
        if (m.state === "queued" || m.state === "working") {
          dl.state = m.state;
          dl.progress = typeof m.progress === "number" ? m.progress : null;
          notify(false);
        } else {
          finish(m.state);
        }
      },
    },
  };
  globalThis.chromineVideo = C;

  function check() {
    const v = findVideo();
    if (!v) return;
    const media = mediaUrl(v);
    const key = `${location.href}|${media}`;
    if (key !== reported) {
      reported = key;
      send({ type: "video", frame: location.href, media });
    }
  }

  let pendingCheck = 0;
  C.changed = new Set();
  function soon() {
    if (pendingCheck) return;
    pendingCheck = setTimeout(() => {
      pendingCheck = 0;
      check();
      for (const fn of C.changed) fn();
    }, 800);
  }
  C.soon = soon;

  try {
    chrome.storage.local.get("quality").then((s) => C.setQuality(s.quality, false), () => {});
    chrome.storage.onChanged.addListener((c, area) => {
      if (area === "local" && c.quality) C.setQuality(c.quality.newValue, false);
    });
    chrome.runtime.onMessage.addListener((m) => {
      if (!m || typeof m.type !== "string") return;
      if (m.type === "status") C.download.status(m);
      else if (m.type === "show" && TOP) for (const fn of showListeners) fn();
    });
  } catch {}

  if (TOP) send({ type: "page", url: location.href });
  for (const ev of ["loadedmetadata", "play", "playing"]) document.addEventListener(ev, soon, true);
  new MutationObserver(soon).observe(document.documentElement, { childList: true, subtree: true });
  check();
  setTimeout(check, 2500);
  setTimeout(() => {
    reported = "";
    check();
  }, 5000);
})();
