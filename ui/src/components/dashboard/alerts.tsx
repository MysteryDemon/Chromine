import { HardDriveDownload, PlugZap, ServerCrash, ShieldAlert, TriangleAlert, WifiOff } from "lucide-react";

import type { DashboardState } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { cn } from "@/lib/utils";

type Item = { key: string; tone: "bad" | "warn"; icon: React.ElementType; title: string; text?: string };

export function collectAlerts(st: DashboardState): Item[] {
  const out: Item[] = [];
  const d = st.disk;
  if (d.low) {
    out.push({
      key: "disk",
      tone: "bad",
      icon: HardDriveDownload,
      title: "Low disk space: new downloads are refused",
      text: `${formatBytes(d.downloads.available_bytes)} free, minimum is ${formatBytes(d.min_free_bytes)}. Free space on the Docker host or lower MIN_FREE_DISK_BYTES. Failed and unprocessed files are never deleted automatically.`,
    });
  }
  if (st.https?.error) {
    out.push({ key: "https", tone: "bad", icon: ShieldAlert, title: "HTTPS certificate not issued", text: st.https.error });
  }
  const b = st.browser;
  if (b.stale || !b.status) {
    out.push({
      key: "agent",
      tone: "bad",
      icon: ServerCrash,
      title: "The browser service is not reporting",
      text: "Check `docker compose ps` and `docker compose logs browser`.",
    });
  } else {
    const s = b.status;
    if (s.sandbox_ok === false) {
      out.push({ key: "sandbox", tone: "bad", icon: ShieldAlert, title: "Chrome sandbox unavailable", text: s.sandbox_detail ?? undefined });
    }
    if (s.chrome_running && s.monitor_detail) {
      out.push({ key: "monitor", tone: "bad", icon: PlugZap, title: "Downloads are not being detected", text: s.monitor_detail });
    }
    if (!s.chrome_running && s.last_error) {
      out.push({ key: "chrome", tone: "bad", icon: ServerCrash, title: "Chrome is not running", text: s.last_error });
    }
    if (s.downloads_paused_low_disk && !d.low) {
      out.push({ key: "paused", tone: "warn", icon: HardDriveDownload, title: "Downloads paused for low disk space" });
    }
    s.recent_rejections
      .filter((r) => st.now - r.at_ms < 3600_000)
      .slice(-3)
      .forEach((r, i) =>
        out.push({ key: `rej-${i}-${r.at_ms}`, tone: "warn", icon: TriangleAlert, title: `Download not processed: ${r.filename || "unnamed file"}`, text: r.reason }),
      );
  }
  return out;
}

export function AlertList({ state, connectionError }: { state: DashboardState | null; connectionError: string | null }) {
  const items: Item[] = [];
  if (connectionError) {
    items.push({ key: "conn", tone: "bad", icon: WifiOff, title: "Cannot reach the server", text: connectionError });
  }
  if (state) items.push(...collectAlerts(state));
  if (!items.length) return null;
  return (
    <div className="grid gap-2 px-3 pt-3 sm:px-5">
      {items.map(({ key, tone, icon: Icon, title, text }) => (
        <div
          key={key}
          role="alert"
          className={cn(
            "flex items-start gap-3 rounded-xl px-3.5 py-3 ring-1 ring-inset",
            tone === "bad" ? "bg-destructive/[0.06] ring-destructive/20" : "bg-warning/[0.05] ring-warning/20",
          )}
        >
          <span
            className={cn(
              "grid size-8 shrink-0 place-items-center rounded-lg",
              tone === "bad" ? "bg-destructive/15 text-destructive" : "bg-warning/15 text-warning",
            )}
          >
            <Icon className="size-4" aria-hidden />
          </span>
          <div className="min-w-0 pt-[3px]">
            <p className="text-sm leading-snug font-medium text-white">{title}</p>
            {text ? <p className="mt-0.5 text-[13px] leading-relaxed break-words text-white/55">{text}</p> : null}
          </div>
        </div>
      ))}
    </div>
  );
}
