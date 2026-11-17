"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { LoaderCircle, LogOut } from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

import { Tabs, TabsContent } from "@/components/ui/tabs";
import {
  getSession,
  getState,
  logout,
  type CodeView,
  type DashboardState,
  type DeviceMode,
  type DisplayView,
} from "@/lib/api";
import { AlertList, browserStarting } from "./alerts";
import { BrowserPanel } from "./browser-panel";
import { CodePanel, codeUrl } from "./code-panel";
import { SettingsPanel } from "./settings-panel";
import { TopBar } from "./top-bar";
import { TransfersPanel } from "./transfers-panel";

type Tab = "browser" | "transfers" | "code" | "settings";

function asTab(t: string | null): Tab {
  return t === "transfers" || t === "code" || t === "settings" ? t : "browser";
}

function readTab(): Tab {
  try {
    return asTab(localStorage.getItem("chromine-tab"));
  } catch {
    return "browser";
  }
}

function readDeviceMode(): DeviceMode {
  try {
    const m = localStorage.getItem("chromine-stream-mode");
    return m === "vnc" || m === "webrtc" ? m : "default";
  } catch {
    return "default";
  }
}

function sameDisplay(a: DisplayView | null, b: DisplayView) {
  return (
    !!a &&
    a.stream_mode === b.stream_mode &&
    a.display_scale === b.display_scale &&
    a.vnc_quality === b.vnc_quality &&
    a.vnc_compression === b.vnc_compression
  );
}

function readSound(): boolean {
  try {
    return localStorage.getItem("chromine-sound") !== "off";
  } catch {
    return true;
  }
}

