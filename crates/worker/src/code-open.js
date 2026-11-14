(() => {
  const here = location.origin;
  const toRemote = (raw) => {
    let url;
    try {
      url = new URL(String(raw), location.href);
    } catch {
      return null;
    }
    if (url.protocol !== "http:" && url.protocol !== "https:") return null;
    if (url.origin !== here) {
      let query = url.search + url.hash;
      try {
        query = decodeURIComponent(query);
      } catch {}
      return query.includes(here) ? null : url.href;
    }
    const proxied = url.pathname.match(/^\/proxy\/(\d+)(\/.*)?$/);
    return proxied ? `http://localhost:${proxied[1]}${proxied[2] || "/"}${url.search}${url.hash}` : null;
  };
  const send = (url) => {
    fetch("/_chromine/open", { method: "POST", body: url, credentials: "same-origin", keepalive: true }).catch(() => {});
  };
  const nativeOpen = window.open.bind(window);
  window.open = function (url, target, features) {
    if (arguments.length === 0 || url === undefined) {
      let real = null;
      const loc = {};
      Object.defineProperty(loc, "href", {
        get: () => "about:blank",
        set: (value) => {
          const remote = toRemote(value);
          if (remote) send(remote);
          else real = nativeOpen(value, target, features);
        },
      });
      return { opener: null, closed: false, location: loc, close: () => real && real.close(), focus: () => {} };
    }
    const remote = toRemote(url);
    if (!remote) return nativeOpen(url, target, features);
    send(remote);
    return null;
  };
  document.addEventListener(
    "click",
    (e) => {
      const a = e.target instanceof Element ? e.target.closest("a[href]") : null;
      if (!a || e.defaultPrevented || a.target !== "_blank") return;
      const remote = toRemote(a.href);
      if (!remote) return;
      e.preventDefault();
      e.stopPropagation();
      send(remote);
    },
    true,
  );
})();
