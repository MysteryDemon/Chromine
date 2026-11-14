type Gesture = CustomEvent<{ type: string; clientX: number; clientY: number }>;
type Sample = { t: number; x: number; y: number };

const MIN_SPEED = 0.02;
const MAX_SPEED = 8;
const GLIDE_MS = 400;
const FLING_MS = 80;
const FLING_STEPS = 3;
const BOOST_MS = 600;
const MAX_BOOST = 8;
const FLING_WINDOW = 80;
const VELOCITY_WINDOW = 100;
const STEP_RATIO = 50 / 120;
const WHEEL_STEP = 50;

export function attachTouchScroll(frame: HTMLIFrameElement, scale: number) {
  let view: (Window & typeof globalThis) | null;
  try {
    view = frame.contentWindow as (Window & typeof globalThis) | null;
    if (!view?.document) return;
  } catch {
    return;
  }
  const win = view;

  let canvas: EventTarget | null = null;
  let originX = 0;
  let originY = 0;
  let axis: "x" | "y" | null = null;
  let carry = 0;
  let samples: Sample[] = [];
  let raf = 0;
  let boost = 1;
  let lastFling = { at: -Infinity, dir: 0 };

  const along = (x: number, y: number) => (axis === "x" ? x : y);

  const move = (distance: number) => {
    carry += (-distance * STEP_RATIO) / scale;
    while (Math.abs(carry) >= WHEEL_STEP) {
      const step = Math.sign(carry) * WHEEL_STEP;
      carry -= step;
      canvas?.dispatchEvent(
        new win.WheelEvent("wheel", {
          deltaX: axis === "x" ? step : 0,
          deltaY: axis === "x" ? 0 : step,
          clientX: originX,
          clientY: originY,
          bubbles: true,
          cancelable: true,
        }),
      );
    }
  };

  const stopFling = () => {
    win.cancelAnimationFrame(raf);
    raf = 0;
  };

  const fling = (distance: number) => {
    const start = win.performance.now();
    let done = 0;
    const frame = (now: number) => {
      const t = Math.min(1, Math.max(0, now - start) / FLING_MS);
      const target = distance * (1 - (1 - t) ** 3);
      move(target - done);
      done = target;
      raf = t < 1 ? win.requestAnimationFrame(frame) : 0;
    };
    raf = win.requestAnimationFrame(frame);
  };

  const releaseVelocity = () => {
    const last = samples[samples.length - 1];
    const first = samples.find((s) => last.t - s.t <= VELOCITY_WINDOW) ?? last;
    const dt = last.t - first.t;
    const v = dt > 0 ? along(last.x - first.x, last.y - first.y) / dt : 0;
    return Math.max(-MAX_SPEED, Math.min(MAX_SPEED, v));
  };

  const onGesture = (e: Event) => {
    const { type, clientX, clientY } = (e as Gesture).detail;
    if (type !== "drag") return;
    e.stopImmediatePropagation();
    if (e.type === "gesturestart") {
      stopFling();
      canvas = e.target;
      originX = clientX;
      originY = clientY;
      samples = [];
      return;
    }
    if (e.type === "gesturemove") {
      const prev = samples[samples.length - 1] ?? { t: e.timeStamp, x: originX, y: originY };
      if (!samples.length) {
        const next = Math.abs(clientX - originX) > Math.abs(clientY - originY) * 1.2 ? "x" : "y";
        if (next !== axis) carry = 0;
        axis = next;
      }
      samples.push({ t: e.timeStamp, x: clientX, y: clientY });
      if (samples.length > 32) samples.shift();
      move(along(clientX - prev.x, clientY - prev.y));
      return;
    }
    const last = samples[samples.length - 1];
    if (last && e.timeStamp - last.t < FLING_WINDOW) {
      const speed = releaseVelocity();
      if (Math.abs(speed) <= MIN_SPEED) return;
      const dir = Math.sign(speed);
      const chained = dir === lastFling.dir && e.timeStamp - lastFling.at < BOOST_MS;
      boost = chained ? Math.min(boost * 2, MAX_BOOST) : 1;
      lastFling = { at: e.timeStamp, dir };
      const limit = (FLING_STEPS * boost * WHEEL_STEP * scale) / STEP_RATIO;
      fling(dir * Math.min(Math.abs(speed) * GLIDE_MS, limit));
    }
  };

  win.addEventListener("touchstart", stopFling, true);
  for (const t of ["gesturestart", "gesturemove", "gestureend"]) win.addEventListener(t, onGesture, true);
}