export function Dashboard() {
  const [user, setUser] = useState<string | null>(null);
  const [admin, setAdmin] = useState(false);
  const [display, setDisplay] = useState<DisplayView | null>(null);
  const [deviceMode, setDeviceMode] = useState<DeviceMode>("default");
  const [state, setState] = useState<DashboardState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("browser");
  const [code, setCode] = useState<CodeView | null>(null);
  const [codeOpened, setCodeOpened] = useState(false);
  const [vncKey, setVncKey] = useState(0);
  const [sound, setSound] = useState(false);
  const [openedAt] = useState(() => Date.now());
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const page = useRef(1);
  const lastLink = useRef<number | null>(null);
  const changeTabRef = useRef<(v: string) => void>(() => {});

  const refresh = useCallback(async () => {
    const requested = page.current;
    try {
      const next = await getState(requested);
      if (requested !== page.current) return;
      page.current = next.page;
      setState(next);
      const link = next.browser.stale ? null : next.browser.status?.opened_link;
      if (link && lastLink.current !== null && link.at_ms > lastLink.current) {
        toast.success("Opened in the remote browser", {
          description: link.url,
          action: { label: "View", onClick: () => changeTabRef.current("browser") },
        });
      }
      if (link) lastLink.current = Math.max(lastLink.current ?? 0, link.at_ms);
      else if (lastLink.current === null) lastLink.current = 0;
      setDisplay((d) => (sameDisplay(d, next.display) ? d : next.display));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const goToPage = useCallback(
    (p: number) => {
      page.current = p;
      return refresh();
    },
    [refresh],
  );

  const schedule = useCallback(() => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(
      async () => {
        await refresh();
        schedule();
      },
      document.hidden ? 10_000 : 2_000,
    );
  }, [refresh]);

  useEffect(() => {
    const t = readTab();
    setTab(t);
    if (t === "code") setCodeOpened(true);
    setSound(readSound());
    setDeviceMode(readDeviceMode());
    let cancelled = false;
    getSession()
      .then((s) => {
        if (cancelled) return;
        setDisplay(s.display);
        setCode(s.code);
        setUser(s.user);
        setAdmin(s.admin);
        return refresh().then(schedule);
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
    const onVisible = () => {
      if (!document.hidden) refresh().then(schedule);
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      cancelled = true;
      document.removeEventListener("visibilitychange", onVisible);
      if (timer.current) clearTimeout(timer.current);
    };
  }, [refresh, schedule]);

  const changeTab = (v: string) => {
    const t = asTab(v);
    setTab(t);
    if (t === "code") setCodeOpened(true);
    try {
      localStorage.setItem("chromine-tab", t);
    } catch {
    }
  };

  const toggleSound = () => {
    const next = !sound;
    setSound(next);
    try {
      localStorage.setItem("chromine-sound", next ? "on" : "off");
    } catch {
    }
  };

  const changeDeviceMode = useCallback((m: DeviceMode) => {
    setDeviceMode(m);
    try {
      if (m === "default") localStorage.removeItem("chromine-stream-mode");
      else localStorage.setItem("chromine-stream-mode", m);
    } catch {
    }
  }, []);

  const updateDisplay = useCallback((d: DisplayView) => setDisplay((cur) => (sameDisplay(cur, d) ? cur : d)), []);

  const signOut = async () => {
    try {
      await logout();
    } finally {
      window.location.assign("/login");
    }
  };

  if (!user || !display) {
    return (
      <div className="flex min-h-dvh items-center justify-center bg-black text-white/40">
        {error ? <p className="text-destructive">{error}</p> : <LoaderCircle className="size-5 animate-spin" aria-label="Loading" />}
      </div>
    );
  }

  changeTabRef.current = changeTab;
  const activeCount = state ? state.counts.active : 0;
  const codeSrc = code ? codeUrl(code) : null;
  const shownTab: Tab = tab === "code" && !code ? "browser" : tab;
  const mode = deviceMode === "default" ? display.stream_mode : deviceMode;
  const starting = state ? browserStarting(state, openedAt) : false;

  return (
    <Tabs value={shownTab} onValueChange={changeTab} className="flex h-dvh flex-col gap-0 bg-black">
      <TopBar
        state={state}
        starting={starting}
        activeCount={activeCount}
        codeSrc={codeSrc}
        showCodeLink={shownTab === "code"}
        showReconnect={shownTab === "browser"}
        onReconnect={() => setVncKey((k) => k + 1)}
        sound={sound}
        onSound={toggleSound}
      />
      <AlertList state={state} starting={starting} connectionError={error} />
      <main className="min-h-0 flex-1">
        <TabsContent value="browser" forceMount className="h-full data-[state=inactive]:hidden">
          <BrowserPanel active={shownTab === "browser"} reloadKey={vncKey} sound={sound} display={display} mode={mode} />
        </TabsContent>
        {codeSrc && codeOpened ? (
          <TabsContent value="code" forceMount className="h-full data-[state=inactive]:hidden">
            <CodePanel src={codeSrc} />
          </TabsContent>
        ) : null}
        <TabsContent value="transfers" className="h-full overflow-y-auto overscroll-y-contain">
          <TransfersPanel state={state} onChanged={refresh} onPage={goToPage} />
        </TabsContent>
        <TabsContent value="settings" forceMount className="h-full overflow-y-auto overscroll-y-contain data-[state=inactive]:hidden">
          <SettingsPanel
            admin={admin}
            active={shownTab === "settings"}
            onDisplay={updateDisplay}
            deviceMode={deviceMode}
            onDeviceMode={changeDeviceMode}
          />
        </TabsContent>
      </main>

      {shownTab === "code" ? null : (
        <Tooltip>
          <TooltipTrigger asChild>
            <Button
              size="icon"
              variant="outline"
              aria-label="Sign out"
              onClick={signOut}
              className="fixed bottom-4 left-4 z-40 size-11 rounded-full border-0 bg-[#0d0d0d] text-white/70 shadow-[0_10px_40px_rgba(0,0,0,0.9)] ring-1 ring-white/10 hover:bg-[#1f1f1f] hover:text-white dark:bg-[#0d0d0d] dark:hover:bg-[#1f1f1f]"
            >
              <LogOut className="size-[18px]" />
            </Button>
          </TooltipTrigger>
          <TooltipContent side="right">Sign out ({user})</TooltipContent>
        </Tooltip>
      )}
    </Tabs>
  );
}
