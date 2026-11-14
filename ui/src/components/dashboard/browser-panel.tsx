"use client";

import { memo, useEffect, useRef, useState } from "react";

import type { DisplayView, StreamMode } from "@/lib/api";
import { useRemoteAudio } from "@/lib/remote-audio";
import { attachTouchScroll } from "@/lib/touch-scroll";

export const WEBRTC_URL = "/browser/webrtc.html";

export function vncUrl(d: DisplayView) {
  return `/browser/vnc.html?autoconnect=true&reconnect=true&reconnect_delay=2000&resize=remote&quality=${d.vnc_quality}&compression=${d.vnc_compression}&path=websockify`;
}

const MIN_REMOTE_WIDTH = 500;
const UI_ZOOM_ID = "chromine-ui-zoom";
const UI_ZOOM_VAR = "--chromine-ui-zoom";

function zoomNoVncUi(frame: HTMLIFrameElement | null, zoom: number) {
  try {
    const doc = frame?.contentDocument;
    if (!doc?.head) return;
    if (!doc.getElementById(UI_ZOOM_ID)) {
      const style = doc.createElement("style");
      style.id = UI_ZOOM_ID;
      style.textContent = `body > :not(#noVNC_container, #screen) { zoom: var(${UI_ZOOM_VAR}, 1); }`;
      doc.head.append(style);
    }
    doc.documentElement.style.setProperty(UI_ZOOM_VAR, String(zoom));
  } catch {}
}

export const BrowserPanel = memo(function BrowserPanel({
  active,
  reloadKey,
  sound,
  display,
  mode,
}: {
  active: boolean;
  reloadKey: number;
  sound: boolean;
  display: DisplayView;
  mode: StreamMode;
}) {
  const scale = display.display_scale;
  const src = mode === "webrtc" ? WEBRTC_URL : vncUrl(display);
  const [loaded, setLoaded] = useState(active);
  if (active && !loaded) setLoaded(true);
  const webrtc = mode === "webrtc";
  const watchFrame = useRemoteAudio(loaded && sound && !webrtc);
  const box = useRef<HTMLDivElement>(null);
  const frame = useRef<HTMLIFrameElement>(null);
  const [fit, setFit] = useState(1);
  const shrink = fit / scale;

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const observer = new ResizeObserver(([entry]) => {
      const width = entry.contentRect.width;
      setFit(width > 0 ? Math.min(1, width / MIN_REMOTE_WIDTH) : 1);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    zoomNoVncUi(frame.current, 1 / shrink);
  }, [shrink]);

  useEffect(() => {
    if (webrtc) frame.current?.contentWindow?.postMessage({ sound }, window.location.origin);
  }, [webrtc, sound]);

  return (
    <div className="h-full p-1.5 sm:p-3">
      <div
        ref={box}
        className="relative h-full overflow-hidden rounded-xl bg-[#050505] shadow-[0_24px_80px_-24px_rgba(0,0,0,1)] ring-1 ring-white/[0.08] sm:rounded-2xl"
      >
        {loaded ? (
          <iframe
            ref={frame}
            key={`${reloadKey}|${src}|${scale}`}
            title="Remote browser"
            src={src}
            className="absolute inset-0 size-full origin-top-left border-0"
            style={
              shrink !== 1
                ? { width: `${100 / shrink}%`, height: `${100 / shrink}%`, transform: `scale(${shrink})` }
                : undefined
            }
            allow="clipboard-read; clipboard-write"
            onLoad={(e) => {
              watchFrame(e.currentTarget);
              attachTouchScroll(e.currentTarget, scale);
              zoomNoVncUi(e.currentTarget, 1 / shrink);
              if (webrtc) e.currentTarget.contentWindow?.postMessage({ sound }, window.location.origin);
            }}
          />
        ) : null}
      </div>
    </div>
  );
});
