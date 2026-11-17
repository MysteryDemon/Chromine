"use client";

import { useSyncExternalStore } from "react";
import {
  ArrowDownToLine,
  ArrowDownUp,
  CodeXml,
  ExternalLink,
  Globe,
  LoaderCircle,
  Maximize,
  Minimize,
  RefreshCw,
  Settings,
  Volume2,
  VolumeX,
} from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import type { DashboardState } from "@/lib/api";
import { formatBytes, percent } from "@/lib/format";
import { cn } from "@/lib/utils";
import { Pill } from "./state-badge";

const TRIGGER =
  "group/trigger h-7 flex-none gap-1.5 rounded-full border-0 px-3.5 text-[13px] font-medium text-white/55 transition-colors hover:text-white data-[state=active]:bg-white data-[state=active]:text-black data-[state=active]:shadow-[0_1px_3px_rgba(0,0,0,0.5)] dark:text-white/55 dark:hover:text-white dark:data-[state=active]:border-transparent dark:data-[state=active]:bg-white dark:data-[state=active]:text-black [&_svg:not([class*='size-'])]:size-3.5";

function StatusChips({ state, starting }: { state: DashboardState | null; starting: boolean }) {
  if (!state) return null;
  const b = state.browser;
  const offline = !starting && (b.stale || !b.status);
  const stopped = !starting && !offline && !b.status?.chrome_running;
  const downloading = b.stale ? [] : (b.status?.active_downloads ?? []);
  const received = downloading.reduce((n, dl) => n + dl.received_bytes, 0);
  const total = downloading.reduce((n, dl) => n + dl.total_bytes, 0);
  if (!offline && !stopped && !starting && downloading.length === 0) return null;
  return (
    <div
      className={cn(
        "order-last min-w-0 basis-full md:order-none md:flex-1 md:basis-auto",
        !offline && !stopped && "max-md:hidden",
      )}
    >
      <div className="-mx-3 flex items-center gap-1.5 overflow-x-auto px-3 [scrollbar-width:none] md:mx-0 md:px-0">
        {starting ? (
          <span className="max-md:hidden">
            <Pill tone="info" icon={LoaderCircle} spin>
              Starting browser
            </Pill>
          </span>
        ) : null}
        {offline || stopped ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <span>
                <Pill tone="bad" icon={Globe}>
                  {offline ? "Browser offline" : "Chrome stopped"}
                </Pill>
              </span>
            </TooltipTrigger>
            <TooltipContent>Remote browser status</TooltipContent>
          </Tooltip>
        ) : null}
        {downloading.length > 0 ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <span className="max-md:hidden">
                <Pill tone="info" icon={ArrowDownToLine}>
                  {downloading.length === 1 ? "1 downloading" : `${downloading.length} downloading`}
                  {total > 0 ? (
                    <span className="relative ml-0.5 h-1 w-10 overflow-hidden rounded-full bg-info/20">
                      <span className="absolute inset-y-0 left-0 rounded-full bg-info" style={{ width: `${percent(received, total)}%` }} />
                    </span>
                  ) : null}
                </Pill>
              </span>
            </TooltipTrigger>
            <TooltipContent className="grid gap-1">
              {downloading.map((dl) => (
                <span key={dl.guid} className="flex max-w-80 gap-3">
                  <span className="truncate">{dl.filename || "download"}</span>
                  <span className="ml-auto shrink-0 tabular-nums opacity-70">
                    {dl.total_bytes > 0
                      ? `${percent(dl.received_bytes, dl.total_bytes)}% of ${formatBytes(dl.total_bytes)}`
                      : formatBytes(dl.received_bytes)}
                  </span>
                </span>
              ))}
            </TooltipContent>
          </Tooltip>
        ) : null}
      </div>
    </div>
  );
}

const LABEL = "hidden sm:inline group-data-[state=active]/trigger:inline";
const NARROW_LABEL = "hidden sm:inline min-[25rem]:group-data-[state=active]/trigger:inline";

function subscribeFullscreen(onChange: () => void) {
  document.addEventListener("fullscreenchange", onChange);
  return () => document.removeEventListener("fullscreenchange", onChange);
}

function useFullscreen() {
  const supported = useSyncExternalStore(subscribeFullscreen, () => document.fullscreenEnabled === true, () => false);
  const active = useSyncExternalStore(subscribeFullscreen, () => !!document.fullscreenElement, () => false);
  function toggle() {
    const change = document.fullscreenElement ? document.exitFullscreen() : document.documentElement.requestFullscreen();
    change.catch(() => toast.error("The browser blocked full screen"));
  }
  return { supported, active, toggle };
}

