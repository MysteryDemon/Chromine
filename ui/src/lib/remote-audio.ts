"use client";

import { useCallback, useEffect, useRef } from "react";

const RATE = 48000;
const FRAME = 4;
const LEAD = 0.12;
const MAX_LEAD = 0.5;
const UNLOCK_EVENTS = ["pointerdown", "keydown", "touchend"] as const;

class RemoteAudio {
  private ctx = new AudioContext({ latencyHint: "interactive" });
  private ws: WebSocket | null = null;
  private next = 0;
  private carry = new Uint8Array(0);
  private retry = 0;
  private delay = 1000;
  private stopped = false;

  constructor() {
    this.connect();
  }

  unlock = () => {
    if (this.ctx.state === "suspended") void this.ctx.resume().catch(() => {});
  };

  stop() {
    this.stopped = true;
    window.clearTimeout(this.retry);
    this.ws?.close();
    this.ws = null;
    void this.ctx.close().catch(() => {});
  }

  private connect() {
    const scheme = location.protocol === "https:" ? "wss:" : "ws:";
    const ws = new WebSocket(`${scheme}//${location.host}/browser/audio`, "binary");
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      this.delay = 1000;
    };
    ws.onmessage = (e) => {
      if (e.data instanceof ArrayBuffer) this.play(new Uint8Array(e.data));
    };
    ws.onclose = () => {
      if (this.ws !== ws || this.stopped) return;
      this.ws = null;
      this.carry = new Uint8Array(0);
      this.retry = window.setTimeout(() => this.connect(), this.delay);
      this.delay = Math.min(this.delay * 2, 30000);
    };
    this.ws = ws;
  }

  private play(chunk: Uint8Array) {
    let bytes = chunk;
    if (this.carry.length) {
      bytes = new Uint8Array(this.carry.length + chunk.length);
      bytes.set(this.carry);
      bytes.set(chunk, this.carry.length);
    }
    const usable = bytes.length - (bytes.length % FRAME);
    this.carry = bytes.slice(usable);
    if (!usable || this.ctx.state !== "running") return;

    const frames = usable / FRAME;
    const pcm = new DataView(bytes.buffer, bytes.byteOffset, usable);
    const buffer = this.ctx.createBuffer(2, frames, RATE);
    const left = buffer.getChannelData(0);
    const right = buffer.getChannelData(1);
    for (let i = 0; i < frames; i++) {
      left[i] = pcm.getInt16(i * FRAME, true) / 32768;
      right[i] = pcm.getInt16(i * FRAME + 2, true) / 32768;
    }

    const now = this.ctx.currentTime;
    if (this.next < now + 0.02) this.next = now + LEAD;
    else if (this.next > now + MAX_LEAD) return;
    const source = this.ctx.createBufferSource();
    source.buffer = buffer;
    source.connect(this.ctx.destination);
    source.start(this.next);
    this.next += buffer.duration;
  }
}

export function useRemoteAudio(on: boolean) {
  const player = useRef<RemoteAudio | null>(null);

  useEffect(() => {
    if (!on) return;
    const audio = new RemoteAudio();
    player.current = audio;
    for (const t of UNLOCK_EVENTS) window.addEventListener(t, audio.unlock, true);
    audio.unlock();
    return () => {
      for (const t of UNLOCK_EVENTS) window.removeEventListener(t, audio.unlock, true);
      audio.stop();
      player.current = null;
    };
  }, [on]);

  return useCallback((frame: HTMLIFrameElement) => {
    try {
      const win = frame.contentWindow;
      if (!win) return;
      for (const t of UNLOCK_EVENTS) win.addEventListener(t, () => player.current?.unlock(), true);
    } catch {}
  }, []);
}
