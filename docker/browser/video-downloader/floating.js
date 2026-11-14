(() => {
  const C = globalThis.chromineVideo;
  if (!C || C.floating) return;
  C.floating = true;

  const RING = 2 * Math.PI * 24;
  const TAP_GAP = 600;
  const TAP_RANGE = 40;
  const HIDE_AFTER = 5000;

  const CSS = `
    :host { all: initial; }
    .wrap { display: flex; align-items: center; gap: 10px; font: 500 13px/1.25 ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; }
    .btn { position: relative; width: 52px; height: 52px; padding: 0; border: 0; border-radius: 50%; cursor: pointer; display: grid; place-items: center;
      color: #f5f5f5; background: rgba(8, 8, 8, 0.86); box-shadow: 0 0 0 1px rgba(255, 255, 255, 0.14), 0 12px 32px rgba(0, 0, 0, 0.6);
      backdrop-filter: blur(14px); -webkit-backdrop-filter: blur(14px); transition: transform 0.15s ease, background 0.15s ease, color 0.15s ease; }
    .btn:hover { transform: scale(1.06); background: rgba(22, 22, 22, 0.92); }
    .btn:active { transform: scale(0.97); }
    .btn:focus-visible { outline: 2px solid #6aa8ff; outline-offset: 3px; }
    .btn > svg.main { width: 22px; height: 22px; }
    .ring { position: absolute; inset: 0; width: 52px; height: 52px; transform: rotate(-90deg); opacity: 0; transition: opacity 0.2s; pointer-events: none; }
    .ring circle { fill: none; stroke-width: 3; }
    .ring .track { stroke: rgba(255, 255, 255, 0.1); }
    .ring .bar { stroke: #6aa8ff; stroke-linecap: round; transition: stroke-dashoffset 0.5s ease; }
    .btn.working .ring { opacity: 1; }
    .btn.working.spin .ring { animation: spin 1s linear infinite; }
    .btn.working > svg.main { opacity: 0.55; }
    .btn.done { color: #3fd68b; }
    .btn.error { color: #f5b544; }
    .col { position: relative; }
    .chip { position: absolute; top: calc(100% + 6px); left: 50%; transform: translateX(-50%); height: 22px; padding: 0 6px 0 9px; border: 0; border-radius: 999px;
      display: inline-flex; align-items: center; gap: 2px; cursor: pointer; white-space: nowrap; font: inherit; font-size: 11px; font-weight: 600; line-height: 1;
      font-variant-numeric: tabular-nums; color: #d6d6d6; background: rgba(8, 8, 8, 0.86); box-shadow: 0 0 0 1px rgba(255, 255, 255, 0.14), 0 8px 22px rgba(0, 0, 0, 0.5);
      backdrop-filter: blur(14px); -webkit-backdrop-filter: blur(14px); transition: background 0.15s ease, color 0.15s ease; }
    .chip:hover, .chip[aria-expanded="true"] { color: #fff; background: rgba(22, 22, 22, 0.92); }
    .chip:focus-visible { outline: 2px solid #6aa8ff; outline-offset: 2px; }
    .chip > svg { width: 12px; height: 12px; transition: transform 0.15s ease; }
    .chip[aria-expanded="true"] > svg { transform: rotate(180deg); }
    .menu { position: absolute; right: calc(100% + 12px); top: 50%; transform: translateY(-50%); min-width: 140px; padding: 6px; border-radius: 16px;
      background: rgba(8, 8, 8, 0.94); box-shadow: 0 0 0 1px rgba(255, 255, 255, 0.12), 0 16px 40px rgba(0, 0, 0, 0.6);
      backdrop-filter: blur(14px); -webkit-backdrop-filter: blur(14px); }
    .menu[hidden] { display: none; }
    .label { padding: 6px 10px; font-size: 11px; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; color: #8b8b8b; }
    .item { width: 100%; display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 8px 10px; border: 0; border-radius: 10px;
      background: transparent; color: #e8e8e8; font: inherit; text-align: left; cursor: pointer; }
    .item:hover, .item:focus-visible { background: rgba(255, 255, 255, 0.08); outline: none; }
    .item > svg { width: 15px; height: 15px; color: #6aa8ff; opacity: 0; }
    .item[aria-checked="true"] > svg { opacity: 1; }
    .wrap { opacity: 0; visibility: hidden; transform: translateX(14px); pointer-events: none; user-select: none; -webkit-user-select: none;
      transition: opacity 0.2s ease, transform 0.2s ease, visibility 0s linear 0.2s; }
    .wrap.on { opacity: 1; visibility: visible; transform: none; transition: opacity 0.2s ease, transform 0.2s ease; }
    .wrap.on .col { pointer-events: auto; }
    .toast { max-width: 280px; padding: 9px 14px; border-radius: 999px; color: #f2f2f2; background: rgba(8, 8, 8, 0.9);
      box-shadow: 0 0 0 1px rgba(255, 255, 255, 0.12), 0 10px 28px rgba(0, 0, 0, 0.55); opacity: 0; transform: translateX(8px);
      transition: opacity 0.2s ease, transform 0.2s ease; pointer-events: none; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .toast.on { opacity: 1; transform: none; }
    @keyframes spin { to { transform: rotate(270deg); } }
    @media (prefers-reduced-motion: reduce) { .btn, .ring, .ring .bar, .toast, .chip, .chip > svg, .wrap, .wrap.on { transition: none; } .btn.working.spin .ring { animation: none; } }
  `;

  const ui = (() => {
    let host = null;
    let root = null;
    let wrap = null;
    let btn = null;
    let chip = null;
    let chipText = null;
    let menu = null;
    let bar = null;
    let toastEl = null;
    let glyph = null;
    let toastTimer = 0;
    let hideTimer = 0;
    const glyphs = {};

    function build() {
      host = document.createElement("chromine-video-download");
      host.style.cssText =
        "all: initial; position: fixed; z-index: 2147483647; right: 20px; top: 50%; transform: translateY(-50%); display: block; pointer-events: none;";
      root = host.attachShadow({ mode: "closed" });
      const sheet = new CSSStyleSheet();
      sheet.replaceSync(CSS);
      root.adoptedStyleSheets = [sheet];

      wrap = document.createElement("div");
      wrap.className = "wrap";
      for (const t of ["pointerdown", "pointermove", "keydown", "focusin", "wheel"]) wrap.addEventListener(t, arm, { passive: true });
      toastEl = document.createElement("div");
      toastEl.className = "toast";
      toastEl.setAttribute("role", "status");
      toastEl.setAttribute("aria-live", "polite");
      btn = document.createElement("button");
      btn.type = "button";
      btn.className = "btn";
      btn.setAttribute("aria-label", "Download video");
      btn.title = "Download video";

      const ring = document.createElementNS(C.SVG, "svg");
      ring.setAttribute("class", "ring");
      ring.setAttribute("viewBox", "0 0 52 52");
      const track = C.circle(26, 26, 24);
      track.setAttribute("class", "track");
      bar = C.circle(26, 26, 24);
      bar.setAttribute("class", "bar");
      ring.append(track, bar);
      bar.style.strokeDasharray = `${RING}`;
      bar.style.strokeDashoffset = `${RING}`;

      for (const name of ["download", "done", "error"]) {
        glyphs[name] = C.ICONS[name]();
        glyphs[name].setAttribute("class", "main");
      }
      glyph = glyphs.download;
      btn.append(ring, glyph);

      chip = document.createElement("button");
      chip.type = "button";
      chip.className = "chip";
      chip.title = "Video quality";
      chip.setAttribute("aria-haspopup", "menu");
      chip.setAttribute("aria-expanded", "false");
      chipText = document.createElement("span");
      chip.append(chipText, C.ICONS.chevron());

      menu = document.createElement("div");
      menu.className = "menu";
      menu.hidden = true;
      menu.setAttribute("role", "menu");
      menu.setAttribute("aria-label", "Video quality");
      const label = document.createElement("div");
      label.className = "label";
      label.setAttribute("aria-hidden", "true");
      label.textContent = "Quality";
      menu.appendChild(label);
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
          closeMenu(true);
        });
        menu.appendChild(item);
      }

      const col = document.createElement("div");
      col.className = "col";
      col.append(btn, chip, menu);
      wrap.append(toastEl, col);
      root.appendChild(wrap);
      refresh();
      render(C.download.state, false);

      btn.addEventListener("click", (ev) => {
        if (!ev.isTrusted || C.download.busy) return;
        closeMenu(false);
        C.download.start(C.findVideo());
      });
      chip.addEventListener("click", (ev) => {
        if (!ev.isTrusted) return;
        if (menu.hidden) openMenu();
        else closeMenu(true);
      });
      menu.addEventListener("keydown", menuKeys);

      (document.body || document.documentElement).appendChild(host);
    }

    function items() {
      return [...menu.querySelectorAll(".item")];
    }

    function refresh() {
      if (!chip) return;
      const text = C.labelFor(C.quality);
      chipText.textContent = text;
      chip.setAttribute("aria-label", `Video quality: ${text}`);
      for (const item of items()) item.setAttribute("aria-checked", String(Number(item.dataset.value) === C.quality));
    }

    function openMenu() {
      toastEl.classList.remove("on");
      menu.hidden = false;
      chip.setAttribute("aria-expanded", "true");
      const list = items();
      (list.find((i) => i.getAttribute("aria-checked") === "true") || list[0]).focus();
      window.addEventListener("pointerdown", outside, true);
    }

    function closeMenu(focusChip) {
      if (!menu || menu.hidden) return;
      menu.hidden = true;
      chip.setAttribute("aria-expanded", "false");
      window.removeEventListener("pointerdown", outside, true);
      if (focusChip) chip.focus();
    }

    function outside(ev) {
      if (!ev.composedPath().includes(host)) closeMenu(false);
    }

    function menuKeys(ev) {
      const list = items();
      const at = list.indexOf(root.activeElement);
      if (ev.key === "ArrowDown" || ev.key === "ArrowUp") list[(at + (ev.key === "ArrowDown" ? 1 : -1) + list.length) % list.length].focus();
      else if (ev.key === "Home" || ev.key === "End") list[ev.key === "Home" ? 0 : list.length - 1].focus();
      else if (ev.key === "Escape" || ev.key === "Tab") closeMenu(true);
      else return;
      ev.preventDefault();
      ev.stopPropagation();
    }

    function setGlyph(name) {
      const next = glyphs[name];
      if (next !== glyph) {
        btn.replaceChild(next, glyph);
        glyph = next;
      }
    }

    function toast(text) {
      clearTimeout(toastTimer);
      toastEl.textContent = text;
      toastEl.classList.add("on");
      toastTimer = setTimeout(() => toastEl.classList.remove("on"), 3800);
    }

    function arm() {
      clearTimeout(hideTimer);
      if (!C.download.busy) hideTimer = setTimeout(hide, HIDE_AFTER);
    }

    function hide() {
      closeMenu(false);
      if (root.activeElement) root.activeElement.blur();
      wrap.classList.remove("on");
      toastEl.classList.remove("on");
      btn.classList.remove("done", "error");
      setGlyph("download");
    }

    function render(d, final) {
      if (!btn) return;
      if (d.ref) {
        clearTimeout(hideTimer);
        btn.classList.remove("done", "error");
        btn.classList.add("working");
        setGlyph("download");
        if (typeof d.progress === "number" && d.progress > 0) {
          btn.classList.remove("spin");
          bar.style.strokeDashoffset = `${RING * (1 - Math.min(d.progress, 100) / 100)}`;
          btn.setAttribute("aria-label", `Downloading video, ${Math.round(d.progress)}%`);
        } else {
          btn.classList.add("spin");
          bar.style.strokeDashoffset = `${RING * 0.72}`;
          btn.setAttribute("aria-label", "Downloading video");
        }
        return;
      }
      btn.classList.remove("working", "spin");
      bar.style.strokeDashoffset = `${RING}`;
      btn.setAttribute("aria-label", "Download video");
      if (!final || !wrap.classList.contains("on")) return;
      const kind = d.state === "done" ? "done" : "error";
      btn.classList.add(kind);
      setGlyph(kind);
      toast(C.MESSAGES[d.state]);
      arm();
    }

    C.onQuality(refresh);
    C.download.subscribe(render);

    return {
      owns(ev) {
        return !!host && ev.composedPath().includes(host);
      },
      reveal() {
        if (!host) build();
        else if (!host.isConnected) (document.body || document.documentElement).appendChild(host);
        void wrap.offsetWidth;
        wrap.classList.add("on");
        arm();
      },
    };
  })();

  let taps = 0;
  let lastTap = { t: 0, x: 0, y: 0 };
  function editable(ev) {
    const t = ev.composedPath()[0];
    return t instanceof HTMLElement && (t.isContentEditable || !!t.closest("input, textarea, select"));
  }

  function onTap(ev) {
    if (!ev.isTrusted || ev.button !== 0 || C.ours(ev) || editable(ev)) return;
    const near = ev.timeStamp - lastTap.t < TAP_GAP && Math.hypot(ev.clientX - lastTap.x, ev.clientY - lastTap.y) < TAP_RANGE;
    taps = near ? taps + 1 : 1;
    lastTap = { t: ev.timeStamp, x: ev.clientX, y: ev.clientY };
    if (taps < 3) return;
    taps = 0;
    if (C.TOP) ui.reveal();
    else C.send({ type: "reveal" });
  }
  window.addEventListener("pointerdown", onTap, true);
  C.onShow(() => ui.reveal());
})();
