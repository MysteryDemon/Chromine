const UNITS = ["B", "KiB", "MiB", "GiB", "TiB"];

export function formatBytes(n: number | null | undefined): string {
  if (n === null || n === undefined) return "–";
  let v = n;
  let i = 0;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${v.toFixed(0)} B` : `${v.toFixed(v >= 100 ? 0 : 1)} ${UNITS[i]}`;
}

export function formatTime(ms: number | null | undefined): string {
  if (!ms) return "";
  return new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

export function formatUntil(ms: number | null | undefined, now: number): string {
  if (!ms) return "";
  const s = Math.max(0, Math.round((ms - now) / 1000));
  if (s < 60) return `in ${s} s`;
  if (s < 3600) return `in ${Math.round(s / 60)} min`;
  return `at ${formatTime(ms)}`;
}

export function formatAgo(ms: number | null | undefined, now: number): string {
  if (!ms) return "";
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return formatTime(ms);
}

export function percent(part: number, total: number): number {
  if (total <= 0) return 100;
  return Math.min(100, Math.max(0, Math.round((100 * part) / total)));
}
