(() => {
  const C = globalThis.chromineVideo;
  if (!C || C.player) return;
  C.player = true;

  const TAG = "chromine-player-download";
  const PAD = 12;
  const IDLE = 3000;
  const TICK = 250;
  const NOTE_FOR = 4500;
  const KNOWN = [
    [".html5-video-player", (r) => r.classList.contains("ytp-autohide")],
    [".video-js", (r) => r.classList.contains("vjs-user-inactive") && r.classList.contains("vjs-playing")],
    [".jwplayer", (r) => r.classList.contains("jw-flag-user-inactive") && r.classList.contains("jw-state-playing")],
    [".plyr", (r) => r.classList.contains("plyr--hide-controls")],
  ];
  const INTERACTIVE = "button, [role=button], [role=slider], [role=menuitem], input[type=range]";
  const SOLID = "button, a[href], input, select, textarea, img, svg, canvas, [role=button], [role=slider], [tabindex]";
  const STOP = ["click", "dblclick", "auxclick", "contextmenu", "mousedown", "mouseup", "pointerdown", "pointerup", "touchstart", "touchend", "keydown", "keyup", "keypress", "wheel"];
  const ACTIVITY = ["pointermove", "pointerdown", "touchstart", "keydown", "wheel"];
  const RING = 2 * Math.PI * 10;

  const CSS = `
    .ui { --s: 40px; --fg: #fff; --font: ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
      display: flex; flex-direction: column; align-items: flex-start; gap: 8px; color: var(--fg); font: 500 13px/1.2 var(--font);
      opacity: 0; visibility: hidden; transition: opacity 0.2s ease, visibility 0s linear 0.2s; user-select: none; -webkit-user-select: none; }
    .ui.on { opacity: 1; visibility: visible; transition: opacity 0.2s ease; }
    .ui.on .bar, .ui.on .menu { pointer-events: auto; }
    .row { display: flex; align-items: center; gap: 8px; }
    .bar { display: inline-flex; align-items: center; height: var(--s); border-radius: calc(var(--s) / 2); background: rgba(0, 0, 0, 0.55);
      box-shadow: 0 0 0 1px rgba(255, 255, 255, 0.08), 0 4px 16px rgba(0, 0, 0, 0.35); backdrop-filter: blur(8px); -webkit-backdrop-filter: blur(8px); }
    button { font: inherit; color: inherit; background: transparent; border: 0; margin: 0; padding: 0; cursor: pointer; height: var(--s);
      display: inline-flex; align-items: center; justify-content: center; border-radius: calc(var(--s) / 2); transition: background 0.15s ease; }
    button:focus-visible { outline: 2px solid #fff; outline-offset: 2px; box-shadow: 0 0 0 4px rgba(0, 0, 0, 0.7); }
    .dl { position: relative; width: var(--s); }
    .dl:hover, .q:hover, .q[aria-expanded="true"] { background: rgba(255, 255, 255, 0.14); }
    .dl > svg.main { width: calc(var(--s) * 0.5); height: calc(var(--s) * 0.5); filter: drop-shadow(0 1px 1px rgba(0, 0, 0, 0.5)); }
    .ring { position: absolute; inset: 0; margin: auto; width: calc(var(--s) - 6px); height: calc(var(--s) - 6px); transform: rotate(-90deg); opacity: 0; pointer-events: none; }
    .ring circle { fill: none; stroke-width: 2; }
    .ring .track { stroke: rgba(255, 255, 255, 0.2); }
    .ring .fill { stroke: currentColor; stroke-linecap: round; transition: stroke-dashoffset 0.4s ease; }
    .dl.working .ring { opacity: 1; }
    .dl.working.spin .ring { animation: spin 1s linear infinite; }
    .dl.working > svg.main { opacity: 0.6; }
    .dl.done { color: #3fd68b; }
    .dl.error { color: #f5b544; }
    .dl[aria-disabled="true"] > svg.main { opacity: 0.55; }
    .sep { width: 1px; height: 46%; background: rgba(255, 255, 255, 0.25); }
    .q { gap: 2px; padding: 0 10px 0 8px; font-size: max(12px, calc(var(--s) * 0.32)); font-weight: 600; font-variant-numeric: tabular-nums; white-space: nowrap; }
    .q > svg { width: 14px; height: 14px; transition: transform 0.15s ease; }
    .q[aria-expanded="true"] > svg { transform: rotate(180deg); }
    .note { max-width: min(340px, 55vw); padding: 7px 12px; border-radius: 8px; background: rgba(0, 0, 0, 0.75); opacity: 0; transition: opacity 0.2s ease; }
    .note.on { opacity: 1; }
    .menu { min-width: 150px; padding: 6px; border-radius: 12px; background: rgba(28, 28, 28, 0.92); box-shadow: 0 8px 28px rgba(0, 0, 0, 0.5);
      overflow-y: auto; backdrop-filter: blur(8px); -webkit-backdrop-filter: blur(8px); }
    .menu[hidden] { display: none; }
    .label { padding: 6px 10px; font-size: 11px; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; color: rgba(255, 255, 255, 0.6); }
    .item { width: 100%; height: auto; justify-content: space-between; gap: 16px; padding: 8px 10px; border-radius: 8px; font-size: 14px; text-align: left; }
    .item:hover, .item:focus-visible { background: rgba(255, 255, 255, 0.14); outline: none; box-shadow: none; }
    .item > svg { width: 16px; height: 16px; opacity: 0; }
    .item[aria-checked="true"] > svg { opacity: 1; }
    @keyframes spin { to { transform: rotate(270deg); } }
    @media (prefers-reduced-motion: reduce) { .ui, .ui.on, .ring .fill, .note, button, .q > svg { transition: none; } .dl.working.spin .ring { animation: none; } }
  `;
  const sheet = new CSSStyleSheet();
  sheet.replaceSync(CSS);

  function area(el) {
    const r = el.getBoundingClientRect();
    return r.width * r.height;
  }

  function alpha(color) {
    const m = color.match(/rgba?\(([^)]+)\)/);
    if (!m) return 0;
    const parts = m[1].split(/[\s,/]+/).filter(Boolean);
    return parts.length > 3 ? parseFloat(parts[3]) : 1;
  }

  function playerFor(video) {
    for (const [selector, hidden] of KNOWN) {
      const root = video.closest(selector);
      if (root) return { root, hidden };
    }
    const vr = video.getBoundingClientRect();
    let root = null;
    for (let el = video.parentElement; el && el !== document.body && el !== document.documentElement; el = el.parentElement) {
      const r = el.getBoundingClientRect();
      const covers = r.left <= vr.left + 2 && r.top <= vr.top + 2 && r.right >= vr.right - 2 && r.bottom >= vr.bottom - 2;
      if (!covers || r.width > vr.width + 16 || r.height > vr.height * 1.4 + 16) break;
      if (getComputedStyle(el).position !== "static") root = el;
    }
    return { root, hidden: null };
  }

  function findControls(root, video) {
    const counts = new Map();
    for (const el of root.querySelectorAll(INTERACTIVE)) {
      for (let e = el.parentElement; e && e !== root; e = e.parentElement) counts.set(e, (counts.get(e) || 0) + 1);
    }
    let best = null;
    let most = 1;
    for (const [el, n] of counts) {
      if (el.contains(video) || el.tagName === TAG.toUpperCase()) continue;
      if (n > most || (n === most && best && best.contains(el))) {
        best = el;
        most = n;
      }
    }
    return most >= 2 ? best : null;
  }

  function shown(el, root) {
    const stop = root.parentElement;
    for (let e = el; e && e !== stop; e = e.parentElement) {
      const cs = getComputedStyle(e);
      if (cs.display === "none" || cs.visibility === "hidden" || parseFloat(cs.opacity) < 0.15) return false;
    }
    const r = el.getBoundingClientRect();
    const rr = root.getBoundingClientRect();
    return r.width > 0 && r.height > 0 && r.bottom > rr.top + 2 && r.top < rr.bottom - 2;
  }

  function obstacles(box, video, host, rect) {
    const limit = box.clientWidth * box.clientHeight * 0.4;
    const out = [];
    for (const el of box.querySelectorAll("*")) {
      if (el === host || el === video || el.contains(video)) continue;
      const r = el.getBoundingClientRect();
      if (!r.width || !r.height || r.right <= rect.left || r.left >= rect.right || r.bottom <= rect.top || r.top >= rect.bottom) continue;
      if (r.width * r.height > limit) continue;
      const cs = getComputedStyle(el);
      const text = [...el.childNodes].some((n) => n.nodeType === 3 && n.textContent.trim());
      if (cs.pointerEvents === "none" && !text) continue;
      if (text || el.matches(SOLID) || cs.backgroundImage !== "none" || alpha(cs.backgroundColor) > 0.1) out.push(r);
    }
    return out;
  }

  class Control {
    constructor(video) {
      this.video = video;
      this.root = null;
      this.hidden = null;
      this.controls = null;
      this.visible = false;
      this.dirty = true;
      this.focused = false;
      this.lastActivity = 0;
      this.noteUntil = 0;
      this.ticks = 0;
      this.activity = () => {
        this.lastActivity = performance.now();
        this.sync();
      };
      this.media = () => this.sync();
      this.build();
      for (const t of ["play", "pause", "ended", "playing"]) video.addEventListener(t, this.media);
      this.unsubscribe = C.download.subscribe((d, final) => this.render(d, final));
      this.attach();
      this.render(C.download.state, false);
    }

    build() {
      this.host = document.createElement(TAG);
      this.host.style.cssText = "all: initial; position: absolute; left: 12px; top: 12px; z-index: 2147483000; display: block; pointer-events: none;";
      for (const t of STOP) this.host.addEventListener(t, (e) => e.stopPropagation());
      for (const t of ACTIVITY) this.host.addEventListener(t, this.activity, { passive: true });
      this.shadow = this.host.attachShadow({ mode: "closed" });
      this.shadow.adoptedStyleSheets = [sheet];

      this.ui = document.createElement("div");
      this.ui.className = "ui";
      const row = document.createElement("div");
      row.className = "row";
      this.bar = document.createElement("div");
      this.bar.className = "bar";

      this.dl = document.createElement("button");
      this.dl.type = "button";
      this.dl.className = "dl";
      const ring = document.createElementNS(C.SVG, "svg");
      ring.setAttribute("class", "ring");
      ring.setAttribute("viewBox", "0 0 24 24");
      const track = C.circle(12, 12, 10);
      track.setAttribute("class", "track");
      this.fill = C.circle(12, 12, 10);
      this.fill.setAttribute("class", "fill");
      this.fill.style.strokeDasharray = `${RING}`;
      this.fill.style.strokeDashoffset = `${RING}`;
      ring.append(track, this.fill);
      this.glyphs = {};
      for (const name of ["download", "done", "error"]) {
        this.glyphs[name] = C.ICONS[name]();
        this.glyphs[name].setAttribute("class", "main");
      }
      this.glyph = this.glyphs.download;
      this.dl.append(ring, this.glyph);

      const sep = document.createElement("span");
      sep.className = "sep";
      sep.setAttribute("aria-hidden", "true");

      this.q = document.createElement("button");
      this.q.type = "button";
      this.q.className = "q";
      this.q.title = "Video quality";
      this.q.setAttribute("aria-haspopup", "menu");
      this.q.setAttribute("aria-expanded", "false");
      this.qText = document.createElement("span");
      this.q.append(this.qText, C.ICONS.chevron());
      this.bar.append(this.dl, sep, this.q);

      this.noteEl = document.createElement("div");
      this.noteEl.className = "note";
      this.noteEl.setAttribute("role", "status");
      this.noteEl.setAttribute("aria-live", "polite");
      row.append(this.bar, this.noteEl);

      this.menu = document.createElement("div");
      this.menu.className = "menu";
      this.menu.hidden = true;
      this.menu.setAttribute("role", "menu");
      this.menu.setAttribute("aria-label", "Video quality");
      const label = document.createElement("div");
      label.className = "label";
      label.setAttribute("aria-hidden", "true");
      label.textContent = "Quality";
      this.menu.appendChild(label);
      for (const [value, text] of C.QUALITIES) {
        const item = document.createElement("button");
        item.type = "button";
        item.className = "item";
        item.tabIndex = -1;
        item.dataset.value = String(value);
        item.setAttribute("role", "menuitemradio");
        const name = document.createElement("span");
        name.textContent = text;
        item.append(name, C.ICONS.check());
        item.addEventListener("click", (ev) => {
          if (!ev.isTrusted) return;
          C.setQuality(value, true);
          this.closeMenu(true);
        });
        this.menu.appendChild(item);
      }
      this.ui.append(row, this.menu);
      this.shadow.appendChild(this.ui);

      this.dl.addEventListener("click", (ev) => {
        if (!ev.isTrusted) return;
        this.closeMenu(false);
        if (!C.download.busy) C.download.start(this.video);
      });
      this.q.addEventListener("click", (ev) => {
        if (!ev.isTrusted) return;
        if (this.menu.hidden) this.openMenu();
        else this.closeMenu(true);
      });
      this.menu.addEventListener("keydown", (ev) => this.menuKeys(ev));
      this.shadow.addEventListener("focusin", (ev) => {
        this.focused = ev.target instanceof Element && ev.target.matches(":focus-visible");
        this.sync();
      });
      this.shadow.addEventListener("focusout", (ev) => {
        if (!ev.relatedTarget || !this.shadow.contains(ev.relatedTarget)) {
          this.focused = false;
          this.sync();
        }
      });
      this.outside = (ev) => {
        if (!ev.composedPath().includes(this.host)) this.closeMenu(false);
      };
      this.refresh = () => {
        const text = C.labelFor(C.quality);
        this.qText.textContent = text;
        this.q.setAttribute("aria-label", `Video quality: ${text}`);
        for (const item of this.items()) item.setAttribute("aria-checked", String(Number(item.dataset.value) === C.quality));
      };
      C.onQuality(this.refresh);
      this.refresh();
    }

    attach() {
      const { root, hidden } = playerFor(this.video);
      if (root === this.root && this.host.isConnected) return;
      const watched = this.root || this.video;
      for (const t of ACTIVITY) watched.removeEventListener(t, this.activity, true);
      this.root = root;
      this.hidden = hidden;
      this.controls = root && !hidden ? findControls(root, this.video) : null;
      for (const t of ACTIVITY) (root || this.video).addEventListener(t, this.activity, { capture: true, passive: true });
      this.visible = false;
      this.ui.classList.remove("on");
      this.style();
      this.sync();
    }

    style() {
      const source = this.root || this.video.parentElement;
      if (!source) return;
      const cs = getComputedStyle(source);
      if (cs.fontFamily) this.ui.style.setProperty("--font", cs.fontFamily);
      const button = (this.controls || this.root || source).querySelector("button, [role=button]");
      const h = button ? button.getBoundingClientRect().height : 0;
      if (h >= 24) this.ui.style.setProperty("--s", `${Math.round(Math.min(44, Math.max(34, h)))}px`);
    }

    container() {
      const fs = document.fullscreenElement;
      if (fs === this.video) return null;
      if (fs && fs.contains(this.video) && !(this.root && fs.contains(this.root))) return fs;
      return this.root || document.documentElement;
    }

    wanted() {
      if (!this.video.isConnected || !C.large(this.video)) return false;
      if (!this.root && this.offscreen) return false;
      if (!this.menu.hidden || this.focused || performance.now() < this.noteUntil) return true;
      if (this.hidden) return !this.hidden(this.root);
      if (this.controls) {
        if (this.controls.isConnected && this.root.contains(this.controls)) return shown(this.controls, this.root);
        this.controls = null;
      }
      return this.video.paused || this.video.ended || performance.now() - this.lastActivity < IDLE;
    }

    sync() {
      const box = this.container();
      if (!box) {
        this.show(false);
        return;
      }
      if (this.host.parentNode !== box) {
        box.appendChild(this.host);
        this.dirty = true;
      }
      const fixed = !this.root;
      this.host.style.position = fixed ? "fixed" : "absolute";
      if (fixed) this.follow();
      if (this.root && !this.hidden && !this.controls && ++this.ticks % 8 === 0) this.controls = findControls(this.root, this.video);
      if (this.video.mediaKeys) {
        this.dl.setAttribute("aria-disabled", "true");
        this.dl.title = C.MESSAGES.protected;
      } else {
        this.dl.removeAttribute("aria-disabled");
        this.dl.title = "Download video";
      }
      const want = this.wanted();
      if (want && (!this.visible || this.dirty) && !fixed) {
        this.place(box);
        this.dirty = false;
      }
      this.show(want);
    }

    follow() {
      const r = this.video.getBoundingClientRect();
      this.offscreen = r.bottom < 40 || r.top > innerHeight - 40 || r.right < 40 || r.left > innerWidth - 40;
      this.host.style.left = `${Math.round(r.left + PAD)}px`;
      this.host.style.top = `${Math.round(r.top + PAD)}px`;
    }

    place(box) {
      const cr = box.getBoundingClientRect();
      const w = this.bar.offsetWidth || 100;
      const h = this.bar.offsetHeight || 40;
      let y = PAD;
      for (let i = 0; i < 4; i++) {
        const hits = obstacles(box, this.video, this.host, { left: cr.left + PAD, top: cr.top + y, right: cr.left + PAD + w, bottom: cr.top + y + h });
        if (!hits.length) break;
        y = Math.round(Math.max(...hits.map((r) => r.bottom)) - cr.top + 8);
      }
      if (y + h > cr.height * 0.5) y = PAD;
      this.host.style.left = `${PAD}px`;
      this.host.style.top = `${y}px`;
    }

    show(on) {
      if (on === this.visible) return;
      this.visible = on;
      this.ui.classList.toggle("on", on);
      if (!on) this.closeMenu(false);
    }

    items() {
      return [...this.menu.querySelectorAll(".item")];
    }

    openMenu() {
      const box = this.host.parentNode instanceof Element ? this.host.parentNode : document.documentElement;
      const bottom = box === document.documentElement ? innerHeight : box.getBoundingClientRect().bottom;
      this.menu.style.maxHeight = `${Math.max(96, Math.round(bottom - this.bar.getBoundingClientRect().bottom - 16))}px`;
      this.menu.hidden = false;
      this.q.setAttribute("aria-expanded", "true");
      const list = this.items();
      (list.find((i) => i.getAttribute("aria-checked") === "true") || list[0]).focus();
      window.addEventListener("pointerdown", this.outside, true);
      this.sync();
    }

    closeMenu(focusButton) {
      if (this.menu.hidden) return;
      this.menu.hidden = true;
      this.q.setAttribute("aria-expanded", "false");
      window.removeEventListener("pointerdown", this.outside, true);
      if (focusButton) this.q.focus();
    }

    menuKeys(ev) {
      const list = this.items();
      const at = list.indexOf(this.shadow.activeElement);
      if (ev.key === "ArrowDown" || ev.key === "ArrowUp") list[(at + (ev.key === "ArrowDown" ? 1 : -1) + list.length) % list.length].focus();
      else if (ev.key === "Home" || ev.key === "End") list[ev.key === "Home" ? 0 : list.length - 1].focus();
      else if (ev.key === "Escape" || ev.key === "Tab") this.closeMenu(true);
      else return;
      ev.preventDefault();
    }

    setGlyph(name) {
      const next = this.glyphs[name];
      if (next !== this.glyph) {
        this.dl.replaceChild(next, this.glyph);
        this.glyph = next;
      }
    }

    note(text) {
      clearTimeout(this.noteTimer);
      this.noteEl.textContent = text;
      this.noteEl.classList.add("on");
      this.noteUntil = performance.now() + NOTE_FOR;
      this.noteTimer = setTimeout(() => {
        this.noteEl.classList.remove("on");
        this.dl.classList.remove("done", "error");
        this.setGlyph("download");
        this.sync();
      }, NOTE_FOR);
      this.sync();
    }

    render(d, final) {
      if (d.ref) {
        this.dl.classList.remove("done", "error");
        this.dl.classList.add("working");
        this.setGlyph("download");
        if (typeof d.progress === "number" && d.progress > 0) {
          this.dl.classList.remove("spin");
          this.fill.style.strokeDashoffset = `${RING * (1 - Math.min(d.progress, 100) / 100)}`;
          this.dl.setAttribute("aria-label", `Downloading video, ${Math.round(d.progress)}%`);
        } else {
          this.dl.classList.add("spin");
          this.fill.style.strokeDashoffset = `${RING * 0.72}`;
          this.dl.setAttribute("aria-label", "Downloading video");
        }
        return;
      }
      this.dl.classList.remove("working", "spin");
      this.fill.style.strokeDashoffset = `${RING}`;
      this.dl.setAttribute("aria-label", "Download video");
      if (!final || d.video !== this.video) return;
      const kind = d.state === "done" ? "done" : "error";
      this.dl.classList.remove("done", "error");
      this.dl.classList.add(kind);
      this.setGlyph(kind);
      this.note(C.MESSAGES[d.state]);
    }

    destroy() {
      clearTimeout(this.noteTimer);
      this.closeMenu(false);
      for (const t of ACTIVITY) (this.root || this.video).removeEventListener(t, this.activity, true);
      for (const t of ["play", "pause", "ended", "playing"]) this.video.removeEventListener(t, this.media);
      this.unsubscribe();
      this.host.remove();
    }
  }

  const controls = new Map();
  let timer = 0;

  function eligible(v) {
    return v.isConnected && C.large(v) && (C.loaded(v) || KNOWN.some(([s]) => v.closest(s)));
  }

  function scan() {
    const vids = [...document.querySelectorAll("video")].filter(eligible).sort((a, b) => area(b) - area(a));
    let main = vids[0] || null;
    for (const v of controls.keys()) if (main && v !== main && vids.includes(v) && area(v) >= area(main) * 0.7) main = v;
    for (const [v, c] of controls) {
      if (v !== main) {
        c.destroy();
        controls.delete(v);
      }
    }
    if (main && !controls.has(main)) controls.set(main, new Control(main));
    for (const c of controls.values()) c.attach();
    const mine = new Set([...controls.values()].map((c) => c.host));
    for (const h of document.querySelectorAll(TAG)) if (!mine.has(h)) h.remove();
    if (controls.size && !timer) timer = setInterval(tick, TICK);
    if (!controls.size && timer) {
      clearInterval(timer);
      timer = 0;
    }
  }

  function tick() {
    if (document.hidden) return;
    for (const c of controls.values()) c.sync();
  }

  let scrollQueued = false;
  function onScroll() {
    if (scrollQueued) return;
    scrollQueued = true;
    requestAnimationFrame(() => {
      scrollQueued = false;
      for (const c of controls.values()) if (!c.root) c.follow();
    });
  }

  C.changed.add(scan);
  document.addEventListener("fullscreenchange", () => {
    scan();
    for (const c of controls.values()) {
      c.dirty = true;
      c.sync();
    }
  });
  window.addEventListener("resize", () => {
    for (const c of controls.values()) c.dirty = true;
    tick();
  });
  window.addEventListener("scroll", onScroll, { capture: true, passive: true });
  scan();
  setTimeout(scan, 1500);
})();