export function TopBar({
  state,
  starting,
  activeCount,
  codeSrc,
  showCodeLink,
  showReconnect,
  onReconnect,
  sound,
  onSound,
}: {
  state: DashboardState | null;
  starting: boolean;
  activeCount: number;
  codeSrc: string | null;
  showCodeLink: boolean;
  showReconnect: boolean;
  onReconnect: () => void;
  sound: boolean;
  onSound: () => void;
}) {
  const fullscreen = useFullscreen();
  const label = codeSrc && fullscreen.supported ? NARROW_LABEL : LABEL;
  return (
    <header className="sticky top-0 z-30 border-b border-white/[0.06] bg-black">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2.5 px-3 py-2.5 sm:px-5 md:h-14 md:flex-nowrap md:py-0">
        <TabsList className="h-9 shrink-0 gap-0.5 rounded-full bg-white/[0.04] p-1 ring-1 ring-white/[0.07]">
          <TabsTrigger value="browser" className={TRIGGER} aria-label="Browser">
            <Globe aria-hidden />
            <span className={label}>Browser</span>
          </TabsTrigger>
          {codeSrc ? (
            <TabsTrigger value="code" className={TRIGGER} aria-label="VS Code">
              <CodeXml aria-hidden />
              <span className={label}>VS Code</span>
            </TabsTrigger>
          ) : null}
          <TabsTrigger value="transfers" className={TRIGGER} aria-label="Transfers">
            <ArrowDownUp aria-hidden />
            <span className={label}>Transfers</span>
            {activeCount > 0 ? (
              <span className="ml-0.5 inline-flex h-[18px] min-w-[18px] items-center justify-center rounded-full bg-info/15 px-1 text-[10px] font-semibold text-info tabular-nums group-data-[state=active]/trigger:bg-black group-data-[state=active]/trigger:text-white">
                {activeCount}
              </span>
            ) : null}
          </TabsTrigger>
          <TabsTrigger value="settings" className={TRIGGER} aria-label="Settings">
            <Settings aria-hidden />
            <span className={label}>Settings</span>
          </TabsTrigger>
        </TabsList>

        <StatusChips state={state} starting={starting} />

        <div className={cn("ml-auto flex items-center gap-2", !showReconnect && !showCodeLink && !fullscreen.supported && "hidden md:flex")}>
          {showCodeLink && codeSrc ? (
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  asChild
                  variant="ghost"
                  size="sm"
                  className="h-9 rounded-full bg-white/[0.04] px-3 text-[13px] text-white/75 ring-1 ring-white/[0.08] hover:bg-white/[0.09] hover:text-white dark:hover:bg-white/[0.09]"
                >
                  <a href={codeSrc} target="_blank" rel="noopener noreferrer">
                    <ExternalLink aria-hidden />
                    <span className="hidden sm:inline">Open in new tab</span>
                  </a>
                </Button>
              </TooltipTrigger>
              <TooltipContent>Open VS Code in its own browser tab</TooltipContent>
            </Tooltip>
          ) : null}
          {showReconnect ? (
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={onSound}
                  aria-label={sound ? "Mute" : "Unmute"}
                  aria-pressed={!sound}
                  className={cn(
                    "size-9 rounded-full bg-white/[0.04] ring-1 ring-white/[0.08] hover:bg-white/[0.09] hover:text-white dark:hover:bg-white/[0.09]",
                    sound ? "text-white/75" : "text-white/40",
                  )}
                >
                  {sound ? <Volume2 aria-hidden /> : <VolumeX aria-hidden />}
                </Button>
              </TooltipTrigger>
              <TooltipContent>{sound ? "Mute" : "Unmute"}</TooltipContent>
            </Tooltip>
          ) : null}
          {showReconnect ? (
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={onReconnect}
                  aria-label="Reconnect"
                  className="h-9 rounded-full bg-white/[0.04] px-3 text-[13px] text-white/75 ring-1 ring-white/[0.08] hover:bg-white/[0.09] hover:text-white dark:hover:bg-white/[0.09]"
                >
                  <RefreshCw aria-hidden />
                  <span className="hidden sm:inline">Reconnect</span>
                </Button>
              </TooltipTrigger>
              <TooltipContent>Reload the remote browser display</TooltipContent>
            </Tooltip>
          ) : null}
          {fullscreen.supported ? (
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={fullscreen.toggle}
                  aria-label={fullscreen.active ? "Exit full screen" : "Full screen"}
                  className="size-9 rounded-full bg-white/[0.04] text-white/75 ring-1 ring-white/[0.08] hover:bg-white/[0.09] hover:text-white dark:hover:bg-white/[0.09]"
                >
                  {fullscreen.active ? <Minimize aria-hidden /> : <Maximize aria-hidden />}
                </Button>
              </TooltipTrigger>
              <TooltipContent>{fullscreen.active ? "Exit full screen" : "Full screen"}</TooltipContent>
            </Tooltip>
          ) : null}
        </div>
      </div>
    </header>
  );
}
